# notecli

A tiny command-line notebook. Notes are plain text lines with optional tags,
stored in one JSONL file.

## Install

```sh
cargo install --path .
```

## Usage

```sh
notecli add "buy milk" --tags home,errands
notecli list
notecli search milk
```

Notes live in `~/.notes` by default; override with the `NOTECLI_HOME`
environment variable.
