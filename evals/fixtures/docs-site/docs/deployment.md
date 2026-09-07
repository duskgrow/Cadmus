# Deployment

A Peridot site is plain static files, so any static host can serve it.

## The deploy command

```sh
peridot deploy
```

`peridot deploy` runs a production build (minified, with draft pages
excluded) and uploads the contents of `public/` via rsync over SSH to the
hosting target configured in `peridot.yml`. Run it from the project root.

## Hosting notes

- The uploaded site needs no server-side runtime; a plain file host or an
  object store behind a CDN is enough.
- Builds are reproducible: the same source always produces the same
  `public/` tree, so hosts can cache aggressively.
