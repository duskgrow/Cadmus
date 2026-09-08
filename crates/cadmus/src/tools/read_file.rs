use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use cadmus_contract::ToolSpec;
use cadmus_core::{AgentTool, Concurrency, ToolError};
use serde_json::{Value, json};

use super::{error, resolve};

/// `read_file` per-call output cap — applies to the returned window, never
/// to the file head, so every line of a large file stays reachable.
const MAX_OUTPUT_BYTES: usize = 512 * 1024;
/// `read_file` per-line cut; the tail is drained, never stored whole.
const MAX_LINE_BYTES: usize = 64 * 1024;

/// `read_file`: UTF-8 text paged by 1-based line windows. The file is
/// streamed, so windows past the first 512 KiB are reachable; the byte cap
/// applies to the returned window, never to the file head.
pub(super) struct ReadFile {
    pub(super) root: PathBuf,
}

#[async_trait]
impl AgentTool for ReadFile {
    /// Read-only and stateless: parallel-safe like every perception tool
    /// (ADR-0008 item 2).
    fn concurrency(&self) -> Concurrency {
        Concurrency::ParallelSafe
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read a UTF-8 text file from the workspace, paged by 1-based line windows \
                          (offset/limit). Use this to view a file whose path you know; to find which \
                          file to read, use grep first. Binary files are rejected (UTF-8 only). \
                          `path` is relative to the workspace root; absolute paths are honored only \
                          inside the workspace. UTF-8 only: a window containing non-UTF-8 bytes is \
                          rejected as binary. Output is capped at 512 KiB per call and any single \
                          line longer than 64 KiB is cut inline; when output is cut, the footer names \
                          the exact offset to resume from — follow it instead of re-reading from the top."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "path relative to the workspace root; absolute paths work only inside the workspace"},
                    "offset": {"type": "integer", "minimum": 1, "description": "first line to read (1-based, default 1); use the resume offset from a truncation footer"},
                    "limit": {"type": "integer", "minimum": 1, "description": "maximum number of lines to read; prefer a few hundred lines per call to keep context cheap"},
                },
                "required": ["path"],
            }),
        }
    }

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
        let path = arguments["path"].as_str().unwrap_or_default();
        let canonical = resolve(&self.root, path).map_err(|message| error("read_file", message))?;
        let metadata = fs::metadata(&canonical)
            .map_err(|err| error("read_file", format!("cannot stat `{path}`: {err}")))?;
        if !metadata.is_file() {
            return Err(error("read_file", format!("`{path}` is not a file")));
        }

        let offset = match arguments.get("offset") {
            None => 1,
            Some(value) => value
                .as_u64()
                .filter(|n| *n >= 1)
                .map(|n| n.try_into().unwrap_or(usize::MAX))
                .ok_or_else(|| {
                    error(
                        "read_file",
                        format!("offset must be a positive integer, got {value}"),
                    )
                })?,
        };
        let limit = match arguments.get("limit") {
            None => usize::MAX,
            Some(value) => value
                .as_u64()
                .filter(|n| *n >= 1)
                .map(|n| n.try_into().unwrap_or(usize::MAX))
                .ok_or_else(|| {
                    error(
                        "read_file",
                        format!("limit must be a positive integer, got {value}"),
                    )
                })?,
        };

        let window = read_window(&canonical, offset, limit)
            .map_err(|err| error("read_file", format!("cannot read `{path}`: {err}")))?;
        Ok(Value::String(window.render(path, offset)))
    }
}

/// One page of a streamed read. Footers never claim more than was verified:
/// an exact total appears only when EOF was reached, and every cut names the
/// offset that resumes past it (ADR-0007 layer 1: silent truncation is a
/// defect).
struct WindowRead {
    /// Shown lines; fragments carry an inline cut marker.
    lines: Vec<String>,
    /// 1-based number of the last shown line.
    shown_end: usize,
    /// Exact file total — known only when the read (or its probe) hit EOF.
    total_lines: Option<usize>,
    /// Resume offset when the 512 KiB output cap stopped the read.
    capped: Option<usize>,
    /// Lines exist beyond `shown_end` (the limit cut the window short).
    more_lines: bool,
}

impl WindowRead {
    fn render(self, path: &str, offset: usize) -> String {
        if self.lines.is_empty() {
            return match self.total_lines {
                Some(0) => format!("`{path}` is empty (0 lines)"),
                Some(total) => format!(
                    "offset {offset} is past the end of `{path}` ({total} lines); \
                     retry with offset between 1 and {total}"
                ),
                // Unreachable: an empty window means the scan ran to EOF.
                None => format!("no lines shown for `{path}`"),
            };
        }
        let mut output = self.lines.join("\n");
        if let Some(resume) = self.capped {
            let _ = write!(
                output,
                "\n… [capped at 512 KiB — resume with offset={resume}]"
            );
        } else if self.more_lines {
            let _ = write!(
                output,
                "\n… [lines {offset}–{}; more lines follow — resume with offset={}]",
                self.shown_end,
                self.shown_end + 1,
            );
        } else if let Some(total) = self.total_lines
            && (offset > 1 || self.shown_end < total)
        {
            let _ = write!(output, "\n… [lines {offset}–{} of {total}]", self.shown_end);
        }
        output
    }
}

/// Streams one window of `path`: lines are read one at a time and only the
/// window is kept, so any line of any file is reachable and transient memory
/// stays bounded regardless of file size.
fn read_window(path: &Path, offset: usize, limit: usize) -> std::io::Result<WindowRead> {
    let mut reader = std::io::BufReader::new(fs::File::open(path)?);
    let mut lines = Vec::new();
    let mut line_no = 0;
    let mut out_bytes = 0;
    let mut capped = None;
    let mut more_lines = false;

    loop {
        if lines.len() >= limit {
            more_lines = true;
            break;
        }
        if out_bytes >= MAX_OUTPUT_BYTES {
            // The cap landed on a line boundary; resume at the next line.
            capped = Some(line_no + 1);
            break;
        }
        let Some((raw, oversized)) = read_physical_line(&mut reader)? else {
            break; // EOF
        };
        line_no += 1;
        if line_no < offset {
            continue;
        }
        let text = validate_window_line(&raw, oversized)?;

        let budget = MAX_OUTPUT_BYTES - out_bytes;
        if text.len() > budget {
            // The cap cut this line's content; back off to a char boundary
            // and name the line as the resume offset — re-reading there
            // returns it in full.
            let mut end = budget;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            capped = Some(line_no);
            if end > 0 {
                lines.push(format!("{} … [cut at 512 KiB]", &text[..end]));
            }
            break;
        }
        let mut display = text;
        if oversized {
            display.push_str(" … [line cut at 64 KiB]");
        }
        // +1: the newline this line carries when joined into the output.
        out_bytes += display.len() + 1;
        lines.push(display);
    }

    let mut total_lines = None;
    if more_lines || capped == Some(line_no + 1) {
        // Stopped on a line boundary: probe one more line so an EOF here
        // upgrades the footer to an exact total. A probe failure must not
        // destroy a good window — treat it as "more exists".
        if let Ok(None) = read_physical_line(&mut reader) {
            total_lines = Some(line_no);
            more_lines = false;
            capped = None;
        }
    } else if capped.is_none() {
        // EOF ended the scan.
        total_lines = Some(line_no);
    }

    Ok(WindowRead {
        lines,
        shown_end: line_no,
        total_lines,
        capped,
        more_lines,
    })
}

/// Decodes one in-window line's bytes. Only the window is decoded — skipped
/// lines and drained tails pass through as bytes, so the UTF-8 guarantee
/// covers exactly what the model receives. A line the 64 KiB guard cut
/// mid-character yields its boundary-aligned head; genuinely invalid bytes
/// fail the call.
fn validate_window_line(raw: &[u8], oversized: bool) -> std::io::Result<String> {
    match std::str::from_utf8(raw) {
        Ok(text) => Ok(trim_line_end(text).to_owned()),
        Err(err) if oversized && err.error_len().is_none() => {
            let head = std::str::from_utf8(&raw[..err.valid_up_to()])
                .expect("valid_up_to is a UTF-8 boundary");
            Ok(trim_line_end(head).to_owned())
        }
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a UTF-8 text file",
        )),
    }
}

/// `str::lines` terminator semantics: one trailing `\n`, then one `\r`.
fn trim_line_end(line: &str) -> &str {
    line.strip_suffix('\n')
        .map_or(line, |l| l.strip_suffix('\r').unwrap_or(l))
}

/// Reads one physical line as bytes, bounded: a line over 64 KiB is cut and
/// its tail drained without storing, so transient allocation stays flat for
/// any input. Returns `None` at EOF.
fn read_physical_line(
    reader: &mut impl std::io::BufRead,
) -> std::io::Result<Option<(Vec<u8>, bool)>> {
    use std::io::{BufRead as _, Read as _};
    let mut raw = Vec::new();
    let read = reader
        .by_ref()
        .take(MAX_LINE_BYTES as u64 + 1)
        .read_until(b'\n', &mut raw)?;
    if read == 0 {
        return Ok(None);
    }
    let mut oversized = false;
    if !raw.ends_with(b"\n") && raw.len() == MAX_LINE_BYTES + 1 {
        // Possibly over the guard: drain the tail fragment by fragment. An
        // immediate EOF means the line was exactly guard-sized — complete.
        loop {
            let mut sink = Vec::new();
            let drained = reader
                .by_ref()
                .take(MAX_LINE_BYTES as u64 + 1)
                .read_until(b'\n', &mut sink)?;
            if drained == 0 {
                break;
            }
            oversized = true;
            if sink.ends_with(b"\n") {
                break;
            }
        }
    }
    Ok(Some((raw, oversized)))
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use serde_json::json;

    use super::super::tests::{Scratch, tool};

    #[tokio::test]
    async fn read_file_returns_contents_with_line_window() {
        let scratch = Scratch::new("read-window");
        scratch.write("src/main.rs", "fn main() {\n    // TODO\n}\n");
        let read_file = tool(&scratch.0, "read_file");

        let full = read_file
            .invoke(json!({"path": "src/main.rs"}))
            .await
            .expect("read");
        assert_eq!(full, json!("fn main() {\n    // TODO\n}"));

        let window = read_file
            .invoke(json!({"path": "src/main.rs", "offset": 2, "limit": 1}))
            .await
            .expect("read window");
        let text = window.as_str().expect("string");
        assert!(text.starts_with("    // TODO"), "got: {text}");
        // A limit cut names the resume offset; an exact total is only
        // reported once EOF was reached.
        assert!(
            text.contains("lines 2–2; more lines follow — resume with offset=3"),
            "got: {text}"
        );
    }

    /// Builds `count` lines of known byte size so byte-cap math is exact:
    /// `line NNNN ` (10 B) + `pad` '€' (3 B each) + '\n' = 11 + 3·pad bytes.
    fn numbered_lines(count: usize, pad: usize) -> String {
        (1..=count).fold(String::new(), |mut out, n| {
            let _ = writeln!(out, "line {n:04} {}", "€".repeat(pad));
            out
        })
    }

    #[tokio::test]
    async fn read_file_reaches_windows_past_the_byte_cap() {
        let scratch = Scratch::new("read-past-cap");
        // 851 B/line × 3000 ≈ 2.4 MiB — far past the old 512 KiB head cap,
        // which made these lines unreachable.
        scratch.write("big.txt", &numbered_lines(3000, 280));
        let read_file = tool(&scratch.0, "read_file");

        let window = read_file
            .invoke(json!({"path": "big.txt", "offset": 2000, "limit": 1}))
            .await
            .expect("read");
        let text = window.as_str().expect("string");
        assert!(text.starts_with("line 2000 "), "got: {text}");
    }

    #[tokio::test]
    async fn read_file_byte_cap_footer_names_a_working_resume_offset() {
        let scratch = Scratch::new("read-resume");
        scratch.write("big.txt", &numbered_lines(3000, 280));
        let read_file = tool(&scratch.0, "read_file");

        let first = read_file
            .invoke(json!({"path": "big.txt"}))
            .await
            .expect("read");
        let text = first.as_str().expect("string");
        // 616 lines fit (616 × 851 = 524 216); the 72-byte remainder cuts
        // line 617 mid-way, backed off to a char boundary.
        assert!(
            text.ends_with("… [capped at 512 KiB — resume with offset=617]"),
            "got tail: {}",
            &text[text.len() - 200..]
        );

        let resumed = read_file
            .invoke(json!({"path": "big.txt", "offset": 617, "limit": 1}))
            .await
            .expect("resume");
        let text = resumed.as_str().expect("string");
        assert!(text.starts_with("line 0617 "), "got: {text}");
    }

    #[tokio::test]
    async fn read_file_cap_landing_on_a_line_boundary_resumes_at_the_next_line() {
        let scratch = Scratch::new("read-exact-boundary");
        // 2048 B/line (11 + 3·679): 256 lines fill the 512 KiB cap to the
        // exact byte — the stop lands on a boundary, so no line is cut and
        // the resume offset is the next line, not a re-read.
        scratch.write("big.txt", &numbered_lines(300, 679));
        let read_file = tool(&scratch.0, "read_file");

        let first = read_file
            .invoke(json!({"path": "big.txt"}))
            .await
            .expect("read");
        let text = first.as_str().expect("string");
        assert!(
            text.ends_with("… [capped at 512 KiB — resume with offset=257]"),
            "got tail: {}",
            &text[text.len() - 100..]
        );
        assert!(
            !text.contains("[cut at 512 KiB]"),
            "a boundary stop must not cut a line, got: {text}"
        );

        let resumed = read_file
            .invoke(json!({"path": "big.txt", "offset": 257, "limit": 1}))
            .await
            .expect("resume");
        let text = resumed.as_str().expect("string");
        assert!(text.starts_with("line 0257 "), "got: {text}");
    }

    #[tokio::test]
    async fn read_file_marks_overlong_lines_and_continues() {
        let scratch = Scratch::new("read-long-line");
        let content = format!("{}\nsecond\n", "x".repeat(100_000));
        scratch.write("long.txt", &content);
        let read_file = tool(&scratch.0, "read_file");

        let result = read_file
            .invoke(json!({"path": "long.txt"}))
            .await
            .expect("read");
        let text = result.as_str().expect("string");
        assert!(text.contains("… [line cut at 64 KiB]"), "got: {text}");
        assert!(text.ends_with("second"), "got: {text}");
    }

    #[tokio::test]
    async fn read_file_multibyte_char_at_the_line_guard_is_cut_not_misreported() {
        let scratch = Scratch::new("read-guard-boundary");
        // The '€' straddles the 64 KiB guard: the clamp lands mid-character.
        // A valid file must yield a cut head, never a "not UTF-8" error.
        let content = format!("{}€{}", "a".repeat(65_535), "b".repeat(1_000));
        scratch.write("straddle.txt", &content);
        let read_file = tool(&scratch.0, "read_file");

        let result = read_file
            .invoke(json!({"path": "straddle.txt"}))
            .await
            .expect("valid UTF-8 must not error");
        let text = result.as_str().expect("string");
        assert!(text.contains("… [line cut at 64 KiB]"), "got: {text}");
    }

    #[tokio::test]
    async fn read_file_limit_at_eof_reports_the_window_without_a_footer() {
        let scratch = Scratch::new("read-limit-eof");
        scratch.write("three.txt", "one\ntwo\nthree\n");
        let read_file = tool(&scratch.0, "read_file");

        let result = read_file
            .invoke(json!({"path": "three.txt", "limit": 3}))
            .await
            .expect("read");
        // limit lands exactly on EOF: no "more lines follow" footer.
        assert_eq!(result, json!("one\ntwo\nthree"));
    }

    #[tokio::test]
    async fn read_file_out_of_range_window_names_the_valid_range() {
        let scratch = Scratch::new("read-range");
        scratch.write("small.txt", "one\ntwo\nthree\n");
        let read_file = tool(&scratch.0, "read_file");

        let result = read_file
            .invoke(json!({"path": "small.txt", "offset": 99}))
            .await
            .expect("read");
        let text = result.as_str().expect("string");
        assert!(text.contains("past the end"), "got: {text}");
        assert!(text.contains("3 lines"), "got: {text}");
    }

    #[tokio::test]
    async fn read_file_normalizes_crlf_and_reports_empty_files() {
        let scratch = Scratch::new("read-crlf");
        scratch.write("crlf.txt", "a\r\nb\r\n");
        scratch.write("crlf-unterminated.txt", "a\r\nb\r");
        scratch.write("empty.txt", "");
        let read_file = tool(&scratch.0, "read_file");

        let result = read_file
            .invoke(json!({"path": "crlf.txt"}))
            .await
            .expect("read");
        assert_eq!(result, json!("a\nb"));

        // Parity with `str::lines`: a lone trailing \r is content, not a
        // line terminator.
        let result = read_file
            .invoke(json!({"path": "crlf-unterminated.txt"}))
            .await
            .expect("read");
        assert_eq!(result, json!("a\nb\r"));

        let result = read_file
            .invoke(json!({"path": "empty.txt"}))
            .await
            .expect("read");
        assert!(result.as_str().expect("string").contains("0 lines"));
    }

    #[tokio::test]
    async fn read_file_rejects_invalid_offset_and_limit() {
        let scratch = Scratch::new("read-invalid-args");
        scratch.write("f.txt", "x\n");
        let read_file = tool(&scratch.0, "read_file");

        for args in [
            json!({"path": "f.txt", "offset": 0}),
            json!({"path": "f.txt", "offset": -1}),
            json!({"path": "f.txt", "limit": 0}),
            json!({"path": "f.txt", "limit": 1.5}),
        ] {
            let err = read_file
                .invoke(args)
                .await
                .expect_err("invalid window argument must be a tool error");
            assert!(err.message.contains("positive integer"), "got: {err}");
        }
    }
}
