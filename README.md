# cadmus

[中文文档](./README.zh-CN.md)

[![CI](https://github.com/duskgrow/cadmus/actions/workflows/ci.yml/badge.svg)](https://github.com/duskgrow/cadmus/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/cadmus.svg)](https://crates.io/crates/cadmus)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](./LICENSE-MIT)

A self-evolving coding agent in Rust: self-built agent loop, OpenAI-compatible provider dialects, read-only coding tools — currently phase 0 of the [roadmap](docs/roadmap.md).

## Install

After the first release, use the installer from GitHub Releases:

    curl --proto '=https' --tlsv1.2 -LsSf https://github.com/duskgrow/cadmus/releases/latest/download/cadmus-installer.sh | sh

or install from crates.io:

    cargo install cadmus

## Usage

    export MOONSHOT_API_KEY=sk-…   # or DEEPSEEK_API_KEY for --provider deepseek
    cadmus chat "explain crates/cadmus-core/src/agent.rs"

The coding tools (`read_file`, `grep`, `list_dir`) are read-only and confined to the current directory. See `cadmus --help` for all options.

## Development

    direnv allow   # or: nix develop (entering the shell also arms the git hooks)
    just ci        # full quality gate (local green ≡ CI green)

See [CONTRIBUTING.md](./CONTRIBUTING.md).

## License

MIT OR Apache-2.0 — see [LICENSE-MIT](./LICENSE-MIT) and [LICENSE-APACHE](./LICENSE-APACHE).
