# birdlog

A small library and CLI for birding observation logs. Each observation
records what you saw, where, and when, stored as one JSON line per record.

## Install

```sh
pip install birdlog
```

## Usage

```sh
birdlog log --species "barn swallow" --location "old mill pond" --date 2026-04-02
birdlog list
birdlog find swallow
```

The store lives at `~/.birdlog/sightings.jsonl` by default; override the
directory with the `BIRDLOG_HOME` environment variable.

## Project layout

- `src/birdlog/models.py` — the Observation dataclass.
- `src/birdlog/store.py` — the JSONL-backed store.
- `src/birdlog/search.py` — substring search over observations.
- `src/birdlog/cli.py` — the command-line entry point.
