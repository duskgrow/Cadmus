# parcelops runbook

## Restarting services in production

Restart ledger first, then gateway. Gateway replays unprocessed events from
the `parcel-settle-events` queue at boot, so ledger must be healthy before
gateway comes back.

1. `kubectl rollout restart deployment/ledger`
2. Wait until all ledger replicas report ready on `/readyz`.
3. `kubectl rollout restart deployment/gateway`
4. Wait 45 seconds for gateway connections to drain, then verify
   `GET /healthz` returns `ok` on each gateway replica.

Never restart gateway before ledger: parcels submitted during the gap are
queued, not lost, but gateway health checks fail until ledger is back.
