//! ledger: settlement worker for parcelops.

use ids::format_entry_id;

fn main() {
    let cfg = ledger_config::load("ledger.toml").expect("config");
    let consumer = QueueConsumer::connect(&cfg.queue);
    spawn_readiness_probe("/readyz", cfg.port);
    for event in consumer.iter() {
        let entry_id = format_entry_id(&event.parcel_id);
        record_settlement(&entry_id, event);
    }
}
