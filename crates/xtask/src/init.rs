//! `init <name> <owner>` — one-shot bootstrap: turns the template into a fresh
//! project (rename, strip template artifacts, git init, stage). This module is
//! template-only: a successful init deletes it (and its registration in
//! `main.rs`), so generated projects never carry their own scaffolding.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::new_crate::is_kebab_case;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
/// This very file contains the placeholder literals and is deleted at the end
/// of the run — never substitute inside it.
const SELF_REL: &str = "crates/xtask/src/init.rs";
/// Never substituted: VCS/build/tooling directories.
const SKIP_DIRS: [&str; 3] = [".git", "target", ".direnv"];

const README_TEMPLATE: &str = r"# @PROJECT@

[中文文档](./README.zh-CN.md)

[![CI](https://github.com/@OWNER@/@PROJECT@/actions/workflows/ci.yml/badge.svg)](https://github.com/@OWNER@/@PROJECT@/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/@PROJECT@.svg)](https://crates.io/crates/@PROJECT@)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](./LICENSE-MIT)

TODO: one-line description of what this project does (also update `description` in crates/@PROJECT@/Cargo.toml).

## Install

After the first release, use the installer from GitHub Releases:

    curl --proto '=https' --tlsv1.2 -LsSf https://github.com/@OWNER@/@PROJECT@/releases/latest/download/@PROJECT@-installer.sh | sh

or install from crates.io:

    cargo install @PROJECT@

## Usage

See `@PROJECT@ --help`.

## Development

    direnv allow   # or: nix develop (entering the shell also arms the git hooks)
    just ci        # full quality gate (local green ≡ CI green)

See [CONTRIBUTING.md](./CONTRIBUTING.md).

## License

MIT OR Apache-2.0 — see [LICENSE-MIT](./LICENSE-MIT) and [LICENSE-APACHE](./LICENSE-APACHE).
";

const README_ZH_TEMPLATE: &str = r"# @PROJECT@

[English](./README.md)（如有出入以英文版为准）

TODO: 一句话说明这个项目做什么（同步更新 crates/@PROJECT@/Cargo.toml 的 description）。

## 安装

首次发版后可使用 GitHub Release 的安装脚本：

    curl --proto '=https' --tlsv1.2 -LsSf https://github.com/@OWNER@/@PROJECT@/releases/latest/download/@PROJECT@-installer.sh | sh

或从 crates.io 安装：

    cargo install @PROJECT@

## 使用

见 `@PROJECT@ --help`。

## 开发

    direnv allow   # 或 nix develop（进入 devShell 会自动安装 git 钩子）
    just ci        # 全量质量门（本地绿 ≡ CI 绿）

详见 [CONTRIBUTING.zh-CN.md](./CONTRIBUTING.zh-CN.md)。

## License

MIT OR Apache-2.0，见 [LICENSE-MIT](./LICENSE-MIT) 与 [LICENSE-APACHE](./LICENSE-APACHE)。
";

const ADR_TEMPLATE: &str = r#"# 0001. Record architecture decisions with ADRs

- Status: accepted
- Date: @DATE@

## Context

The hardest thing to track over a project's lifetime is the *motivation* behind
decisions; rationale scattered across PR descriptions, chat and wikis rots or
contradicts itself.

## Decision

Record architecturally significant decisions as lightweight MADR files in
`docs/decisions/`, named `NNNN-title.md`, sequentially numbered, numbers never
reused. A superseded decision is marked `superseded by ADR-NNNN`, never deleted.
Code comments, docs and PR descriptions reference the number ("see ADR-0007")
instead of restating the rationale.

## Consequences

Each decision's rationale has exactly one authoritative location; a new decision
is a new file, so there is no merge-conflict hot spot. ADRs record "why we
decided this back then"; current-state descriptions belong to architecture docs
and code.
"#;

/// Entry point: `init <project-name> <github-owner>`.
pub fn run(args: &[String]) -> ExitCode {
    let [name, owner] = args else {
        return fail(
            "missing arguments",
            "init requires a project name and a GitHub owner",
            "just init my-cli octocat",
        );
    };
    if !is_kebab_case(name) {
        return fail(
            &format!("invalid project name: '{name}'"),
            "the name doubles as the crate and binary name, so it must be kebab-case",
            "lowercase letters, digits and hyphens only, e.g. my-cli",
        );
    }
    if !is_github_owner(owner) {
        return fail(
            &format!("invalid GitHub owner: '{owner}'"),
            "GitHub user/org names allow only letters, digits and hyphens",
            "e.g. octocat or my-org",
        );
    }
    let root = Path::new(ROOT);
    let placeholder_dir = root.join("crates/__project_name__");
    if !placeholder_dir.is_dir() {
        return fail(
            "crates/__project_name__ not found",
            "this repository looks already initialized — init can only run once",
            "to start over: nix flake init -t <template-repo> into a fresh directory",
        );
    }

    let snake = name.replace('-', "_");
    println!("==> project: {name} (rust crate: {snake})   owner: {owner}");
    if let Err(err) = bootstrap(root, name, &snake, owner, &placeholder_dir) {
        eprintln!("error: {err}");
        return ExitCode::FAILURE;
    }

    println!(
        "
✅ {name} initialized — zero template residue, the whole tree staged, and the
   git hooks already armed. No commits yet: the first commit is yours.

Next steps:
  1. Review the staged tree        git status && git diff --cached --stat
  2. Make your first commit        git commit -m \"chore: initial commit\"
     (hooks fire on it: just fmt + just lint + the commit-msg convention check)
  3. Run the full quality gate     just ci
  4. Fill in the crate description: the description field in
     crates/{name}/Cargo.toml (feeds --help and crates.io)
  5. Create the GitHub repo        gh repo create {owner}/{name} --public --source=. --push
  6. Lock the repo down            see CONTRIBUTING.md \"Repository settings\"
     (squash-only merge + branch protection — the hard boundary that keeps
     both humans and agents from merging red code)
  7. One-time release setup        see CONTRIBUTING.md \"Releasing\"
     (GitHub permissions + first manual crates.io publish + Trusted Publishing)
"
    );
    ExitCode::SUCCESS
}

fn bootstrap(
    root: &Path,
    name: &str,
    snake: &str,
    owner: &str,
    placeholder_dir: &Path,
) -> Result<(), String> {
    // 1. Placeholder substitution: snake_case in .rs files (Rust crate
    //    references), kebab-case everywhere else.
    let mut files = Vec::new();
    walk(root, &mut files)?;
    substitute(&files, "__project_name__", snake, true)?;
    substitute(&files, "__project_name__", name, false)?;
    substitute(&files, "__GITHUB_OWNER__", owner, false)?;
    std::fs::rename(placeholder_dir, root.join("crates").join(name))
        .map_err(|err| format!("rename crate dir: {err}"))?;

    // 2. Remove template artifacts (blocks fenced by template-only markers
    //    + template-only files, including this module).
    println!("==> stripping template-only parts");
    for rel in [
        "justfile",
        "flake.nix",
        ".github/workflows/ci.yml",
        "crates/xtask/src/main.rs",
    ] {
        strip_template_only(&root.join(rel))?;
    }
    remove_file(&root.join("docs/engspec.md"))?;
    remove_file(&root.join(".github/workflows/init.yml"))?;
    remove_file(&root.join(SELF_REL))?;

    // 3. Project README (replaces the template README), English canonical + zh-CN.
    for (rel, template) in [
        ("README.md", README_TEMPLATE),
        ("README.zh-CN.md", README_ZH_TEMPLATE),
    ] {
        let content = template
            .replace("@PROJECT@", name)
            .replace("@OWNER@", owner);
        std::fs::write(root.join(rel), content).map_err(|err| format!("write {rel}: {err}"))?;
    }

    // 4. Reset ADRs to a fresh starting point (the template's ADRs stay with
    //    the template — they document template decisions, not yours).
    let decisions = root.join("docs/decisions");
    for entry in read_dir(&decisions)? {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            std::fs::remove_file(path).map_err(|err| format!("remove ADR: {err}"))?;
        }
    }
    let today = civil_from_days(epoch_days_now());
    std::fs::write(
        decisions.join("0001-record-architecture-decisions.md"),
        ADR_TEMPLATE.replace("@DATE@", &today),
    )
    .map_err(|err| format!("write ADR: {err}"))?;

    // 5. Rebuild the lockfile + git init + stage the tree. Deliberately NO
    //    commit: the project's history must be authored entirely by its owner.
    println!("==> regenerating Cargo.lock");
    run_cmd("cargo", &["generate-lockfile"], root)?;
    if !root.join(".git").is_dir() {
        run_cmd("git", &["init", "-b", "main"], root)?;
    }
    run_cmd("git", &["add", "-A"], root)?;

    // Arm the git hooks now so the creator has zero setup steps left (the
    // devShell shellHook does the same for every future contributor).
    if which("prek") {
        run_cmd(
            "prek",
            &[
                "install",
                "--hook-type",
                "pre-commit",
                "--hook-type",
                "commit-msg",
            ],
            root,
        )?;
    }
    Ok(())
}

/// Three-part bootstrap failure: what happened / why / what to do.
fn fail(what: &str, why: &str, how: &str) -> ExitCode {
    eprintln!("error: {what}\n  -> why: {why}\n  -> how: {how}");
    ExitCode::FAILURE
}

/// `[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?` — GitHub user/org names.
fn is_github_owner(owner: &str) -> bool {
    let bytes = owner.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
}

/// Recursive file listing, skipping symlinks and [`SKIP_DIRS`]
/// (`grep -rlI --exclude-dir` equivalent).
fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in read_dir(dir)? {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|err| format!("file type of {}: {err}", path.display()))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let name = entry.file_name();
            if !SKIP_DIRS.contains(&name.to_string_lossy().as_ref()) {
                walk(&path, out)?;
            }
        } else if file_type.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn read_dir(dir: &Path) -> Result<Vec<std::fs::DirEntry>, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|err| format!("read {}: {err}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("read {}: {err}", dir.display()))?;
    Ok(entries)
}

/// Replace `pattern` in every text file (binary files are skipped, like
/// `grep -I`). `only_rs` restricts to Rust sources for the `snake_case` pass.
fn substitute(
    files: &[PathBuf],
    pattern: &str,
    replacement: &str,
    only_rs: bool,
) -> Result<(), String> {
    for path in files {
        if path.ends_with(SELF_REL) {
            continue;
        }
        if only_rs && path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        if text.contains(pattern) {
            std::fs::write(path, text.replace(pattern, replacement))
                .map_err(|err| format!("write {}: {err}", path.display()))?;
        }
    }
    Ok(())
}

/// Delete every line between `>>> template-only` and `<<< template-only`
/// markers inclusive (`sed '/>>>/,/<<</d'` equivalent; the marker works with
/// any comment prefix — `#` in justfile/nix/YAML, `//` in Rust).
fn strip_template_only(path: &Path) -> Result<(), String> {
    let text =
        std::fs::read_to_string(path).map_err(|err| format!("read {}: {err}", path.display()))?;
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    for line in text.split_inclusive('\n') {
        if line.contains(">>> template-only") {
            inside = true;
        } else if line.contains("<<< template-only") {
            inside = false;
        } else if !inside {
            out.push_str(line);
        }
    }
    std::fs::write(path, out).map_err(|err| format!("write {}: {err}", path.display()))
}

fn remove_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("remove {}: {err}", path.display())),
    }
}

fn run_cmd(program: &str, args: &[&str], dir: &Path) -> Result<(), String> {
    let status = std::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|err| format!("spawn {program}: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}

/// Poor-man's `which(1)`: is `program` (or `program.exe`) on `PATH`?
fn which(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .any(|dir| dir.join(program).is_file() || dir.join(format!("{program}.exe")).is_file())
    })
}

/// Days since 1970-01-01 (UTC), read once at the edge so the calendar math
/// stays pure and unit-testable (workspace style: time is injected).
fn epoch_days_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() / 86_400)
        .unwrap_or(0)
}

/// Calendar date (`YYYY-MM-DD`, UTC) for `days` since 1970-01-01 — Howard
/// Hinnant's civil-from-days algorithm, in std so `xtask` stays dependency-free.
fn civil_from_days(days: u64) -> String {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), "1970-01-01");
        assert_eq!(civil_from_days(20_089), "2025-01-01");
    }

    #[test]
    fn github_owner_validation() {
        assert!(is_github_owner("octocat"));
        assert!(is_github_owner("my-org"));
        assert!(is_github_owner("a"));
        assert!(!is_github_owner("-bad"));
        assert!(!is_github_owner("bad-"));
        assert!(!is_github_owner("bad_name"));
        assert!(!is_github_owner(""));
    }

    #[test]
    fn template_only_strip_handles_both_comment_styles() {
        let dir = std::env::temp_dir().join("xtask-strip-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sample.txt");
        std::fs::write(
            &file,
            "keep1\n# >>> template-only\ndrop\n// <<< template-only\nkeep2\n",
        )
        .unwrap();
        strip_template_only(&file).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep1\nkeep2\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
