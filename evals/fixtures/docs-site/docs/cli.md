# Command-line reference

Peridot has three core subcommands: `new`, `build`, and `serve`. For
deployment, see `docs/deployment.md`.

## `peridot new <path>`

Scaffold a new site: a `peridot.yml` and a `docs/` directory with a
starter `index.md`.

Flags:

- `--theme <name>` — preselect a bundled theme (default: `citrine`).

## `peridot build`

Render the site into the output directory (`public/` by default).

Flags:

- `--drafts` — include pages marked `draft: true`.
- `--strict` — turn every warning into an error.

## `peridot serve`

Start the dev server. It renders the site once, then watches the source
files: whenever a file changes, it rebuilds the site automatically and
reloads the open browser tabs.

Flags:

- `--port <number>` — listen on this port instead of the default 7180.
- `--open` — open a browser tab on startup.
