# Getting started

## Install

Peridot is a single binary. Install it with:

```sh
cargo install peridot
```

## Create a site

```sh
peridot new my-site
cd my-site
```

This scaffolds a `peridot.yml` and a `docs/` directory with a starter
`index.md`.

## Preview

```sh
peridot serve
```

The dev server listens on port 7180 by default. The rendered site lands in
the output directory when you run `peridot build`.
