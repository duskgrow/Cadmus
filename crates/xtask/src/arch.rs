//! Architecture test (ADR-0002: "dependency direction is enforced by an xtask
//! architecture test in CI"): std-only text scanning — a deliberate deviation
//! from report §9.1.2's `cargo_metadata` + `syn`, forced by xtask's
//! zero-dependency policy (ADR-0005 §6, amended 2026-09-05 from a per-crate
//! name whitelist to forbidden edges: routine dependency churn had to edit
//! the table without any real decision happening, and a whitelist left out
//! of sync on removals degrades to forbidden edges anyway). Three invariants
//! fail the build:
//!
//! 1. **Dependency direction** — forbidden edges over the workspace's
//!    internal crates: the contract depends on no internal crate and its
//!    third-party deps are a closed set (every workspace crate links it
//!    transitively, so a new entry fans out everywhere and is never a
//!    routine decision), the core takes the contract alone, adapters take
//!    the contract alone (plus the core's test fakes in dev-dependencies);
//!    the binary wires everything. The
//!    internal-crate set comes from the directory scan, so new crates
//!    default to the adapter posture and dependency churn never touches this
//!    file — only a new *kind* of crate does. The core additionally carries
//!    a deliberately short tripwire list of runtime/IO crates: "pure logic,
//!    no IO" can't be expressed as edges, and the list is known-incomplete —
//!    the real guarantee is that adapters already own all IO.
//! 2. **Version SSOT** — members inherit every dependency via
//!    `workspace = true`; a direct version in a member manifest means the
//!    version escaped the root `[workspace.dependencies]` (CONTRIBUTING.md
//!    "Adding a dependency").
//! 3. **Serialization boundary** — `cadmus-contract` is the only crate where
//!    serializable wire types live (report §9.1.1): a `Serialize` /
//!    `Deserialize` derive anywhere else fails. The scan follows derive
//!    attributes across rustfmt's line wrapping; doc comments and prose are
//!    not code and never match.
//!
//! Text scanning is convention-dependent matching: quoted TOML keys
//! (`"cadmus-core".workspace = true`), aliased derive imports
//! (`use serde::Serialize as S`), `cfg_attr`-gated derives and hand-written
//! `impl Serialize for` all evade it — the accepted price of std-only
//! (ADR-0005). The tripwire guards the conventional spellings;
//! unconventional spellings are review's job.
//!
//! The four named postures (binary, contract, core, xtask) are fused against
//! the directory scan: renaming or removing one fails instead of silently
//! dropping its rule to the adapter default. Scanning fails closed: an
//! unparseable dependency section header is a failure, never a silent skip.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The repository root is baked in at compile time, so the check works from
/// any working directory (hooks, CI jobs and the bootstrap app alike).
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// One crate's posture toward dependencies. `deps_internal` / `dev_internal`
/// are the workspace members allowed in `[dependencies]` /
/// `[dev-dependencies]`; `None` means unconstrained (the binary wires
/// everything).
struct Posture {
    deps_internal: Option<&'static [&'static str]>,
    dev_internal: Option<&'static [&'static str]>,
    third_party: ThirdParty,
}

enum ThirdParty {
    /// Adapters and the binary own their IO — unconstrained.
    Any,
    /// xtask's zero-dependency policy.
    None,
    /// The core's "pure logic, no IO" tripwire (module doc item 1). Applies
    /// to `[dependencies]` only — dev-dependencies legitimately run the
    /// tokio test runtime.
    Forbid(&'static [&'static str]),
    /// The contract's closed set (module doc item 1). Applies to both
    /// sections — a new dev tool there fans out exactly the same.
    Only(&'static [&'static str]),
}

/// The dependency-direction table (ADR-0002). Everything not named is an
/// adapter: internal deps limited to the contract, plus the core's replay
/// fakes in dev-dependencies.
fn posture_of(name: &str) -> Posture {
    match name {
        // The binary wires the ports to the adapters.
        "cadmus" => Posture {
            deps_internal: None,
            dev_internal: None,
            third_party: ThirdParty::Any,
        },
        // The contract depends on nothing internal, so the direction can
        // never invert — and its third-party set is closed because every
        // workspace crate links it transitively.
        "cadmus-contract" => Posture {
            deps_internal: Some(&[]),
            dev_internal: Some(&[]),
            third_party: ThirdParty::Only(&[
                "async-trait",
                "insta",
                "serde",
                "serde_json",
                "thiserror",
                "tokio-stream",
            ]),
        },
        // Pure logic, no IO: the contract is the only internal edge, and the
        // tripwire list guards the "just add a runtime quickly" shortcut.
        "cadmus-core" => Posture {
            deps_internal: Some(&["cadmus-contract"]),
            dev_internal: Some(&["cadmus-contract"]),
            third_party: ThirdParty::Forbid(&["genai", "reqwest", "rusqlite", "tokio"]),
        },
        // Zero third-party dependencies, by policy.
        "xtask" => Posture {
            deps_internal: Some(&[]),
            dev_internal: Some(&[]),
            third_party: ThirdParty::None,
        },
        _ => Posture {
            deps_internal: Some(&["cadmus-contract"]),
            dev_internal: Some(&["cadmus-contract", "cadmus-core"]),
            third_party: ThirdParty::Any,
        },
    }
}

/// Postures pinned by name — renaming or removing one of these crates must
/// fail loudly instead of silently dropping its rule to the adapter default.
const NAMED_POSTURES: [&str; 4] = ["cadmus", "cadmus-contract", "cadmus-core", "xtask"];

/// Entry point: `arch-test` (no arguments).
pub fn run(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        eprintln!("usage: arch-test  (takes no arguments)");
        return ExitCode::from(2);
    }
    let root = Path::new(ROOT);
    let mut failures: Vec<String> = Vec::new();
    check_tree(&root.join("crates"), root, &mut failures);

    if failures.is_empty() {
        println!(
            "architecture test ok: dependency direction, version SSOT, serialization boundary"
        );
        return ExitCode::SUCCESS;
    }
    eprintln!("architecture test FAILED:");
    for failure in &failures {
        eprintln!("  - {failure}");
    }
    ExitCode::FAILURE
}

/// The whole scan, extracted from `run` so the wiring itself is testable
/// against a fixture tree — a duplicated or dropped call here is invisible
/// to the per-check unit tests.
fn check_tree(crates_dir: &Path, root: &Path, failures: &mut Vec<String>) {
    let crate_names = member_crates(crates_dir, failures);
    check_named_postures(&crate_names, failures);
    for name in &crate_names {
        let manifest = crates_dir.join(name).join("Cargo.toml");
        let rel = relative(&manifest, root);
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            failures.push(format!("{rel}: unreadable"));
            continue;
        };
        check_package_name(&rel, &text, name, failures);
        check_manifest_text(&rel, &text, name, &posture_of(name), &crate_names, failures);
    }
    check_serialization_boundary(crates_dir, root, &crate_names, failures);
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every subdirectory of `crates/` carrying a Cargo.toml is a member (the
/// workspace globs `crates/*`, so the directory scan needs no manifest
/// parse). This set is also the definition of "internal crate" for the
/// edge checks.
fn member_crates(crates_dir: &Path, failures: &mut Vec<String>) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(crates_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().join("Cargo.toml").is_file())
                .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    if names.is_empty() {
        failures.push("crates/: no member crates found".to_string());
    }
    names
}

fn check_named_postures(crate_names: &[String], failures: &mut Vec<String>) {
    for named in NAMED_POSTURES {
        if !crate_names.iter().any(|name| name == named) {
            failures.push(format!(
                "crates/{named}: gone — its named posture in crates/xtask/src/arch.rs must move with it (staleness fuse)"
            ));
        }
    }
}

/// "Internal crate" identity is the directory name (`member_crates` feeds
/// the edge checks); a hand-edited package rename inside the manifest would
/// silently exempt the crate from them.
fn check_package_name(rel: &str, text: &str, crate_name: &str, failures: &mut Vec<String>) {
    let expected = format!("name = \"{crate_name}\"");
    if !text
        .lines()
        .map(str::trim)
        .any(|line| line == expected.as_str())
    {
        failures.push(format!(
            "{rel}: package name must equal the directory name {crate_name:?} — the internal-edge checks key on directory names"
        ));
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Section {
    Deps,
    DevDeps,
    Other,
}

fn section_of(header: &str) -> Section {
    match header {
        "[dependencies]" | "[build-dependencies]" => Section::Deps,
        "[dev-dependencies]" => Section::DevDeps,
        // [target.'cfg(...)'.dependencies] / .dev-dependencies]
        h if h.starts_with("[target.") && h.ends_with(".dev-dependencies]") => Section::DevDeps,
        h if h.starts_with("[target.") && h.ends_with(".dependencies]") => Section::Deps,
        _ => Section::Other,
    }
}

/// A dependency entry's package key: the segment before the first `.` (the
/// dotted form `name.workspace = true`) or `=` / whitespace (the inline-table
/// form). Entries are single-line by convention; nothing formats TOML here,
/// so a wrapped multi-line entry parses as fresh garbage keys and fails the
/// checks loudly — that fail-closed behavior is the enforcer.
fn dep_name(line: &str) -> &str {
    let end = line.find(['.', '=', ' ', '\t']).unwrap_or(line.len());
    &line[..end]
}

fn check_manifest_text(
    rel: &str,
    text: &str,
    crate_name: &str,
    posture: &Posture,
    members: &[String],
    failures: &mut Vec<String>,
) {
    let mut section = Section::Other;
    for raw_line in text.lines() {
        // Strip comments before any matching: a trailing `# ... workspace =
        // true` must not satisfy the version-SSOT test below. The
        // repository's dependency grammar (`workspace = true` plus
        // features/optional) never carries a '#' inside a value, and
        // deny.toml's source policy keeps git URLs out.
        let line = raw_line.split('#').next().unwrap_or(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            section = section_of(line);
            if section == Section::Other && line.contains("dependencies") {
                // Fail closed: an unrecognized spelling must not silently
                // exempt the section's entries from the checks.
                failures.push(format!(
                    "{rel}: unrecognized dependency-section header {line:?} — use [dependencies] / [dev-dependencies]"
                ));
            }
            continue;
        }
        if section == Section::Other {
            continue;
        }
        let name = dep_name(line);
        let dev = section == Section::DevDeps;
        let kind = if dev { "dev-dependency" } else { "dependency" };

        if members.iter().any(|member| member == name) {
            let allowed = if dev {
                posture.dev_internal
            } else {
                posture.deps_internal
            };
            if let Some(allowed) = allowed
                && !allowed.contains(&name)
            {
                failures.push(format!(
                    "{rel}: {kind} '{name}' inverts the dependency direction — {crate_name} may not depend on this workspace crate (ADR-0002; postures in crates/xtask/src/arch.rs)"
                ));
            }
        } else {
            match posture.third_party {
                ThirdParty::None => failures.push(format!(
                    "{rel}: {kind} '{name}' — xtask keeps zero dependencies by policy (std-only)"
                )),
                ThirdParty::Forbid(list) if !dev && list.contains(&name) => {
                    failures.push(format!(
                        "{rel}: dependency '{name}' is a runtime/IO crate — the core stays pure logic; IO lives in the adapters (ADR-0002)"
                    ));
                }
                ThirdParty::Only(list) if !list.contains(&name) => {
                    failures.push(format!(
                        "{rel}: {kind} '{name}' — the contract's third-party deps are a closed set (every crate links it transitively, so a new entry fans out workspace-wide); a deliberate addition edits crates/xtask/src/arch.rs"
                    ));
                }
                ThirdParty::Forbid(_) | ThirdParty::Any | ThirdParty::Only(_) => {}
            }
        }

        if !line.replace([' ', '\t'], "").contains("workspace=true") {
            failures.push(format!(
                "{rel}: {kind} '{name}' must inherit via workspace = true — versions live only in the root [workspace.dependencies]"
            ));
        }
    }
}

fn check_serialization_boundary(
    crates_dir: &Path,
    root: &Path,
    crate_names: &[String],
    failures: &mut Vec<String>,
) {
    for name in crate_names {
        if name == "cadmus-contract" {
            continue;
        }
        let mut files: Vec<PathBuf> = Vec::new();
        collect_rs_files(&crates_dir.join(name), &mut files);
        files.sort();
        for file in files {
            let rel = relative(&file, root);
            let Ok(text) = std::fs::read_to_string(&file) else {
                failures.push(format!("{rel}: unreadable"));
                continue;
            };
            check_rs_text(&rel, &text, failures);
        }
    }
}

/// Scans one Rust source for serde derives. rustfmt wraps long derive lists
/// across lines — exactly the derive-heavy wire types this scan exists to
/// catch — so the match follows an open `#[derive(` attribute until its
/// closing `)]` instead of requiring a single line.
fn check_rs_text(rel: &str, text: &str, failures: &mut Vec<String>) {
    let mut inside_derive = false;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[derive(") {
            inside_derive = true;
        }
        if inside_derive && (trimmed.contains("Serialize") || trimmed.contains("Deserialize")) {
            failures.push(format!(
                "{rel}:{}: Serialize/Deserialize derive outside cadmus-contract — serializable wire types live only in the contract crate (report §9.1.1)",
                index + 1
            ));
        }
        if inside_derive && trimmed.contains(")]") {
            inside_derive = false;
        }
    }
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members() -> Vec<String> {
        [
            "cadmus",
            "cadmus-contract",
            "cadmus-core",
            "cadmus-llm-openai",
        ]
        .map(str::to_owned)
        .into()
    }

    fn check(crate_name: &str, manifest: &str) -> Vec<String> {
        let mut failures = Vec::new();
        check_manifest_text(
            "test/Cargo.toml",
            manifest,
            crate_name,
            &posture_of(crate_name),
            &members(),
            &mut failures,
        );
        failures
    }

    #[test]
    fn tree_scan_reports_each_violation_exactly_once() {
        // Wiring-level regression test: the per-check unit tests cannot see
        // a duplicated or dropped call in the tree scan itself.
        let root = std::env::temp_dir().join(format!("cadmus-arch-tree-{}", std::process::id()));
        let crates = root.join("crates");
        for name in ["cadmus", "cadmus-contract", "cadmus-core", "xtask"] {
            std::fs::create_dir_all(crates.join(name)).expect("mkdir");
            std::fs::write(
                crates.join(name).join("Cargo.toml"),
                format!("[package]\nname = \"{name}\"\n\n[dependencies]\n"),
            )
            .expect("write manifest");
        }
        std::fs::create_dir_all(crates.join("cadmus-probe")).expect("mkdir probe");
        std::fs::write(
            crates.join("cadmus-probe").join("Cargo.toml"),
            "[package]\nname = \"cadmus-probe\"\n\n[dependencies]\ncadmus-core.workspace = true\n",
        )
        .expect("write probe");

        let mut failures = Vec::new();
        check_tree(&crates, &root, &mut failures);
        std::fs::remove_dir_all(&root).expect("cleanup");

        let probe_failures: Vec<_> = failures
            .iter()
            .filter(|f| f.contains("cadmus-probe"))
            .collect();
        assert_eq!(probe_failures.len(), 1, "failures: {failures:?}");
        assert!(probe_failures[0].contains("inverts the dependency direction"));
        // No staleness fuses: all four named crates exist above.
        assert!(
            !failures.iter().any(|f| f.contains("staleness fuse")),
            "failures: {failures:?}"
        );
    }

    #[test]
    fn named_postures_are_fused_to_the_directory_scan() {
        // The fixture lacks cadmus-memory (an adapter — its absence is fine,
        // the default posture needs no fuse) and xtask (a named posture —
        // its absence must fail loudly).
        let mut failures = Vec::new();
        check_named_postures(&members(), &mut failures);
        assert!(failures.iter().any(|f| f.contains("crates/xtask: gone")));
        assert!(!failures.iter().any(|f| f.contains("cadmus-memory")));
        assert!(!failures.iter().any(|f| f.contains("cadmus-core")));
    }

    #[test]
    fn new_crates_default_to_the_adapter_posture() {
        let posture = posture_of("cadmus-sandbox");
        assert_eq!(posture.deps_internal, Some(&["cadmus-contract"][..]));
        assert_eq!(
            posture.dev_internal,
            Some(&["cadmus-contract", "cadmus-core"][..])
        );
        assert!(matches!(posture.third_party, ThirdParty::Any));
    }

    #[test]
    fn adapters_add_third_party_deps_without_touching_this_file() {
        // The friction case that motivated forbidden edges: phase 2's SQL
        // store arriving in the memory adapter is not an architecture event.
        let manifest = "\
[dependencies]
cadmus-contract.workspace = true
rusqlite.workspace = true
";
        assert!(check("cadmus-memory", manifest).is_empty());
    }

    #[test]
    fn internal_edges_follow_the_direction() {
        let edge = "\
[dependencies]
cadmus-contract.workspace = true
";
        // Allowed: the core and adapters point at the contract.
        assert!(check("cadmus-core", edge).is_empty());
        assert!(check("cadmus-llm-openai", edge).is_empty());
        // Forbidden: the contract takes nothing internal.
        assert!(check("cadmus-contract", edge)[0].contains("inverts the dependency direction"));

        let adapter_edge = "\
[dependencies]
cadmus-llm-openai.workspace = true
";
        // Forbidden: the core and other adapters never name an adapter.
        assert!(check("cadmus-core", adapter_edge)[0].contains("inverts the dependency direction"));
        assert!(
            check("cadmus-memory", adapter_edge)[0].contains("inverts the dependency direction")
        );
        // The binary wires everything.
        assert!(check("cadmus", adapter_edge).is_empty());
    }

    #[test]
    fn dev_edges_allow_core_fakes_for_adapters_only() {
        let dev_core = "\
[dev-dependencies]
cadmus-core.workspace = true
";
        assert!(check("cadmus-llm-openai", dev_core).is_empty());
        assert!(check("cadmus-core", dev_core)[0].contains("inverts the dependency direction"));
        assert!(check("cadmus-contract", dev_core)[0].contains("inverts the dependency direction"));
        assert!(check("xtask", dev_core)[0].contains("inverts the dependency direction"));
    }

    #[test]
    fn the_core_io_tripwire_applies_to_deps_not_dev_deps() {
        let as_dep = "\
[dependencies]
tokio.workspace = true
";
        assert!(check("cadmus-core", as_dep)[0].contains("runtime/IO crate"));

        let as_dev_dep = "\
[dev-dependencies]
tokio = { workspace = true, features = [\"macros\", \"rt\"] }
";
        // The test runtime is legitimate; other crates are unconstrained.
        assert!(check("cadmus-core", as_dev_dep).is_empty());
        assert!(check("cadmus-memory", as_dep).is_empty());
    }

    #[test]
    fn the_contracts_third_party_set_is_closed() {
        // Every crate links the contract transitively: a new dep there fans
        // out workspace-wide, so it is never a routine decision.
        let client = "\
[dependencies]
genai.workspace = true
";
        assert!(check("cadmus-contract", client)[0].contains("closed set"));
        // The same dep is unconstrained in an adapter — the asymmetry is the
        // point.
        assert!(check("cadmus-llm-openai", client).is_empty());
    }

    #[test]
    fn xtask_stays_at_zero_dependencies() {
        let anything = "\
[dependencies]
serde.workspace = true
";
        assert!(check("xtask", anything)[0].contains("zero dependencies"));
    }

    #[test]
    fn package_names_must_match_directory_names() {
        let mut failures = Vec::new();
        check_package_name(
            "test/Cargo.toml",
            "[package]\nname = \"renamed\"\n",
            "cadmus-core",
            &mut failures,
        );
        assert_eq!(failures.len(), 1, "failures: {failures:?}");
        assert!(failures[0].contains("package name must equal the directory name"));

        let mut failures = Vec::new();
        check_package_name(
            "test/Cargo.toml",
            "[package]\nname = \"cadmus-core\"\n",
            "cadmus-core",
            &mut failures,
        );
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    #[test]
    fn section_headers_map_to_dep_kinds() {
        assert_eq!(section_of("[dependencies]"), Section::Deps);
        assert_eq!(section_of("[build-dependencies]"), Section::Deps);
        assert_eq!(section_of("[dev-dependencies]"), Section::DevDeps);
        assert_eq!(
            section_of("[target.'cfg(unix)'.dependencies]"),
            Section::Deps
        );
        assert_eq!(
            section_of("[target.'cfg(windows)'.dev-dependencies]"),
            Section::DevDeps
        );
        assert_eq!(section_of("[package.metadata.dist]"), Section::Other);
    }

    #[test]
    fn dep_names_parse_from_dotted_and_inline_forms() {
        assert_eq!(
            dep_name("cadmus-contract.workspace = true"),
            "cadmus-contract"
        );
        assert_eq!(
            dep_name("tokio = { workspace = true, features = [\"rt\"] }"),
            "tokio"
        );
        assert_eq!(dep_name("serde = \"1\""), "serde");
    }

    #[test]
    fn direct_versions_fail_even_when_faked_in_comments() {
        let direct = "\
[dependencies]
serde = \"1\"
";
        assert!(check("cadmus-core", direct)[0].contains("must inherit via workspace = true"));

        let commented = "\
[dependencies]
serde = \"1\" # TODO: restore serde.workspace = true after the hotfix
";
        let failures = check("cadmus-core", commented);
        assert_eq!(failures.len(), 1, "failures: {failures:?}");
        assert!(failures[0].contains("must inherit via workspace = true"));
    }

    #[test]
    fn unrecognized_dependency_headers_fail_closed() {
        let manifest = "\
[dependencies.serde]
workspace = true

[dependencies] # keep sorted
cadmus-contract.workspace = true
";
        let failures = check("cadmus-core", manifest);
        // The sub-table header fails closed; the commented [dependencies]
        // header parses once the comment is stripped, so the edge passes.
        assert_eq!(failures.len(), 1, "failures: {failures:?}");
        assert!(failures[0].contains("unrecognized dependency-section header"));
    }

    #[test]
    fn serde_derives_are_flagged_across_rustfmt_wrapping() {
        // concat! keeps the fixture text off the source line start: arch-test
        // scans this very file, and a fixture line beginning with the
        // attribute would self-flag.
        let source = concat!(
            "#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\n",
            "pub struct OneLine;\n",
            "\n",
            "#[derive(\n",
            "    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize\n",
            ")]\n",
            "pub struct Wrapped;\n",
        );
        let mut failures = Vec::new();
        check_rs_text("test/lib.rs", source, &mut failures);
        assert_eq!(failures.len(), 2, "failures: {failures:?}");
        assert!(failures[0].contains(":1:"));
        // The wrapped derive's serde names land on the list's second line.
        assert!(failures[1].contains(":5:"));
    }

    #[test]
    fn prose_and_comments_mentioning_derives_are_not_code() {
        // The derive attribute must come first: prose after a *closed* derive
        // is the only case pinning the `)]` reset in `check_rs_text`.
        let source = "\
#[derive(Debug)]
struct Plain;
// Serialize lives in the contract
let note = \"derive(Serialize)\";
//! the derive(Serialize) boundary scan
";
        let mut failures = Vec::new();
        check_rs_text("test/lib.rs", source, &mut failures);
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    #[test]
    fn boundary_scan_walks_subdirs_and_exempts_contract() {
        let root = std::env::temp_dir().join(format!("cadmus-arch-test-{}", std::process::id()));
        let crates = root.join("crates");
        std::fs::create_dir_all(crates.join("cadmus-core/src")).expect("mkdir core");
        std::fs::create_dir_all(crates.join("cadmus-core/tests")).expect("mkdir core tests");
        std::fs::create_dir_all(crates.join("cadmus-contract/src")).expect("mkdir contract");
        std::fs::write(
            crates.join("cadmus-core/src/lib.rs"),
            "#[derive(Serialize)]\nstruct InSrc;\n",
        )
        .expect("write src");
        std::fs::write(
            crates.join("cadmus-core/tests/fixture.rs"),
            "#[derive(Deserialize)]\nstruct InTests;\n",
        )
        .expect("write tests");
        std::fs::write(
            crates.join("cadmus-contract/src/lib.rs"),
            "#[derive(Serialize)]\nstruct Allowed;\n",
        )
        .expect("write contract");

        let names = vec!["cadmus-contract".to_string(), "cadmus-core".to_string()];
        let mut failures = Vec::new();
        check_serialization_boundary(&crates, &root, &names, &mut failures);
        std::fs::remove_dir_all(&root).expect("cleanup");

        assert_eq!(failures.len(), 2, "failures: {failures:?}");
        assert!(failures.iter().all(|f| f.contains("cadmus-core")));
        assert!(failures.iter().any(|f| f.contains("tests/fixture.rs:1")));
    }
}
