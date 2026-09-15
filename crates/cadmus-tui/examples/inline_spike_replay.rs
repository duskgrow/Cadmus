//! Replay analyzer for `inline_spike` captures: turns a real-terminal
//! matrix run into machine-checked evidence, so the verdict never depends
//! on an eyewitness description.
//!
//! Input: a capture stem (`target/inline-spike/capture-<ts>`) produced by an
//! `inline_spike` run — `<stem>.bin` is the raw output stream, `<stem>.txt`
//! the sidecar (initial size, identity, resize events with stream offsets,
//! key log, expected final row sequence).
//!
//! Checks, all mechanical:
//!
//! 1. Replay the byte stream through vt100 (applying resize events at their
//!    logged offsets) and diff the final scrollback+screen against the
//!    sidecar's expected rows — any lost/duplicated/garbage row shows up as
//!    a missing/extra entry, attributable via the sidecar stats (shrink
//!    replays).
//! 2. Count the 2026h synchronized-update guards: balanced begin/end pairs.
//! 3. Sentinel check: every height key (`g`/`s`) must be bracketed by `x`
//!    presses — a missing sentinel means the CPR race (upstream #2640)
//!    swallowed input around a Terminal recreation.
//!
//! Usage: `cargo run -p cadmus-tui --example inline_spike_replay -- <stem>`
//!
//! What this cannot judge is compositing (flicker lives in the terminal,
//! not in the byte stream): on a terminal that ignores 2026h, a height
//! change flashes one frame — a bounded, accepted residual identical for
//! every implementation including a fork (ADR-0018, second 2026-09-14
//! amendment). The `G`/`f`/`F` failure-reference keys retired with the
//! verdict; their evidence is frozen in
//! `docs/research/2026-09-14-terminal-recreation-spike.md` §7.5.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;

/// Replay scrollback must outlast the whole session or the oldest rows fall
/// off the emulated terminal and masquerade as "missing" (first long tmux
/// soak: 23k rows through a 5k scrollback). Sized from the sidecar's own
/// expected-row count — the sidecar always covers the full session.
fn scrollback_len(sidecar: &Sidecar) -> usize {
    sidecar.expected.len() + 100
}

struct Sidecar {
    identity: String,
    rows: u16,
    cols: u16,
    resizes: Vec<(u64, u16, u16)>,
    keys: Vec<String>,
    stats: String,
    expected: Vec<String>,
}

fn parse_sidecar(text: &str) -> Result<Sidecar, String> {
    let mut identity = None;
    let mut size = None;
    let mut resizes = Vec::new();
    let mut keys = Vec::new();
    let mut stats = String::new();
    let mut lines = text.lines();
    if lines.next() != Some("cadmus-inline-spike-capture v1") {
        return Err("not a v1 capture sidecar".to_string());
    }
    for line in lines.by_ref() {
        if line == "expected:" {
            break;
        } else if let Some(rest) = line.strip_prefix("identity: ") {
            identity = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("size: ") {
            let mut parts = rest.split(' ');
            let (Some(rows), Some(cols)) = (parts.next(), parts.next()) else {
                return Err(format!("malformed size line: {line}"));
            };
            size = Some((
                rows.parse::<u16>().map_err(|e| e.to_string())?,
                cols.parse::<u16>().map_err(|e| e.to_string())?,
            ));
        } else if let Some(rest) = line.strip_prefix("resize: ") {
            let mut parts = rest.split(' ');
            let (Some(offset), Some(rows), Some(cols)) = (parts.next(), parts.next(), parts.next())
            else {
                return Err(format!("malformed resize line: {line}"));
            };
            resizes.push((
                offset.parse::<u64>().map_err(|e| e.to_string())?,
                rows.parse::<u16>().map_err(|e| e.to_string())?,
                cols.parse::<u16>().map_err(|e| e.to_string())?,
            ));
        } else if let Some(rest) = line.strip_prefix("keys: ") {
            keys = rest.split(' ').map(str::to_string).collect();
        } else if let Some(rest) = line.strip_prefix("stats: ") {
            stats = rest.to_string();
        } else {
            return Err(format!("unrecognized sidecar line: {line}"));
        }
    }
    let expected: Vec<String> = lines
        .map(|row| row.trim_end().to_string())
        .filter(|row| !row.is_empty())
        .collect();
    Ok(Sidecar {
        identity: identity.ok_or("missing identity line")?,
        rows: size.ok_or("missing size line")?.0,
        cols: size.ok_or("missing size line")?.1,
        resizes,
        keys,
        stats,
        expected,
    })
}

/// Duplicated from `tests/dynamic_height_spike.rs` (the two rigs share row
/// readers, not backends — cross-reference if one side changes).
fn nonblank_rows(parser: &mut vt100::Parser) -> Vec<String> {
    let screen = parser.screen_mut();
    screen.set_scrollback(usize::MAX);
    let depth = screen.scrollback();
    let mut rows = Vec::with_capacity(depth);
    let mut start = 0;
    while start < depth {
        screen.set_scrollback(depth - start);
        let (rows_count, cols) = screen.size();
        let take = (depth - start).min(usize::from(rows_count));
        rows.extend(
            screen
                .rows(0, cols)
                .take(take)
                .map(|row| row.trim_end().to_string()),
        );
        start += take;
    }
    screen.set_scrollback(0);
    let (_, cols) = screen.size();
    rows.extend(screen.rows(0, cols).map(|row| row.trim_end().to_string()));
    rows.into_iter().filter(|row| !row.is_empty()).collect()
}

fn replay(bytes: &[u8], sidecar: &Sidecar) -> (Vec<String>, usize, usize) {
    let mut parser = vt100::Parser::new(sidecar.rows, sidecar.cols, scrollback_len(sidecar));
    let mut pos = 0_usize;
    for &(offset, rows, cols) in &sidecar.resizes {
        let upto = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        parser.process(&bytes[pos..upto]);
        parser.screen_mut().set_size(rows, cols);
        pos = upto;
    }
    parser.process(&bytes[pos..]);
    let begins = count_occurrences(bytes, b"\x1b[?2026h");
    let ends = count_occurrences(bytes, b"\x1b[?2026l");
    (nonblank_rows(&mut parser), begins, ends)
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// Multiset difference: rows in `expected` but short in `actual` (missing)
/// and rows in `actual` beyond `expected` (extra).
fn multiset_diff(expected: &[String], actual: &[String]) -> (Vec<String>, Vec<String>) {
    let mut balance: BTreeMap<&str, i64> = BTreeMap::new();
    for row in expected {
        *balance.entry(row.as_str()).or_default() += 1;
    }
    for row in actual {
        *balance.entry(row.as_str()).or_default() -= 1;
    }
    let expand = |sign: i64| {
        balance
            .iter()
            .filter(move |(_, count)| count.signum() == sign)
            .flat_map(|(row, count)| {
                vec![(*row).to_string(); usize::try_from(count.unsigned_abs()).unwrap_or(0)]
            })
            .collect::<Vec<_>>()
    };
    (expand(1), expand(-1))
}

const SENTINEL: &str = "Char('x')";
const HEIGHT_KEYS: &[&str] = &["Char('g')", "Char('s')"];

/// Every height key must be bracketed by sentinels; a missing one means the
/// CPR race may have swallowed a keypress around a Terminal recreation.
fn sentinel_violations(keys: &[String]) -> Vec<String> {
    keys.iter()
        .enumerate()
        .filter(|(_, key)| HEIGHT_KEYS.contains(&key.as_str()))
        .filter(|(i, _)| {
            i.checked_sub(1)
                .and_then(|prev| keys.get(prev))
                .map(String::as_str)
                != Some(SENTINEL)
                || keys.get(i + 1).map(String::as_str) != Some(SENTINEL)
        })
        .map(|(i, key)| format!("{key} at position {i}"))
        .collect()
}

fn run(stem: &str) -> io::Result<()> {
    let bin = fs::read(format!("{stem}.bin"))?;
    let text = fs::read_to_string(format!("{stem}.txt"))?;
    let sidecar = parse_sidecar(&text).map_err(io::Error::other)?;
    let (actual, begins, ends) = replay(&bin, &sidecar);
    println!("capture: {stem} ({} bytes)", bin.len());
    println!("identity: {}", sidecar.identity);
    println!(
        "screen: {}x{}, resize events applied: {}",
        sidecar.rows,
        sidecar.cols,
        sidecar.resizes.len()
    );
    let balance = if begins == ends {
        "balanced"
    } else {
        "UNBALANCED"
    };
    println!("2026h guards: {begins} begin / {ends} end ({balance})");
    println!("key log: {}", sidecar.keys.join(" "));
    let violations = sentinel_violations(&sidecar.keys);
    if violations.is_empty() {
        println!("sentinel check: ok (every height key bracketed by x)");
    } else {
        println!("sentinel check: VIOLATIONS — {}", violations.join(", "));
    }
    let (missing, extra) = multiset_diff(&sidecar.expected, &actual);
    if missing.is_empty() && extra.is_empty() {
        println!("final state: identical to the sidecar model");
    } else {
        println!("final state: DIFFERS from the sidecar model");
        for row in &missing {
            println!("  missing: {row}");
        }
        for row in &extra {
            println!("  extra:   {row}");
        }
    }
    println!("attribution (sidecar stats): {}", sidecar.stats);
    println!(
        "note: extra rows are expected only from shrink replays; missing rows are never\n\
         expected. Compositing (flicker) is judged live: a flash on g/s means the\n\
         terminal ignores 2026h — the bounded accepted residual (ADR-0018 amendment)."
    );
    Ok(())
}

fn main() -> io::Result<()> {
    let Some(stem) = env::args().nth(1) else {
        eprintln!("usage: inline_spike_replay <capture-stem> (without extension)");
        std::process::exit(2);
    };
    run(&stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIDECAR: &str = "cadmus-inline-spike-capture v1\n\
        identity: TERM=fixture\n\
        size: 24 80\n\
        stats: turns 0 inserts 0 rows 0 resizes 0 shrink_replays 0 grows 0 shrinks 0 resize_errors 0 draw_errors 0\n\
        keys: Char('x') Char('g') Char('x')\n\
        expected:\n\
        alpha\n\
        beta\n\
        gamma\n\
        STATUS\n\
        PROMPT\n";

    fn fixture_bytes() -> Vec<u8> {
        let mut out = String::from("alpha\r\nbeta\r\ngamma\r\n");
        out.push_str("\x1b[?2026h\x1b[20;1HSTATUS\x1b[21;1HPROMPT\x1b[?2026l");
        out.into_bytes()
    }

    #[test]
    fn clean_capture_analyzes_identical() {
        let sidecar = parse_sidecar(SIDECAR).expect("parse sidecar");
        let (actual, begins, ends) = replay(&fixture_bytes(), &sidecar);
        assert_eq!(actual, sidecar.expected);
        assert_eq!((begins, ends), (1, 1));
        assert!(sentinel_violations(&sidecar.keys).is_empty());
    }

    #[test]
    fn a_missing_final_draw_is_detected() {
        let sidecar = parse_sidecar(SIDECAR).expect("parse sidecar");
        // Drop the PROMPT draw: the model still expects it.
        let bytes = b"alpha\r\nbeta\r\ngamma\r\n\x1b[?2026h\x1b[20;1HSTATUS\x1b[?2026l".to_vec();
        let (actual, _, _) = replay(&bytes, &sidecar);
        let (missing, extra) = multiset_diff(&sidecar.expected, &actual);
        assert_eq!(missing, ["PROMPT"]);
        assert!(extra.is_empty());
    }

    #[test]
    fn an_unbracketed_height_key_is_flagged() {
        let keys = vec!["Char('g')".to_string(), "Char('x')".to_string()];
        assert_eq!(sentinel_violations(&keys).len(), 1);
    }
}
