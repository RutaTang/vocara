//! The sync suites: ports of `client/test/sync_row_test.dart`,
//! `sync_applier_test.dart`, `sync_engine_test.dart`,
//! `sync_round_trip_test.dart`, `sync_sim_regressions_test.dart` and the
//! sync parts of `persistence_boot_safety_test.dart`, run against the public
//! API of `vocara_store::sync`.

#[macro_use]
mod support;

mod applier;
mod boot_safety;
mod device;
mod engine;
mod round_trip;
mod row;
mod sim_regressions;
