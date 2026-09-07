# gateway

Public edge service for parcelops. Accepts parcel submissions, assigns
parcel ids via `ids::format_parcel_id`, and forwards work to ledger.

- Owner team: team-edge
- Listens on port 8421
- `GET /healthz` returns `ok` while the upstream connection to ledger is open
- Rate limit: 250 requests per second per client
