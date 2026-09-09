use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use cadmus_contract::ToolSpec;
use cadmus_core::{AgentTool, Concurrency, Effect, ToolError};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::WalkBuilder;
use serde_json::{Value, json};

use super::{error, resolve};

const MAX_GREP_FILE_BYTES: u64 = 1024 * 1024;
const MAX_GREP_MATCHES: usize = 50;
/// Display budget for one matching line — the preview is a locator, not a
/// read; the full line stays reachable via `read_file` (ripgrep's
/// `--max-columns-preview` convention, without a column axis to resume).
const MAX_GREP_LINE_BYTES: usize = 512;

/// `grep`: regex search over the workspace, walking with ripgrep's rules —
/// gitignore-aware, hidden entries skipped, symlinks never followed.
pub(super) struct Grep {
    pub(super) root: PathBuf,
}

#[async_trait]
impl AgentTool for Grep {
    /// Read-only and stateless: parallel-safe like every perception tool
    /// (ADR-0008 item 2).
    fn concurrency(&self) -> Concurrency {
        Concurrency::ParallelSafe
    }

    /// Reads only: never gated (ADR-0008 item 4).
    fn effect(&self) -> Effect {
        Effect::Perception
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: "Search workspace files with a regular expression (ripgrep/Rust syntax, \
                          Unicode-aware, smart case: all-lowercase patterns ignore case, any \
                          uppercase forces case-sensitive — ripgrep's -S rule). Use this to \
                          find which files to read; \
                          then use read_file to view them. Recursive from path (default: workspace \
                          root); naming a single file searches just it, bypassing the rules below. \
                          Directory searches respect .gitignore (even outside git repositories) and \
                          skip hidden entries, symlinked paths (never followed), binary/non-UTF-8 \
                          files and files over 1 MiB; every skip category is counted in the \
                          footer. Returns `path:line: text`, at most 50 matches — when capped, the \
                          footer names the file the search stopped in, so narrow the pattern or \
                          path to see the rest. Matching lines are previewed to 512 B."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "regular expression (ripgrep syntax, e.g. `fn\\s+\\w+`); invalid patterns return the syntax error"},
                    "path": {"type": "string", "description": "directory or file to search (default: workspace root)"},
                },
                "required": ["pattern"],
            }),
        }
    }

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
        let pattern = arguments["pattern"].as_str().unwrap_or_default();
        if pattern.is_empty() {
            return Err(error("grep", "pattern must not be empty".into()));
        }
        // Smart case (ripgrep's -S rule), forgiving recall by default:
        // an all-lowercase pattern matches case-insensitively, any
        // uppercase letter forces an exact match.
        let regex = RegexMatcherBuilder::new()
            .case_smart(true)
            .build(pattern)
            .map_err(|err| error("grep", format!("invalid regex `{pattern}`: {err}")))?;
        let base = arguments["path"].as_str().unwrap_or(".");
        let canonical = resolve(&self.root, base).map_err(|message| error("grep", message))?;

        let mut files = Vec::new();
        if canonical.is_file() {
            // An explicitly named file is always searched (ripgrep's rule:
            // ignore/hidden policy applies to traversal, not to the file you
            // pointed at). Confinement already ran in `resolve`.
            files.push(canonical.clone());
        } else if canonical.is_dir() {
            collect_files(&self.root, &canonical, &mut files);
        } else {
            return Err(error(
                "grep",
                format!("`{base}` is neither a file nor a directory"),
            ));
        }

        let mut matches = Vec::new();
        let mut skipped_binary = 0usize;
        let mut skipped_large = 0usize;
        let mut stopped_in = None;
        // BOM sniffing stays OFF: the default would transcode BOM-tagged
        // UTF-16 to UTF-8 in flight, sneaking a non-UTF-8 file past both the
        // binary count and the sink's UTF-8 check (and diverging from
        // read_file, which rejects the same file). Off, UTF-16's NULs hit
        // binary detection honestly; a UTF-8 BOM simply shows, as in
        // read_file.
        let mut searcher = SearcherBuilder::new()
            .binary_detection(BinaryDetection::quit(b'\x00'))
            .bom_sniffing(false)
            .line_number(true)
            .build();

        for file in &files {
            let Ok(metadata) = fs::metadata(file) else {
                continue;
            };
            if metadata.len() > MAX_GREP_FILE_BYTES {
                skipped_large += 1;
                continue;
            }

            // Agent-facing paths use `/` on every OS (`Path::display` would
            // emit `\` on Windows).
            let display = file
                .strip_prefix(&self.root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            let mut sink = MatchSink {
                display: &display,
                matches: &mut matches,
                binary: false,
            };
            let found = searcher.search_path(&regex, file, &mut sink);
            // A search error here is almost always non-UTF-8 bytes (the sink
            // rejects them); matches already collected from the file stand.
            if found.is_err() || sink.binary {
                skipped_binary += 1;
            }
            if matches.len() >= MAX_GREP_MATCHES {
                stopped_in = Some(display);
                break;
            }
        }

        let mut output = if matches.is_empty() {
            format!("no matches for `{pattern}`")
        } else {
            matches.join("\n")
        };
        if let Some(stop) = stopped_in {
            let _ = write!(
                output,
                "\n… [stopped at {MAX_GREP_MATCHES} matches in `{stop}` — narrow the pattern or \
                 path to see more]"
            );
        }
        let mut skipped = Vec::new();
        if skipped_binary > 0 {
            skipped.push(format!("{skipped_binary} binary or non-UTF-8"));
        }
        if skipped_large > 0 {
            skipped.push(format!("{skipped_large} over 1 MiB"));
        }
        if !skipped.is_empty() {
            let _ = write!(output, "\n… [skipped: {}]", skipped.join(", "));
        }
        Ok(Value::String(output))
    }
}

/// Collects one file's matches. UTF-8 validation happens here per matched
/// line (the matcher runs on bytes); binary detection is the searcher's, and
/// `binary_data` observing it is what lets the footer count NUL-containing
/// files — there is no separate sniff to drift out of sync.
struct MatchSink<'a> {
    display: &'a str,
    matches: &'a mut Vec<String>,
    binary: bool,
}

impl Sink for MatchSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        let line = std::str::from_utf8(mat.bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "not a UTF-8 text file")
        })?;
        let line_number = mat.line_number().expect("line numbers enabled");
        self.matches.push(format!(
            "{}:{line_number}: {}",
            self.display,
            preview(trim_line_end(line))
        ));
        Ok(self.matches.len() < MAX_GREP_MATCHES)
    }

    fn binary_data(&mut self, _: &Searcher, _: u64) -> Result<bool, Self::Error> {
        self.binary = true;
        Ok(false)
    }
}

/// Collects searchable files under `base` by walking the workspace ROOT, so
/// ignore rules are always evaluated relative to the root (a .gitignore at
/// the root applies to every subdirectory search) and no walk state ever
/// leaks above it. Sorted for deterministic output.
fn collect_files(root: &Path, base: &Path, files: &mut Vec<PathBuf>) {
    let mut walker = WalkBuilder::new(root);
    // The non-default knobs, pinned for determinism and confinement: no
    // symlink following (a symlinked dir must not escape the workspace), no
    // machine-dependent global excludes, no ancestor .gitignore above the
    // root, and .gitignore applies even outside git repositories.
    walker
        .follow_links(false)
        .git_global(false)
        .parents(false)
        .require_git(false);
    for entry in walker.build().flatten() {
        let path = entry.path();
        if entry.file_type().is_some_and(|kind| kind.is_file()) && path.starts_with(base) {
            files.push(path.to_path_buf());
        }
    }
    files.sort();
}

/// `str::lines` terminator semantics: one trailing `\n`, then one `\r`.
fn trim_line_end(line: &str) -> &str {
    line.strip_suffix('\n')
        .map_or(line, |l| l.strip_suffix('\r').unwrap_or(l))
}

/// One matching line's display form: whole, or a char-boundary-aligned head
/// with an inline cut marker.
fn preview(line: &str) -> String {
    if line.len() <= MAX_GREP_LINE_BYTES {
        return line.to_owned();
    }
    let mut end = MAX_GREP_LINE_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} … [line cut at {MAX_GREP_LINE_BYTES} B]", &line[..end])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::tests::{Scratch, tool};

    #[tokio::test]
    async fn grep_respects_gitignore_and_returns_sorted_matches() {
        let scratch = Scratch::new("grep-gitignore");
        scratch.write(".gitignore", "target/\n");
        scratch.write("a.rs", "let x = 1;\nlet y = 2;\n");
        scratch.write("sub/b.rs", "let x = 3;\n");
        scratch.write("target/c.rs", "let x = 4;\n");
        let grep = tool(&scratch.0, "grep");

        let result = grep
            .invoke(json!({"pattern": "let x"}))
            .await
            .expect("grep");
        let text = result.as_str().expect("string");
        assert_eq!(text, "a.rs:1: let x = 1;\nsub/b.rs:1: let x = 3;");
    }

    #[tokio::test]
    async fn grep_searches_a_subdirectory_with_root_gitignore_rules() {
        let scratch = Scratch::new("grep-subdir");
        // The root .gitignore must apply to a subdirectory search: the walk
        // starts at the root and filters to the base, never the reverse.
        scratch.write(".gitignore", "*.log\n");
        scratch.write("root.rs", "needle\n");
        scratch.write("sub/keep.rs", "needle\n");
        scratch.write("sub/skip.log", "needle\n");
        let grep = tool(&scratch.0, "grep");

        let result = grep
            .invoke(json!({"pattern": "needle", "path": "sub"}))
            .await
            .expect("grep");
        assert_eq!(result, json!("sub/keep.rs:1: needle"));
    }

    #[tokio::test]
    async fn grep_supports_regex_and_rejects_invalid_patterns() {
        let scratch = Scratch::new("grep-regex");
        scratch.write("a.rs", "fn main() {}\nlet x = 1;\nstruct foo;\n");
        let grep = tool(&scratch.0, "grep");

        let result = grep
            .invoke(json!({"pattern": r"fn\s+\w+|struct\s+\w+"}))
            .await
            .expect("regex");
        let text = result.as_str().expect("string");
        assert_eq!(text, "a.rs:1: fn main() {}\na.rs:3: struct foo;");

        let err = grep
            .invoke(json!({"pattern": "(unclosed"}))
            .await
            .expect_err("invalid regex must be a tool error");
        assert!(err.message.contains("invalid regex"), "got: {err}");
    }

    #[tokio::test]
    async fn grep_smart_case_matches_like_ripgrep() {
        let scratch = Scratch::new("grep-smart-case");
        scratch.write("f.rs", "needle\nNeedle\nNEEDLE\n");
        let grep = tool(&scratch.0, "grep");

        // All-lowercase: insensitive, all three casings match.
        let result = grep
            .invoke(json!({"pattern": "needle"}))
            .await
            .expect("grep");
        assert_eq!(
            result,
            json!("f.rs:1: needle\nf.rs:2: Needle\nf.rs:3: NEEDLE")
        );

        // Any uppercase: sensitive, only the exact casing matches.
        let result = grep
            .invoke(json!({"pattern": "Needle"}))
            .await
            .expect("grep");
        assert_eq!(result, json!("f.rs:2: Needle"));
    }

    #[tokio::test]
    async fn grep_skips_hidden_entries() {
        let scratch = Scratch::new("grep-hidden");
        scratch.write(".hidden.rs", "needle\n");
        scratch.write(".git/config", "needle\n");
        scratch.write("visible.rs", "needle\n");
        let grep = tool(&scratch.0, "grep");

        let result = grep
            .invoke(json!({"pattern": "needle"}))
            .await
            .expect("grep");
        let text = result.as_str().expect("string");
        assert_eq!(text, "visible.rs:1: needle");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn grep_never_descends_symlinked_dirs() {
        let scratch = Scratch::new("grep-symlink");
        let outside = Scratch::new("grep-symlink-outside");
        outside.write("secret.txt", "needle\n");
        std::os::unix::fs::symlink(&outside.0, scratch.0.join("linked")).expect("symlink");
        let grep = tool(&scratch.0, "grep");

        let result = grep
            .invoke(json!({"pattern": "needle"}))
            .await
            .expect("grep");
        let text = result.as_str().expect("string");
        assert!(
            text.starts_with("no matches"),
            "symlink escape must find nothing, got: {text}"
        );
    }

    #[tokio::test]
    async fn grep_searches_an_explicitly_named_file() {
        let scratch = Scratch::new("grep-explicit-file");
        // Gitignored on purpose: an explicitly named file is searched anyway
        // (ripgrep's rule — the ignore policy governs traversal, not the
        // file you pointed at).
        scratch.write(".gitignore", "target/\n");
        scratch.write("target/c.rs", "let x = 4;\n");
        let grep = tool(&scratch.0, "grep");

        let result = grep
            .invoke(json!({"pattern": "let x", "path": "target/c.rs"}))
            .await
            .expect("grep");
        let text = result.as_str().expect("string");
        assert_eq!(text, "target/c.rs:1: let x = 4;");
    }

    #[tokio::test]
    async fn grep_reports_skipped_binary_and_large_files() {
        let scratch = Scratch::new("grep-skips");
        scratch.write_bytes("binary.bin", b"needle\0binary\n");
        // A NUL at byte 128 KiB + 7, past any head-sized buffer: the first
        // fill is clean, so `needle` matches and is kept — then detection
        // fires on a later fill and the file is still counted as binary.
        let mut tail_binary = format!("needle\n{}", "x".repeat(128 * 1024)).into_bytes();
        tail_binary.push(0);
        // A match AFTER the NUL must never be reported: detection stops the
        // search, not just the counting.
        tail_binary.extend_from_slice(b"\nneedle-after\n");
        scratch.write_bytes("tail.bin", &tail_binary);
        // BOM-tagged UTF-16LE: with BOM sniffing off this is not transcoded
        // in flight — its NULs trip binary detection and it is counted.
        scratch.write_bytes("utf16.bin", b"\xff\xfen\x00e\x00e\x00d\x00l\x00e\x00");
        let large = format!("{}\nneedle\n", "x".repeat(1024 * 1024));
        scratch.write("large.txt", &large);
        scratch.write("small.txt", "needle\n");
        let grep = tool(&scratch.0, "grep");

        let result = grep
            .invoke(json!({"pattern": "needle"}))
            .await
            .expect("grep");
        let text = result.as_str().expect("string");
        assert!(text.starts_with("small.txt:1: needle"), "got: {text}");
        assert!(text.contains("tail.bin:1: needle"), "got: {text}");
        assert!(!text.contains("needle-after"), "got: {text}");
        assert!(
            text.contains("skipped: 3 binary or non-UTF-8, 1 over 1 MiB"),
            "got: {text}"
        );
    }

    #[tokio::test]
    async fn grep_counts_non_utf8_files_as_binary_and_keeps_partial_matches() {
        let scratch = Scratch::new("grep-non-utf8");
        // The invalid bytes sit on a MATCHING line: the per-line UTF-8
        // check in the sink is what turns the file into a skip.
        scratch.write_bytes("bad.txt", b"needle\nneedle \xff\xfe\n");
        scratch.write("good.txt", "needle\n");
        let grep = tool(&scratch.0, "grep");

        let result = grep
            .invoke(json!({"pattern": "needle"}))
            .await
            .expect("grep");
        let text = result.as_str().expect("string");
        assert!(text.contains("bad.txt:1: needle"), "got: {text}");
        assert!(text.contains("good.txt:1: needle"), "got: {text}");
        assert!(
            text.contains("skipped: 1 binary or non-UTF-8"),
            "got: {text}"
        );
    }

    #[tokio::test]
    async fn grep_cap_footer_names_the_file_the_search_stopped_in() {
        let scratch = Scratch::new("grep-cap");
        for n in 0..60 {
            scratch.write(&format!("m{n:02}.rs"), "needle\n");
        }
        let grep = tool(&scratch.0, "grep");

        let result = grep
            .invoke(json!({"pattern": "needle"}))
            .await
            .expect("grep");
        let text = result.as_str().expect("string");
        assert_eq!(text.lines().count(), 51); // 50 matches + the cap footer
        assert!(
            text.contains("stopped at 50 matches in `m49.rs`"),
            "got: {text}"
        );
    }

    #[tokio::test]
    async fn grep_previews_long_match_lines_at_the_budget() {
        let scratch = Scratch::new("grep-long-line");
        let content = format!("{}needle{}\n", "x".repeat(1_000), "y".repeat(1_000));
        scratch.write("long.txt", &content);
        let grep = tool(&scratch.0, "grep");

        let result = grep
            .invoke(json!({"pattern": "needle"}))
            .await
            .expect("grep");
        let text = result.as_str().expect("string");
        let [line] = text.lines().collect::<Vec<_>>()[..] else {
            panic!("one preview line expected, got: {text}");
        };
        assert!(line.starts_with("long.txt:1: xxx"), "got: {line}");
        assert!(line.ends_with("… [line cut at 512 B]"), "got: {line}");
    }

    #[tokio::test]
    async fn grep_preview_cut_backs_off_to_a_char_boundary() {
        let scratch = Scratch::new("grep-long-multibyte");
        // '€' is 3 bytes: the 512 B budget lands mid-character. A naive
        // slice would panic; the cut must back off to the boundary.
        let content = format!("{}needle\n", "€".repeat(300));
        scratch.write("mb.txt", &content);
        let grep = tool(&scratch.0, "grep");

        let result = grep
            .invoke(json!({"pattern": "needle"}))
            .await
            .expect("grep");
        let text = result.as_str().expect("string");
        assert!(text.ends_with("… [line cut at 512 B]"), "got: {text}");
    }

    #[tokio::test]
    async fn grep_errors_on_a_missing_path() {
        let scratch = Scratch::new("grep-missing");
        scratch.write("f.txt", "x\n");
        let grep = tool(&scratch.0, "grep");

        let err = grep
            .invoke(json!({"pattern": "x", "path": "nope"}))
            .await
            .expect_err("missing path must be a tool error");
        assert!(
            err.message.contains("neither a file nor a directory"),
            "got: {err}"
        );
    }
}
