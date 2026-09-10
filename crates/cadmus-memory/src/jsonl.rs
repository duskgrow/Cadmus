//! The JSONL [`EventSink`]: `<root>/YYYY/MM/DD/<trace-id>.jsonl`, one
//! self-describing event per line.
//!
//! - **The shard path is a pure function of the trace id**: ids carry the
//!   UTC run-start date (`tr-YYYYMMDD-…`, see [`mint_trace_id`]), so no
//!   directory scan ever happens, a run crossing midnight keeps one file by
//!   definition, and the file name is immutable once minted. Date-sharded
//!   (never flat) because traces are never auto-deleted: eval sets produce
//!   ~50 traces per invocation, and the day directory is the archival unit
//!   for tiering.
//! - **Durability**: one unbuffered `write_all` per event — line and
//!   newline in a single call, because two writes would let a crash wedged
//!   between them corrupt the *middle* of the file. A process crash can thus
//!   only ever cost the trailing partial line, which readers drop (torn-line
//!   tolerance, ADR-0005 §1). No fsync: the OS page cache outliving the
//!   process is enough at turn-boundary frequencies.
//! - **Tiering** (ADR-0005 §2): the first root is the write root; extra
//!   read roots resolve relocated shards (move the date directories, add a
//!   symlink or the old root as a read root).
//!
//! Single-writer discipline (ADR-0002) is assumed, not enforced: one trace
//! has exactly one appending process at a time.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use cadmus_contract::{ArtifactSink, Event, EventSink, LogError};
use time::OffsetDateTime;

pub struct JsonlLog {
    /// `roots[0]` is the write root; the rest are read-only tiering roots.
    roots: Vec<PathBuf>,
}

impl JsonlLog {
    /// Opens (creating) the write root. Shard directories are created lazily
    /// on each trace's first append.
    pub fn new(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        Ok(Self { roots: vec![root] })
    }

    /// Adds read-only roots for relocated shards (hot/cold tiering).
    #[must_use]
    pub fn with_read_roots(mut self, extra: Vec<PathBuf>) -> Self {
        self.roots.extend(extra);
        self
    }

    /// The file a trace lives in: an existing file wins across all roots,
    /// otherwise the write root's shard path. `None` when the trace id
    /// carries no parseable shard date.
    #[must_use]
    pub fn trace_path(&self, trace_id: &str) -> Option<PathBuf> {
        let rel = shard_rel_path(trace_id)?;
        Some(
            self.roots
                .iter()
                .map(|root| root.join(&rel))
                .find(|path| path.is_file())
                .unwrap_or_else(|| self.roots[0].join(rel)),
        )
    }

    /// Reads one trace back into events. A torn trailing line (crash or
    /// live-tail mid-write) is dropped with a warning; a malformed line
    /// anywhere else is corruption and fails loudly with its line number.
    pub fn read_trace(&self, trace_id: &str) -> Result<Vec<Event>, ReadError> {
        let path = self
            .trace_path(trace_id)
            .filter(|path| path.is_file())
            .ok_or_else(|| ReadError::NotFound(trace_id.to_string()))?;
        let content = fs::read_to_string(&path)?;
        let lines: Vec<(usize, &str)> = content
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .collect();
        let mut events = Vec::with_capacity(lines.len());
        for (position, (index, line)) in lines.iter().enumerate() {
            match serde_json::from_str(line) {
                Ok(event) => events.push(event),
                // Only the last non-blank line may be torn. `position`
                // counts non-blank lines (the torn-tail test); `index` is the
                // file's true line number (the corruption report).
                Err(source) if position + 1 == lines.len() => {
                    tracing::warn!(trace_id, error = %source, "dropping torn trailing line");
                }
                Err(source) => {
                    return Err(ReadError::Parse {
                        line: index + 1,
                        source,
                    });
                }
            }
        }
        Ok(events)
    }
}

impl EventSink for JsonlLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        let mut line = serde_json::to_string(event)?;
        line.push('\n');
        let path = self
            .trace_path(&event.trace_id)
            .ok_or_else(|| LogError::InvalidTraceId(event.trace_id.clone()))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(line.as_bytes())?;
        Ok(())
    }
}

/// The per-trace [`ArtifactSink`] (ADR-0007 item 2/3): spill files live in
/// `<shard>/<trace-id>.artifacts/` — the same pure function of the trace id
/// as the trace file itself, so artifacts shard, tier and archive exactly
/// like their trace. Minted per run via [`JsonlLog::artifacts`].
pub struct JsonlArtifacts {
    /// The write root's artifact directory for this trace.
    dir: PathBuf,
    /// The shard-relative directory the returned references start from —
    /// references stay valid when shards relocate across tiering roots.
    rel_dir: PathBuf,
}

impl JsonlLog {
    /// The per-run artifact sink; `None` when the trace id carries no
    /// parseable shard date (same rejection discipline as the trace file).
    #[must_use]
    pub fn artifacts(&self, trace_id: &str) -> Option<JsonlArtifacts> {
        let rel = shard_rel_path(trace_id)?;
        let rel_dir = rel.with_extension("artifacts");
        Some(JsonlArtifacts {
            dir: self.roots[0].join(&rel_dir),
            rel_dir,
        })
    }
}

impl ArtifactSink for JsonlArtifacts {
    fn spill(&self, name: &str, content: &str) -> Result<String, LogError> {
        // Names are loop-minted (`m<index>.txt`); the guard keeps the
        // shard-relative layout honest against any future caller.
        if name.contains(['/', '\\']) {
            return Err(LogError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("artifact name must not contain a path separator: `{name}`"),
            )));
        }
        fs::create_dir_all(&self.dir)?;
        fs::write(self.dir.join(name), content)?;
        // The reference is `/`-joined by construction (`shard_rel_path`
        // formats with `/`): the log bytes stay platform-identical, and
        // relocation across tiering roots keeps string equality.
        Ok(format!("{}/{name}", self.rel_dir.display()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("trace `{0}` not found under any configured root")]
    NotFound(String),
    #[error("trace read failed: {0}")]
    Io(#[from] io::Error),
    #[error("trace line {line} is corrupt: {source}")]
    Parse {
        line: usize,
        source: serde_json::Error,
    },
}

/// Mints a trace id: `tr-<yyyymmdd UTC>-<nanos hex>-<pid hex>-<seq>`. The
/// date segment is the legible placement label — humans browse shard
/// listings and the adapter extracts it with a substring check; nanos/pid/seq
/// are the uniqueness mechanism (coarse clocks, eval harnesses minting many
/// ids per process). The date is derivable from the nanos — that duplication
/// is deliberate: both segments come from the same clock read, and the
/// adapter never cross-checks them (placement vs identity). Pure — the
/// caller supplies the time; mint time must be the run's start, since the id
/// carries the shard date [`JsonlLog`] computes paths from.
#[must_use]
pub fn mint_trace_id(nanos_since_epoch: u128, pid: u32, sequence: u64) -> String {
    let millis = u64::try_from(nanos_since_epoch / 1_000_000).unwrap_or(u64::MAX);
    let secs = i64::try_from(millis / 1_000).unwrap_or(i64::MAX);
    // The shard date is UTC (machine-facing, AGENTS.md Style); the calendar
    // is the time crate's — calendar math is never hand-rolled.
    let date = OffsetDateTime::from_unix_timestamp(secs)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .date();
    format!(
        "tr-{:04}{:02}{:02}-{nanos_since_epoch:016x}-{pid:x}-{sequence}",
        date.year(),
        u8::from(date.month()),
        date.day(),
    )
}

/// `tr-YYYYMMDD-…` → `YYYY/MM/DD/<trace-id>.jsonl`. Loose day validation
/// (1..=31): the shard label need not be a calendar proof. The whole id must
/// match the minted charset (`[a-z0-9-]`) — it lands verbatim in the file
/// name, so anything else (e.g. `..` or separators in the suffix) is
/// rejected rather than allowed to escape the shard directory.
fn shard_rel_path(trace_id: &str) -> Option<PathBuf> {
    if !trace_id
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return None;
    }
    let date = trace_id.strip_prefix("tr-")?.get(..8)?;
    if date.bytes().any(|byte| !byte.is_ascii_digit()) {
        return None;
    }
    let month: u32 = date[4..6].parse().ok()?;
    let day: u32 = date[6..8].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(PathBuf::from(format!(
        "{}/{}/{}/{trace_id}.jsonl",
        &date[..4],
        &date[4..6],
        &date[6..8],
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadmus_contract::EventKind;

    fn event(trace_id: &str, id: u32) -> cadmus_contract::Event {
        cadmus_contract::Event::new(
            u64::from(id),
            format!("e{id}"),
            trace_id.into(),
            "s1".into(),
            None,
            1_788_393_600_000,
            EventKind::RunFinished { turns: id },
        )
    }

    /// A scratch root under the OS temp dir, unique per test name and
    /// process, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("cadmus-memory-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            Self(root)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // 2026-09-03T00:00:00Z in epoch nanoseconds.
    const SEP_03_2026_NS: u128 = 1_788_393_600_000_000_000;

    #[test]
    fn minted_ids_carry_the_utc_shard_date_and_stay_unique() {
        let first = mint_trace_id(SEP_03_2026_NS, 42, 0);
        let second = mint_trace_id(SEP_03_2026_NS, 42, 1);
        assert!(first.starts_with("tr-20260903-"), "got {first}");
        assert_ne!(first, second, "the sequence separates rapid mints");
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "filesystem-safe charset, got {first}"
        );
        // Leap day lands right (2024-02-29T12:34:56Z): the calendar is the
        // time crate's; this pins that OUR conversion feeds it correctly.
        let leap = mint_trace_id(1_709_210_096_000_000_000, 42, 0);
        assert!(leap.starts_with("tr-20240229-"), "got {leap}");
        // Absurd input degrades to the epoch date instead of panicking or
        // emitting an unshardable segment (the time crate's range ceiling).
        let absurd = mint_trace_id(u128::MAX, 42, 0);
        assert!(absurd.starts_with("tr-19700101-"), "got {absurd}");
    }

    #[test]
    fn shard_paths_parse_the_id_date() {
        assert_eq!(
            shard_rel_path("tr-20260903-abcdef-2a-0"),
            Some(PathBuf::from("2026/09/03/tr-20260903-abcdef-2a-0.jsonl"))
        );
        // Rejections: no prefix, short date, non-digits, month 13, day 0.
        assert_eq!(shard_rel_path("xx-20260903-a"), None);
        assert_eq!(shard_rel_path("tr-2026090-a"), None);
        assert_eq!(shard_rel_path("tr-2026aa03-a"), None);
        assert_eq!(shard_rel_path("tr-20261303-a"), None);
        assert_eq!(shard_rel_path("tr-20260900-a"), None);
        // The suffix becomes a file name: separators and traversal are
        // rejected over the whole id, not just the date window.
        assert_eq!(shard_rel_path("tr-20260903-../../x"), None);
        assert_eq!(shard_rel_path("tr-20260903-a/b"), None);
        assert_eq!(shard_rel_path("tr-20260903-CAPS"), None);
    }

    #[test]
    fn append_writes_into_the_id_dated_shard_and_reads_back() {
        let scratch = Scratch::new("roundtrip");
        let log = JsonlLog::new(scratch.0.clone()).expect("open log");
        log.append(&event("tr-20260903-a", 1)).expect("append");
        log.append(&event("tr-20260903-a", 2)).expect("append");

        let expected = scratch.0.join("2026/09/03/tr-20260903-a.jsonl");
        assert!(expected.is_file(), "shard file at {expected:?}");
        assert_eq!(
            log.trace_path("tr-20260903-a").as_deref(),
            Some(expected.as_path())
        );

        let events = log.read_trace("tr-20260903-a").expect("read");
        assert_eq!(
            events,
            vec![event("tr-20260903-a", 1), event("tr-20260903-a", 2)]
        );
    }

    #[test]
    fn invalid_trace_ids_are_rejected_not_misplaced() {
        let scratch = Scratch::new("invalid-id");
        let log = JsonlLog::new(scratch.0.clone()).expect("open log");
        let err = log
            .append(&event("tr-no-date-here", 1))
            .expect_err("must reject");
        assert!(matches!(err, LogError::InvalidTraceId(_)));
    }

    #[test]
    fn artifacts_spill_under_the_trace_shard_and_reference_it_relatively() {
        let scratch = Scratch::new("artifacts");
        let log = JsonlLog::new(scratch.0.clone()).expect("log");
        let trace_id = "tr-20260903-abc123";
        let sink = log.artifacts(trace_id).expect("shard parses");
        let reference = sink.spill("m4.txt", "full text").expect("spill");
        assert_eq!(
            reference, "2026/09/03/tr-20260903-abc123.artifacts/m4.txt",
            "shard-relative, so artifacts relocate with their trace"
        );
        let written = std::fs::read_to_string(scratch.0.join(&reference)).expect("read back");
        assert_eq!(written, "full text");
        // The name guard keeps the shard-relative layout honest.
        assert!(sink.spill("../escape.txt", "x").is_err());
    }

    #[test]
    fn traces_stay_separate_and_off_root_reads_fail_cleanly() {
        let scratch = Scratch::new("separate");
        let log = JsonlLog::new(scratch.0.clone()).expect("open log");
        log.append(&event("tr-20260903-a", 1)).expect("append");
        log.append(&event("tr-20260904-b", 1)).expect("append");

        assert_eq!(log.read_trace("tr-20260903-a").expect("read a").len(), 1);
        assert_eq!(log.read_trace("tr-20260904-b").expect("read b").len(), 1);
        let missing = log
            .read_trace("tr-20260903-missing")
            .expect_err("must fail");
        assert!(matches!(missing, ReadError::NotFound(_)));
    }

    #[test]
    fn torn_trailing_line_is_dropped() {
        let scratch = Scratch::new("torn");
        let log = JsonlLog::new(scratch.0.clone()).expect("open log");
        log.append(&event("tr-20260903-a", 1)).expect("append");
        let path = log.trace_path("tr-20260903-a").expect("path");
        let mut file = OpenOptions::new().append(true).open(&path).expect("open");
        file.write_all(b"{\"id\":\"e2\",\"trace_id\":\"tr")
            .expect("write torn line");
        drop(file);

        let events = log
            .read_trace("tr-20260903-a")
            .expect("read tolerates the tail");
        assert_eq!(events, vec![event("tr-20260903-a", 1)]);
    }

    #[test]
    fn corrupt_mid_file_line_is_loud_not_dropped() {
        let scratch = Scratch::new("corrupt");
        let log = JsonlLog::new(scratch.0.clone()).expect("open log");
        log.append(&event("tr-20260903-a", 1)).expect("append");
        log.append(&event("tr-20260903-a", 2)).expect("append");
        let path = log.trace_path("tr-20260903-a").expect("path");
        let content = fs::read_to_string(&path).expect("read raw");
        let mut lines: Vec<&str> = content.lines().collect();
        lines[0] = "{corrupt";
        fs::write(&path, lines.join("\n") + "\n").expect("rewrite");

        let err = log
            .read_trace("tr-20260903-a")
            .expect_err("must fail loudly");
        assert!(matches!(err, ReadError::Parse { line: 1, .. }));
    }

    #[test]
    fn blank_lines_are_skipped_without_shifting_line_numbers() {
        let scratch = Scratch::new("blank-lines");
        let log = JsonlLog::new(scratch.0.clone()).expect("open log");
        log.append(&event("tr-20260903-a", 1)).expect("append");
        log.append(&event("tr-20260903-a", 2)).expect("append");
        let path = log.trace_path("tr-20260903-a").expect("path");
        let content = fs::read_to_string(&path).expect("read raw");
        // line 1: event, line 2: blank, line 3: event, line 4: torn tail.
        let mut lines: Vec<&str> = content.lines().collect();
        lines.insert(1, "");
        fs::write(&path, lines.join("\n") + "\n" + "{torn").expect("rewrite");
        let events = log.read_trace("tr-20260903-a").expect("torn tail dropped");
        assert_eq!(events.len(), 2);

        // Corrupt the first line: the reported number must be the file's.
        let corrupt = format!("{{corrupt\n\n{}\n", lines[2]);
        fs::write(&path, corrupt).expect("rewrite");
        let err = log
            .read_trace("tr-20260903-a")
            .expect_err("loud corruption");
        assert!(matches!(err, ReadError::Parse { line: 1, .. }));
    }

    #[test]
    fn relocated_shards_resolve_from_read_roots() {
        let hot = Scratch::new("hot");
        let cold = Scratch::new("cold");
        let writer = JsonlLog::new(hot.0.clone()).expect("open log");
        writer.append(&event("tr-20260903-old", 1)).expect("append");

        // Tiering: the shard dir moves to cold storage; the running log has a
        // new write root and the old root as a read root.
        fs::create_dir_all(cold.0.as_path()).expect("mkdir cold");
        fs::rename(hot.0.join("2026"), cold.0.join("2026")).expect("move shard");
        let log = JsonlLog::new(hot.0.join("new"))
            .expect("open new root")
            .with_read_roots(vec![hot.0.clone(), cold.0.clone()]);

        // Reads find the moved trace; appends to it follow the shard.
        assert_eq!(
            log.read_trace("tr-20260903-old").expect("read moved").len(),
            1
        );
        log.append(&event("tr-20260903-old", 2))
            .expect("append follows shard");
        assert_eq!(
            log.read_trace("tr-20260903-old").expect("read moved").len(),
            2
        );
        // A new trace lands in the new write root.
        log.append(&event("tr-20260904-new", 1))
            .expect("append new");
        assert!(hot.0.join("new/2026/09/04/tr-20260904-new.jsonl").is_file());
    }
}
