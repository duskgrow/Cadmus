//! Shared id formatting for parcelops.
//!
//! Ids are a fixed lowercase prefix, a hyphen, then 12 lowercase hex
//! characters derived from a stable seed. The prefix says what kind of
//! object the id belongs to.

/// Prefix for parcel ids produced by `format_parcel_id`.
pub const PARCEL_PREFIX: &str = "pcl-";

/// Prefix for settlement entry ids produced by `format_entry_id`.
pub const ENTRY_PREFIX: &str = "led-";

/// Format a parcel id, e.g. `pcl-9f31ab02c7e4`.
pub fn format_parcel_id(seed: &str) -> String {
    format!("{PARCEL_PREFIX}{}", hex_suffix(seed))
}

/// Format a settlement entry id, e.g. `led-04de77a1b3c8`.
pub fn format_entry_id(seed: &str) -> String {
    format!("{ENTRY_PREFIX}{}", hex_suffix(seed))
}

fn hex_suffix(seed: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8222_33a5;
    for b in seed.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:012x}")
}
