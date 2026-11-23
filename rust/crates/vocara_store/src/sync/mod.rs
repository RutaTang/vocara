//! The sync machinery: the hybrid logical clock, the local write chokepoint,
//! the oplog, the quarantine and the integrity checker (ported from
//! `lib/data/sync`).

pub mod hlc;
pub mod sync_state_store;
pub mod sync_writer;
