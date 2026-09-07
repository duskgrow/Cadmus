# ledger

Settlement worker for parcelops. Consumes parcel events from the
`parcel-settle-events` queue and records settlement entries with ids from
`ids::format_entry_id`.

- Owner team: team-settlement
- Listens on port 9030
- Readiness probe: `GET /readyz`
