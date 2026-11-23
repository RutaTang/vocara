//! The sync machinery: the hybrid logical clock, the local write chokepoint,
//! the oplog, the quarantine, the integrity checker, the wire format, the
//! transport seam, the remote door and the engine (ported from
//! `lib/data/sync`).

pub mod hlc;
pub mod integrity_checker;
pub mod oplog_drain;
pub mod quarantine_store;
pub mod reconcile;
pub mod sync_applier;
pub mod sync_engine;
pub mod sync_hashes;
pub mod sync_row;
pub mod sync_state_store;
pub mod sync_transport;
pub mod sync_writer;
