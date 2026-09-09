//! Unified-diff rendering for the write tools' results (ADR-0008 item 5,
//! execution-verification feedback). The model already holds the content it
//! sent, so the diff's added value is what the write changed *against the
//! pre-existing bytes*: placement (`@@` headers), surviving context, and —
//! on overwrite — what was lost. Rendering is `similar`'s; nothing here
//! re-implements diff plumbing.

use similar::TextDiff;

/// Diff output budget: feedback for a glance, not a file copy. Past the
/// budget the diff is cut at a line boundary with an explicit marker —
/// silent truncation is a defect (ADR-0007 layer 1).
const MAX_DIFF_BYTES: usize = 8 * 1024;

/// A capped unified diff of `old` → `new` for `path` — `None` when the two
/// are identical (a net-zero change reports itself, never a diff).
pub(super) fn unified_diff(path: &str, old: &str, new: &str) -> Option<String> {
    if old == new {
        return None;
    }
    let old_name = format!("a/{path}");
    let new_name = format!("b/{path}");
    let diff = TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&old_name, &new_name)
        .to_string();
    Some(cap(diff))
}

/// Cuts at the last line boundary within the budget and appends the marker.
/// The byte index is walked back to a char boundary first — a multibyte
/// character at the cut point is a wrong-boundary panic otherwise.
fn cap(diff: String) -> String {
    if diff.len() <= MAX_DIFF_BYTES {
        return diff;
    }
    let mut boundary = MAX_DIFF_BYTES;
    while !diff.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let cut = diff[..boundary].rfind('\n').map_or(boundary, |i| i + 1);
    format!(
        "{}[diff truncated — the file on disk is the full truth; re-read it with read_file]\n",
        &diff[..cut]
    )
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    #[test]
    fn identical_contents_render_no_diff() {
        assert_eq!(unified_diff("a.txt", "same\n", "same\n"), None);
    }

    #[test]
    fn a_small_diff_carries_placement_and_context() {
        let diff =
            unified_diff("a.txt", "alpha\nbeta\ngamma\n", "alpha\nBETA\ngamma\n").expect("changed");
        assert!(diff.contains("@@"), "got: {diff}");
        assert!(diff.contains("-beta"), "got: {diff}");
        assert!(diff.contains("+BETA"), "got: {diff}");
        // Surviving neighbors appear as context.
        assert!(diff.contains(" alpha"), "got: {diff}");
        assert!(diff.contains(" gamma"), "got: {diff}");
    }

    #[test]
    fn an_oversized_diff_is_cut_at_a_line_boundary_with_a_marker() {
        let old = (1..=2000).fold(String::new(), |mut acc, n| {
            let _ = writeln!(acc, "line {n} old");
            acc
        });
        let new = old.replace("old", "new");
        let diff = unified_diff("big.txt", &old, &new).expect("changed");

        assert!(diff.len() < MAX_DIFF_BYTES + 128, "len: {}", diff.len());
        assert!(
            diff.contains("[diff truncated"),
            "got tail: {:?}",
            &diff[diff.len().saturating_sub(200)..]
        );
        // The cut respected line structure: the marker starts on its own line.
        let marker = diff.find("[diff truncated").expect("marker");
        assert_eq!(&diff[marker - 1..marker], "\n");
    }

    #[test]
    fn multibyte_content_at_the_cut_point_does_not_panic() {
        // 3-byte characters straddling the budget boundary.
        let old = "€".repeat(MAX_DIFF_BYTES);
        let new = old.replace('€', "£");
        let diff = unified_diff("mb.txt", &old, &new).expect("changed");
        assert!(diff.contains("[diff truncated"));
    }
}
