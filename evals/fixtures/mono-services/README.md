# parcelops

A mini parcel-delivery platform: one public edge service, one settlement
worker, and a shared id library.

## Layout

- `services/gateway/` — public HTTP edge; config in `services/gateway/gateway.toml`
- `services/ledger/` — settlement worker; config in `services/ledger/ledger.toml`
- `libs/ids/src/lib.rs` — shared id formatting (prefix + hex suffix)
- `deploy/` — plain-data deployment manifests, one per service
- `docs/runbook.md` — operating procedures

## Run

Each service loads the TOML config in its own directory at startup:

```sh
cargo run -p gateway   # reads services/gateway/gateway.toml
cargo run -p ledger    # reads services/ledger/ledger.toml
```

Parcel ids look like `pcl-9f31ab02c7e4`; settlement entry ids look like
`led-04de77a1b3c8`.
