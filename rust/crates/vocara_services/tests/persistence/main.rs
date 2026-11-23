//! Persistence's tests: ports of `client/test/`
//! `recording_recovery_test.dart`, `persistence_migration_backup_test.dart`,
//! `persistence_single_instance_test.dart`, the boot, clock-seed, write-gate
//! and repair parts of `persistence_boot_safety_test.dart`,
//! `repair_library_saves_test.dart`, the Persistence case of
//! `database_durability_test.dart`, the compaction half of
//! `oplog_drain_test.dart`'s query-plan case, and the service-level
//! assertion of `settings_screen_test.dart`'s Repair-library flow.
#![cfg(unix)]

mod boot_safety;
mod migration_backup;
mod recording_recovery;
mod repair;
mod single_instance;
mod support;
