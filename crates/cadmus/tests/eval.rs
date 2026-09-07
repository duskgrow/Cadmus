//! Eval harness integration tests (ADR-0005 §7): the corpus validation test
//! keeps `evals/` mechanically honest in CI (no live calls — the set itself
//! is data), and the scripted end-to-end test drives a full harness run with
//! the replay fake, asserting scores land both in the score file and in each
//! run's own trajectory log.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use cadmus::{EvalConfig, corpus_digest, load_cases, run_eval};
use cadmus_contract::{EvalReport, EvalSplit, FinishReason, ModelError, StreamChunk};
use cadmus_core::ReplayProvider;
use cadmus_memory::JsonlLog;

/// The real corpus (repo-root `evals/`): parseable, uniquely identified,
/// large enough, both splits present, fixtures resolvable — the loader is
/// the same one the harness runs.
#[test]
fn eval_corpus_is_well_formed() {
    let evals = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals");
    let cases = load_cases(&evals.join("cases"), &evals.join("fixtures"))
        .expect("the corpus must load — fix the reported case file");
    assert!(
        cases.len() >= 50,
        "eval set v1 requires ≥50 scenario cases, found {}",
        cases.len()
    );
    assert!(
        cases.iter().any(|case| case.split == EvalSplit::Search),
        "no search-split cases"
    );
    assert!(
        cases.iter().any(|case| case.split == EvalSplit::Holdout),
        "no holdout-split cases"
    );
}

/// A scratch tree under the OS temp dir, unique per test name and process,
/// removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("cadmus-eval-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create scratch");
        Self(root)
    }

    fn write(&self, path: &str, contents: &str) {
        let full = self.0.join(path);
        fs::create_dir_all(full.parent().expect("parent")).expect("mkdirs");
        fs::write(full, contents).expect("write");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_case(scratch: &Scratch, id: &str, expect: &serde_json::Value) {
    scratch.write(
        &format!("cases/{id}.json"),
        &serde_json::json!({
            "id": id,
            "split": "search",
            "fixture": "f1",
            "prompt": "What is the default port? Answer with the number only.",
            "expect": expect,
        })
        .to_string(),
    );
}

fn tool_call_script(args: &str) -> Vec<Result<StreamChunk, ModelError>> {
    ReplayProvider::script(vec![
        StreamChunk::ToolCallStart {
            index: 0,
            id: "c1".into(),
            name: "read_file".into(),
        },
        StreamChunk::ToolArgsDelta {
            index: 0,
            fragment: args.into(),
        },
        StreamChunk::ToolCallEnd { index: 0 },
        StreamChunk::Done {
            finish: FinishReason::ToolCalls,
        },
    ])
}

fn text_script(text: &str) -> Vec<Result<StreamChunk, ModelError>> {
    ReplayProvider::script(vec![
        StreamChunk::TextDelta(text.into()),
        StreamChunk::Done {
            finish: FinishReason::Stop,
        },
    ])
}

/// Full harness run over a two-case scratch corpus: one case passes every
/// metric, the other fails `answer_contains`. Asserts the aggregate report,
/// the score file on disk, and the score events appended to each run's own
/// trace (split attribute included, ADR-0010 §4).
#[tokio::test]
async fn eval_run_scores_and_records_events() {
    let scratch = Scratch::new("harness");
    scratch.write("fixtures/f1/config.toml", "default_port = 42\n");
    write_case(
        &scratch,
        "case-a",
        &serde_json::json!([
            {"metric": "run_completed"},
            {"metric": "answer_contains", "needle": "42"},
            {"metric": "tool_called", "name": "read_file"},
            {"metric": "turns_within", "max": 4},
        ]),
    );
    write_case(
        &scratch,
        "case-b",
        &serde_json::json!([
            {"metric": "run_completed"},
            {"metric": "answer_contains", "needle": "42"},
        ]),
    );

    // Scripts queue in case-id order (the set's run order): case-a reads the
    // file then answers; case-b answers wrong immediately.
    let provider = ReplayProvider::new([
        tool_call_script("{\"path\": \"config.toml\"}"),
        text_script("The default port is 42."),
        text_script("I do not know."),
    ]);
    let config = EvalConfig {
        set: scratch.0.join("cases"),
        fixtures: scratch.0.join("fixtures"),
        max_tokens: 1_024,
        max_turns: 8,
        trace_root: Some(scratch.0.join("traces")),
        out: scratch.0.join("report/scores.json"),
    };
    let report = run_eval(&config, Arc::new(provider), "test", "test-model")
        .await
        .expect("harness run");

    // The ruler's identity is pinned on the report (ADR-0010 §5).
    assert_eq!(report.set_digest.len(), 16);
    assert!(report.set_digest.chars().all(|c| c.is_ascii_hexdigit()));

    assert_eq!(report.total, 2);
    assert_eq!(report.passed, 1);
    assert_eq!(report.results[0].case_id, "case-a");
    assert!(report.results[0].passed);
    assert_eq!(report.results[1].case_id, "case-b");
    assert!(!report.results[1].passed);
    // case-b completes the run but misses the needle.
    let b_metrics: Vec<(&str, bool)> = report.results[1]
        .scores
        .iter()
        .map(|score| (score.metric.as_str(), score.passed == Some(true)))
        .collect();
    assert_eq!(
        b_metrics,
        [("run_completed", true), ("answer_contains", false)]
    );

    // The score file on disk is the same artifact.
    let on_disk: EvalReport =
        serde_json::from_str(&fs::read_to_string(&config.out).expect("score file written"))
            .expect("score file parses");
    assert_eq!(on_disk, report);

    assert_case_a_trace(&scratch, &report.results[0].trace_id);

    // case-b's trace carries its scores too (partial credit visible).
    let log = JsonlLog::new(scratch.0.join("traces")).expect("open log");
    let b_events = log
        .read_trace(&report.results[1].trace_id)
        .expect("trace b");
    assert_eq!(
        b_events
            .iter()
            .filter(|event| matches!(event.kind, cadmus_contract::EventKind::EvalScore(_)))
            .count(),
        2
    );
}

/// Score events live in the run's own trace: four `eval_score` events for
/// case-a, one eval span hanging off the run root, split attr everywhere.
fn assert_case_a_trace(scratch: &Scratch, trace_id: &str) {
    let log = JsonlLog::new(scratch.0.join("traces")).expect("open log");
    let events = log.read_trace(trace_id).expect("trace");
    let root_span = events
        .iter()
        .find_map(|event| match &event.kind {
            cadmus_contract::EventKind::Command(cadmus_contract::Command::StartRun { .. }) => {
                Some(event.span_id.clone())
            }
            _ => None,
        })
        .expect("start_run event");
    let scores: Vec<_> = events
        .iter()
        .filter(|event| matches!(event.kind, cadmus_contract::EventKind::EvalScore(_)))
        .collect();
    assert_eq!(scores.len(), 4, "one score event per expectation");
    assert!(
        scores
            .iter()
            .all(|event| event.parent_span_id.as_deref() == Some(root_span.as_str()))
    );
    assert!(
        scores
            .iter()
            .all(|event| event.attributes.get("selfevol.eval_split")
                == Some(&serde_json::Value::String("search".into())))
    );
    assert!(
        scores
            .windows(2)
            .all(|pair| pair[0].span_id == pair[1].span_id),
        "all scores of a case share one eval span"
    );

    // The start-run command carries the split too, so reflection input
    // selection can exclude holdout traces without scanning for scores.
    let start_run = events
        .iter()
        .find(|event| event.span_id == root_span)
        .expect("root span event");
    assert_eq!(
        start_run.attributes.get("selfevol.eval_split"),
        Some(&serde_json::Value::String("search".into()))
    );
}

/// A corpus that fails validation (duplicate id) fails the load, not the run.
#[test]
fn load_cases_rejects_duplicate_ids() {
    let scratch = Scratch::new("dup-ids");
    scratch.write("fixtures/f1/x.txt", "x");
    write_case(
        &scratch,
        "same-id",
        &serde_json::json!([{"metric": "run_completed"}]),
    );
    // A second file, different name, same id.
    scratch.write(
        "cases/dup.json",
        &serde_json::json!({
            "id": "same-id",
            "split": "search",
            "fixture": "f1",
            "prompt": "p",
            "expect": [{"metric": "run_completed"}],
        })
        .to_string(),
    );
    let err = load_cases(&scratch.0.join("cases"), &scratch.0.join("fixtures"))
        .expect_err("duplicate ids must fail");
    assert!(err.to_string().contains("same-id"), "got: {err}");
}

/// Case files with unknown fields fail loudly (typo protection for a
/// hand-authored, trust-anchor corpus).
#[test]
fn load_cases_rejects_unknown_fields() {
    let scratch = Scratch::new("unknown-field");
    scratch.write("fixtures/f1/x.txt", "x");
    scratch.write(
        "cases/typo.json",
        &serde_json::json!({
            "id": "typo",
            "split": "search",
            "fixturee": "f1",
            "prompt": "p",
            "expect": [{"metric": "run_completed"}],
        })
        .to_string(),
    );
    load_cases(&scratch.0.join("cases"), &scratch.0.join("fixtures"))
        .expect_err("unknown field `fixturee` must fail");
}

/// The trust-anchor guards: a case without expectations would pass vacuously
/// and inflate the pass rate; a missing fixture or an empty set means the
/// corpus is broken, not that the model scored zero.
#[test]
fn load_cases_rejects_empty_expectations() {
    let scratch = Scratch::new("empty-expect");
    scratch.write("fixtures/f1/x.txt", "x");
    write_case(&scratch, "no-expect", &serde_json::json!([]));
    load_cases(&scratch.0.join("cases"), &scratch.0.join("fixtures"))
        .expect_err("empty expectations must fail");
}

#[test]
fn load_cases_rejects_a_missing_fixture() {
    let scratch = Scratch::new("missing-fixture");
    write_case(
        &scratch,
        "no-fixture",
        &serde_json::json!([{"metric": "run_completed"}]),
    );
    let err = load_cases(&scratch.0.join("cases"), &scratch.0.join("fixtures"))
        .expect_err("a missing fixture must fail");
    assert!(err.to_string().contains("no-fixture"), "got: {err}");
}

#[test]
fn load_cases_rejects_an_empty_set() {
    let scratch = Scratch::new("empty-set");
    load_cases(&scratch.0.join("cases"), &scratch.0.join("fixtures"))
        .expect_err("an empty case dir must fail");
}

/// Ids and fixture names become filesystem paths; both are validated at the
/// boundary instead of reaching the scratch dir or the fixture join.
#[test]
fn load_cases_rejects_path_shaped_ids_and_fixtures() {
    let scratch = Scratch::new("path-shape");
    scratch.write("fixtures/f1/x.txt", "x");
    scratch.write(
        "cases/bad-id.json",
        &serde_json::json!({
            "id": "../escape",
            "split": "search",
            "fixture": "f1",
            "prompt": "p",
            "expect": [{"metric": "run_completed"}],
        })
        .to_string(),
    );
    let err = load_cases(&scratch.0.join("cases"), &scratch.0.join("fixtures"))
        .expect_err("a path-shaped id must fail");
    assert!(err.to_string().contains("kebab-case"), "got: {err}");

    fs::remove_file(scratch.0.join("cases/bad-id.json")).expect("remove");
    scratch.write(
        "cases/bad-fixture.json",
        &serde_json::json!({
            "id": "bad-fixture",
            "split": "search",
            "fixture": "../../elsewhere",
            "prompt": "p",
            "expect": [{"metric": "run_completed"}],
        })
        .to_string(),
    );
    load_cases(&scratch.0.join("cases"), &scratch.0.join("fixtures"))
        .expect_err("a path-shaped fixture must fail");

    // "." is not empty, not "..", carries no separator — yet joining it
    // would copy and hash the whole fixtures root.
    fs::remove_file(scratch.0.join("cases/bad-fixture.json")).expect("remove");
    scratch.write(
        "cases/dot-fixture.json",
        &serde_json::json!({
            "id": "dot-fixture",
            "split": "search",
            "fixture": ".",
            "prompt": "p",
            "expect": [{"metric": "run_completed"}],
        })
        .to_string(),
    );
    load_cases(&scratch.0.join("cases"), &scratch.0.join("fixtures"))
        .expect_err("a `.` fixture must fail");
}

/// `"".chars().all(...)` is true — the kebab-case guard must also reject
/// the empty string it would otherwise wave through.
#[test]
fn load_cases_rejects_an_empty_id() {
    let scratch = Scratch::new("empty-id");
    scratch.write("fixtures/f1/x.txt", "x");
    scratch.write(
        "cases/empty-id.json",
        &serde_json::json!({
            "id": "",
            "split": "search",
            "fixture": "f1",
            "prompt": "p",
            "expect": [{"metric": "run_completed"}],
        })
        .to_string(),
    );
    let err = load_cases(&scratch.0.join("cases"), &scratch.0.join("fixtures"))
        .expect_err("an empty id must fail");
    assert!(err.to_string().contains("kebab-case"), "got: {err}");
}

/// `"".contains("")` is true: an empty needle scores a silent vacuous pass
/// — the pass-rate inflation class the trust-anchor guards exist to close.
#[test]
fn load_cases_rejects_an_empty_needle() {
    let scratch = Scratch::new("empty-needle");
    scratch.write("fixtures/f1/x.txt", "x");
    write_case(
        &scratch,
        "empty-needle",
        &serde_json::json!([{"metric": "answer_contains", "needle": ""}]),
    );
    let err = load_cases(&scratch.0.join("cases"), &scratch.0.join("fixtures"))
        .expect_err("an empty needle must fail");
    assert!(err.to_string().contains("empty-needle"), "got: {err}");
}

/// `turns_within` is crash-vacuous by design, so a case carrying only
/// efficiency bounds passes a run that died on turn zero.
#[test]
fn load_cases_rejects_efficiency_only_expectations() {
    let scratch = Scratch::new("efficiency-only");
    scratch.write("fixtures/f1/x.txt", "x");
    write_case(
        &scratch,
        "efficiency-only",
        &serde_json::json!([{"metric": "turns_within", "max": 4}]),
    );
    let err = load_cases(&scratch.0.join("cases"), &scratch.0.join("fixtures"))
        .expect_err("turns_within-only expectations must fail");
    assert!(err.to_string().contains("efficiency-only"), "got: {err}");
}

/// The digest pins the ruler: stable for an identical corpus, sensitive to
/// both case and fixture edits — so two score files can only be compared
/// when their digests match (ADR-0010 §5).
#[test]
fn corpus_digest_is_stable_and_edit_sensitive() {
    let scratch = Scratch::new("digest");
    scratch.write("fixtures/f1/config.toml", "default_port = 42\n");
    write_case(
        &scratch,
        "case-a",
        &serde_json::json!([{"metric": "answer_contains", "needle": "42"}]),
    );
    let cases =
        load_cases(&scratch.0.join("cases"), &scratch.0.join("fixtures")).expect("corpus loads");

    let digest = corpus_digest(&cases, &scratch.0.join("fixtures")).expect("digest");
    let again = corpus_digest(&cases, &scratch.0.join("fixtures")).expect("digest");
    assert_eq!(digest, again, "same corpus, same digest");

    // A whitespace-only case edit normalizes away (canonical JSON), so the
    // digest tracks meaning, not formatting.
    let mut spaced = fs::read_to_string(scratch.0.join("cases/case-a.json")).expect("read");
    spaced = spaced.replace("\"fixture\":", "\"fixture\":  ");
    fs::write(scratch.0.join("cases/case-a.json"), &spaced).expect("write");
    let cases =
        load_cases(&scratch.0.join("cases"), &scratch.0.join("fixtures")).expect("corpus loads");
    assert_eq!(
        digest,
        corpus_digest(&cases, &scratch.0.join("fixtures")).expect("digest"),
        "formatting-only case edits keep the digest"
    );

    scratch.write("fixtures/f1/config.toml", "default_port = 43\n");
    assert_ne!(
        digest,
        corpus_digest(&cases, &scratch.0.join("fixtures")).expect("digest"),
        "a fixture byte change moves the digest"
    );

    // A pure rename moves the digest too: relative paths are hashed, not
    // just bytes — file names are content the agent reads.
    scratch.write("fixtures/f1/config.toml", "default_port = 42\n");
    fs::rename(
        scratch.0.join("fixtures/f1/config.toml"),
        scratch.0.join("fixtures/f1/renamed.toml"),
    )
    .expect("rename");
    assert_ne!(
        digest,
        corpus_digest(&cases, &scratch.0.join("fixtures")).expect("digest"),
        "a pure fixture rename moves the digest"
    );

    scratch.write("fixtures/f1/config.toml", "default_port = 42\n");
    write_case(
        &scratch,
        "case-a",
        &serde_json::json!([{"metric": "answer_contains", "needle": "43"}]),
    );
    let cases =
        load_cases(&scratch.0.join("cases"), &scratch.0.join("fixtures")).expect("corpus loads");
    assert_ne!(
        digest,
        corpus_digest(&cases, &scratch.0.join("fixtures")).expect("digest"),
        "a case edit moves the digest"
    );
}
