# Configuration

Peridot reads one configuration file, `peridot.yml`, at the project root.
Every key is optional unless marked required.

## `site_name`

Required. The site title, shown in the header and the `<title>` tag.

## `theme`

The bundled theme used to render the site. Default: `citrine`. See
`docs/themes.md` for the full list of bundled themes.

## `output_dir`

The directory `peridot build` writes the rendered site into. Default:
`public/`. The output directory is wiped on every build, so never keep
hand-written files there.

## `port`

The port the dev server listens on. Default: `7180`. Override it in
`peridot.yml`, or pass `--port` to `peridot serve` for one run.

## `permalink`

The URL pattern every page gets. Default: `/:slug/`. The placeholder
`:slug` is the page's file name without the `.md` extension, lowercased.
