//! `cadmus eval`: runs eval set v1 (ADR-0005 §7). Every case in the set runs
//! against a scratch copy of its fixture workspace, the folded trace is
//! scored (`cadmus_core::score_case`), the score events are appended to the
//! run's own trajectory log — carrying the `selfevol.eval_split` attribute
//! (ADR-0010 §4) — and the aggregate score file is written at the end.
//!
//! A failed case never aborts the set: its trace records the failure, its
//! scores grade the partial run, and the next case runs. The set's output is
//! the score file, not the exit code — phase-2 gates compare score files.
//!
//! Runs are sequential: live providers are rate-limited, and paired/seeded
//! ordering protocols are the phase-2 gate runner's concern (ADR-0010 §1),
//! not v1's.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cadmus_contract::{
    CaseResult, ChatRequest, Clock, Command, EvalCase, EvalReport, Event, EventKind, EventSink,
    Expectation, IdSequence, Provider, attrs,
};
use cadmus_core::{AgentLoop, ClientProtocol, Telemetry, replay_trace, score_case};
use cadmus_memory::JsonlLog;
use cadmus_transport::{Blackhole, command_channel};

use crate::telemetry::{SeqIds, SystemClock, default_trace_root, mint_trace_id};
use crate::tools::coding_tools;
use crate::{Error, approval, provider};

/// Everything an eval run needs, resolved from CLI arguments.
pub struct EvalConfig {
    /// Directory of case JSON files.
    pub set: PathBuf,
    /// Fixture workspaces root; `case.fixture` names a directory under it.
    pub fixtures: PathBuf,
    pub max_tokens: u32,
    pub max_turns: usize,
    /// Trajectory root; `None` resolves the env/default chain at run time.
    pub trace_root: Option<PathBuf>,
    /// The score file to write.
    pub out: PathBuf,
}

/// Runs the full set and returns the aggregate report (also written to
/// `config.out`). The provider is injected — tests drive a replay fake —
/// while `provider_name`/`model_name` record the composite identity on the
/// report and the start-run attributes.
///
/// # Errors
/// Fails on an invalid corpus, a broken trajectory log, or an unwritable
/// score file. Individual case failures are *data* (zero scores), never
/// errors.
pub async fn run_eval(
    config: &EvalConfig,
    provider: Arc<dyn Provider>,
    provider_name: &str,
    model_name: &str,
) -> Result<EvalReport, Error> {
    let cases = load_cases(&config.set, &config.fixtures)?;
    let set_digest = corpus_digest(&cases, &config.fixtures)?;
    let root = match &config.trace_root {
        Some(root) => root.clone(),
        None => default_trace_root().ok_or(Error::TraceRoot)?,
    };
    let log = Arc::new(JsonlLog::new(root).map_err(Error::TraceLog)?);
    let clock = Arc::new(SystemClock);

    let mut results = Vec::with_capacity(cases.len());
    for case in &cases {
        let result = run_case(
            config,
            case,
            &provider,
            provider_name,
            model_name,
            &log,
            &clock,
        )
        .await?;
        tracing::info!(
            case_id = %case.id,
            trace_id = %result.trace_id,
            passed = result.passed,
            "case scored"
        );
        results.push(result);
    }

    let passed = results.iter().filter(|result| result.passed).count();
    let report = EvalReport {
        cadmus_version: env!("CARGO_PKG_VERSION").into(),
        provider: provider_name.into(),
        model: model_name.into(),
        set_digest,
        passed: u32::try_from(passed).unwrap_or(u32::MAX),
        total: u32::try_from(results.len()).unwrap_or(u32::MAX),
        results,
    };
    if let Some(parent) = config.out.parent() {
        fs::create_dir_all(parent).map_err(Error::EvalScoreFile)?;
    }
    let json = serde_json::to_string_pretty(&report)
        .expect("EvalReport is plain data; serialization cannot fail");
    // Temp-then-rename: a crash mid-write must not tear the score file that
    // a full paid run just produced.
    let tmp = config.out.with_extension("json.tmp");
    fs::write(&tmp, format!("{json}\n")).map_err(Error::EvalScoreFile)?;
    fs::rename(&tmp, &config.out).map_err(Error::EvalScoreFile)?;
    Ok(report)
}

/// Loads and validates the corpus: every `*.json` in `set_dir` parses, case
/// ids are unique (sorted order is the deterministic run order), every case
/// has at least one expectation, and every fixture directory exists.
/// Public so the corpus validation test exercises the same loader the
/// harness runs.
pub fn load_cases(set_dir: &Path, fixtures_root: &Path) -> Result<Vec<EvalCase>, Error> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(set_dir)
        .map_err(|err| Error::EvalCorpus(format!("cannot list {}: {err}", set_dir.display())))?
    {
        let entry = entry.map_err(|err| {
            Error::EvalCorpus(format!("cannot read {}: {err}", set_dir.display()))
        })?;
        if entry.path().extension().and_then(|ext| ext.to_str()) == Some("json") {
            entries.push(entry.path());
        }
    }
    // Sort by path first so a bad id error message is deterministic too.
    entries.sort();

    let mut cases = Vec::with_capacity(entries.len());
    for path in entries {
        let text = fs::read_to_string(&path)
            .map_err(|err| Error::EvalCorpus(format!("cannot read {}: {err}", path.display())))?;
        let case: EvalCase = serde_json::from_str(&text)
            .map_err(|err| Error::EvalCorpus(format!("{}: {err}", path.display())))?;
        cases.push(case);
    }
    cases.sort_by(|a, b| a.id.cmp(&b.id));
    for pair in cases.windows(2) {
        if pair[0].id == pair[1].id {
            return Err(Error::EvalCorpus(format!(
                "duplicate case id `{}`",
                pair[0].id
            )));
        }
    }
    if cases.is_empty() {
        return Err(Error::EvalCorpus(format!(
            "no case files found in {}",
            set_dir.display()
        )));
    }
    for case in &cases {
        // Case files are an input boundary (--set/--fixtures): the id and
        // fixture name become filesystem paths, so confine their charset
        // instead of letting `..`/separators reach the scratch dir or the
        // fixture join. The id's documented kebab-case is enforced here.
        if case.id.is_empty()
            || !case
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(Error::EvalCorpus(format!(
                "case id `{}` must be kebab-case ([a-z0-9-]+)",
                case.id
            )));
        }
        if case.fixture.is_empty()
            || case.fixture == "."
            || case.fixture == ".."
            || case.fixture.contains(['/', '\\'])
        {
            return Err(Error::EvalCorpus(format!(
                "case `{}` fixture `{}` must be a single directory name",
                case.id, case.fixture
            )));
        }
        if case.expect.is_empty() {
            return Err(Error::EvalCorpus(format!(
                "case `{}` has no expectations",
                case.id
            )));
        }
        // turns_within is crash-vacuous by design, so a case carrying only
        // efficiency bounds passes a run that died on turn zero.
        if case
            .expect
            .iter()
            .all(|e| matches!(e, Expectation::TurnsWithin { .. }))
        {
            return Err(Error::EvalCorpus(format!(
                "case `{}` carries only turns_within bounds — an efficiency bound is never a success signal on its own",
                case.id
            )));
        }
        // `"".contains("")` is true: an empty needle is a silent vacuous
        // pass — invisible in the scores, the report and the digest.
        for expectation in &case.expect {
            if let Expectation::AnswerContains { needle } = expectation
                && needle.is_empty()
            {
                return Err(Error::EvalCorpus(format!(
                    "case `{}` has an empty answer_contains needle",
                    case.id
                )));
            }
        }
        if !fixtures_root.join(&case.fixture).is_dir() {
            return Err(Error::EvalCorpus(format!(
                "case `{}` names missing fixture `{}`",
                case.id, case.fixture
            )));
        }
    }
    Ok(cases)
}

/// The corpus's identity (ADR-0010 §5: the set grows, and gate comparisons
/// are only honest within one ruler). FNV-1a over each case's canonical JSON
/// (sorted by id — the run order) plus every file of the referenced
/// fixtures (sorted relative paths, then bytes). Std-only, keeping the
/// data-foundation zero-dependency property. Public so the corpus tests pin
/// the digest the harness records.
pub fn corpus_digest(cases: &[EvalCase], fixtures_root: &Path) -> Result<String, Error> {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    fn hash(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(FNV_PRIME);
        }
    }

    let mut digest = FNV_OFFSET;
    let mut fixtures: Vec<&str> = cases.iter().map(|case| case.fixture.as_str()).collect();
    fixtures.sort_unstable();
    fixtures.dedup();
    for fixture in fixtures {
        let mut files = Vec::new();
        collect_files(&fixtures_root.join(fixture), fixtures_root, &mut files)
            .map_err(|err| Error::EvalCorpus(format!("cannot walk fixture `{fixture}`: {err}")))?;
        for (rel, full) in files {
            hash(&mut digest, rel.as_bytes());
            let bytes = fs::read(&full).map_err(|err| {
                Error::EvalCorpus(format!("cannot read {}: {err}", full.display()))
            })?;
            hash(&mut digest, &bytes);
        }
    }
    for case in cases {
        let canonical =
            serde_json::to_string(case).expect("EvalCase is plain data; serialization cannot fail");
        hash(&mut digest, canonical.as_bytes());
    }
    Ok(format!("{digest:016x}"))
}

/// Depth-first recursive file collection with forward-slash relative paths,
/// sorted for a platform-independent digest order. Symlinks are not followed
/// (`file_type` does not follow), so a planted link can neither loop the
/// walk nor leak bytes from outside the fixture.
fn collect_files(
    dir: &Path,
    root: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), io::Error> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_files(&path, root, out)?;
        } else if entry.file_type()?.is_file() {
            let rel = path
                .strip_prefix(root)
                .expect("walked path is under the root")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            out.push((rel, path));
        }
    }
    out.sort();
    Ok(())
}

/// One case end to end: scratch copy of the fixture → agent run → fold the
/// trace → score → append score events to the run's log.
async fn run_case(
    config: &EvalConfig,
    case: &EvalCase,
    provider: &Arc<dyn Provider>,
    provider_name: &str,
    model_name: &str,
    log: &Arc<JsonlLog>,
    clock: &Arc<SystemClock>,
) -> Result<CaseResult, Error> {
    let scratch = Scratch::new(&case.id).map_err(|source| Error::EvalWorkspace {
        case: case.id.clone(),
        source,
    })?;
    copy_dir(&config.fixtures.join(&case.fixture), &scratch.0).map_err(|source| {
        Error::EvalWorkspace {
            case: case.id.clone(),
            source,
        }
    })?;

    let ids = Arc::new(SeqIds::default());
    let trace_id = mint_trace_id();
    let mut run_attributes = provider::run_attributes(provider_name, model_name);
    run_attributes.insert(attrs::EVAL_SPLIT.into(), case.split.as_attr().into());
    let telemetry = Telemetry {
        sink: log.clone(),
        clock: clock.clone(),
        ids: ids.clone(),
        trace_id: trace_id.clone(),
        run_attributes,
    };
    // Every case runs against a disposable scratch copy of its fixture, so
    // mutations approve unconditionally — through the same command path a
    // client uses, over a blackhole live sink (nobody watches an eval run).
    let (sender, commands) = command_channel();
    let protocol = ClientProtocol {
        live: Arc::new(approval::AutoResolver::new(
            Arc::new(Blackhole),
            sender,
            approval::approve_all,
        )),
        commands: Arc::new(commands),
    };
    let agent = AgentLoop::new(
        provider.clone(),
        coding_tools(scratch.0.clone()),
        protocol,
        config.max_turns,
        telemetry,
    );
    // A failed run is scored, not propagated: its partial trajectory is the
    // evidence (and run_completed = 0).
    if let Err(error) = agent
        .run(&ChatRequest::user_text(&case.prompt, config.max_tokens))
        .await
    {
        tracing::warn!(case_id = %case.id, %error, "case run failed; scoring the partial trace");
    }

    let events = match log.read_trace(&trace_id) {
        Ok(events) => events,
        // The run died before its first append; score the empty trace.
        Err(cadmus_memory::ReadError::NotFound(_)) => Vec::new(),
        Err(err) => return Err(Error::EvalTraceRead(err)),
    };
    let state = replay_trace(&events);
    let scores = score_case(case, &state);
    append_scores(log, &ids, clock, &trace_id, &events, case, &scores)?;

    // Non-empty by the loader's validation; the explicit guard keeps a
    // vacuous `all()` on empty scores from ever reading as a pass.
    let passed = !scores.is_empty() && scores.iter().all(|score| score.passed == Some(true));
    Ok(CaseResult {
        case_id: case.id.clone(),
        split: case.split,
        trace_id,
        scores,
        passed,
    })
}

/// Appends the case's score events to the run's trace: one eval span hanging
/// off the run's root span (recovered from the start-run event — never an
/// assumed id), one event per score, each carrying the split attribute.
fn append_scores(
    log: &Arc<JsonlLog>,
    ids: &Arc<SeqIds>,
    clock: &Arc<SystemClock>,
    trace_id: &str,
    events: &[Event],
    case: &EvalCase,
    scores: &[cadmus_contract::ScoreEvent],
) -> Result<(), Error> {
    if scores.is_empty() {
        return Ok(());
    }
    let root_span = events.iter().find_map(|event| match &event.kind {
        EventKind::Command(Command::StartRun { .. }) => Some(event.span_id.clone()),
        _ => None,
    });
    let eval_span = format!("s{}", ids.next());
    for score in scores {
        let seq = ids.next();
        let event = Event::new(
            seq,
            format!("e{seq}"),
            trace_id.to_string(),
            eval_span.clone(),
            root_span.clone(),
            clock.now_unix_ms(),
            EventKind::EvalScore(score.clone()),
        )
        .with_attribute(attrs::EVAL_SPLIT, case.split.as_attr());
        log.append(&event).map_err(Error::EvalLog)?;
    }
    Ok(())
}

/// Recursive copy of the fixture into the scratch dir: the run's tools are
/// confined to the copy, so fixtures stay pristine — and edit-capable cases
/// (ADR-0008) later get a disposable workspace for free. Symlinks are not
/// followed (`file_type` does not follow): a planted link cannot make the
/// copy escape the fixture.
fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&path, &target)?;
        } else if entry.file_type()?.is_file() {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// A per-case scratch workspace under the OS temp dir, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(case_id: &str) -> io::Result<Self> {
        let root =
            std::env::temp_dir().join(format!("cadmus-eval-{case_id}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root)?;
        Ok(Self(root))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
