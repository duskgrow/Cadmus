//! gateway: public edge service for parcelops.

use ids::format_parcel_id;

fn main() {
    let cfg = gateway_config::load("gateway.toml").expect("config");
    let app = Router::new()
        .route("/healthz", get(health))
        .route("/parcels", post(submit_parcel))
        .layer(RateLimit::per_second(cfg.rate_limit_rps));
    serve(app, cfg.port);
}

async fn health() -> &'static str {
    "ok"
}

async fn submit_parcel(body: Parcel) -> String {
    let id = format_parcel_id(&body.tracking_seed);
    forward_to_ledger(&id, body).await;
    id
}
