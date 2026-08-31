# cadmus

[中文文档](./README.zh-CN.md)

[![CI](https://github.com/duskgrow/cadmus/actions/workflows/ci.yml/badge.svg)](https://github.com/duskgrow/cadmus/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/cadmus.svg)](https://crates.io/crates/cadmus)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](./LICENSE-MIT)

TODO: one-line description of what this project does (also update `description` in crates/cadmus/Cargo.toml).

## Install

After the first release, use the installer from GitHub Releases:

    curl --proto '=https' --tlsv1.2 -LsSf https://github.com/duskgrow/cadmus/releases/latest/download/cadmus-installer.sh | sh

or install from crates.io:

    cargo install cadmus

## Usage

See `cadmus --help`.

## Development

    direnv allow   # or: nix develop (entering the shell also arms the git hooks)
    just ci        # full quality gate (local green ≡ CI green)

See [CONTRIBUTING.md](./CONTRIBUTING.md).

## License

MIT OR Apache-2.0 — see [LICENSE-MIT](./LICENSE-MIT) and [LICENSE-APACHE](./LICENSE-APACHE).
