import 'package:drift/drift.dart';

import 'tables.dart';

part 'app_database.g.dart';

/// The drift/SQLite working store.
///
/// Schema history: v1 greenfield; v2 adds the LOCAL-ONLY sync bookkeeping
/// tables (sync_state, sync_quarantine, asset_sync) — additive only, upgraded
/// on disk by the registered SafeMigrator step (see safe_migrator.dart).
///
/// In-place upgrades are DELIBERATELY refused (see [migration]): the safe
/// schema-migration protocol upgrades a COPY of the database, validates it, and
/// atomically swaps it in, so by the time drift opens the live file its
/// `user_version` already equals [schemaVersion] and drift's own onUpgrade must
/// never run. A firing onUpgrade means the protocol was bypassed.
@DriftDatabase(
  tables: [
    Practices,
    Interpretations,
    Tracks,
    Clips,
    PracticeNotes,
    TechniqueMarkers,
    FxBuses,
    Techniques,
    TechniqueMedia,
    Assets,
    Oplog,
    RecordingsInflight,
    WaveformCache,
    SyncState,
    SyncQuarantine,
    AssetSync,
  ],
)
class AppDatabase extends _$AppDatabase {
  AppDatabase(super.e);

  /// The current code schema version; the SafeMigrator upgrade driver targets
  /// this so persistence wiring can never drift out of step with [schemaVersion].
  static const int currentSchemaVersion = 2;

  @override
  int get schemaVersion => currentSchemaVersion;

  @override
  MigrationStrategy get migration => MigrationStrategy(
    onCreate: (m) async {
      await m.createAll();
      await _createDerivedIndexes();
    },
    onUpgrade: (m, from, to) async {
      // The live database is never opened at a stale version — the
      // copy/migrate/validate/swap protocol guarantees user_version == code
      // schemaVersion first. Reaching here means that protocol was bypassed.
      throw StateError(
        'In-place drift onUpgrade must not run (from $from to $to). The safe '
        'schema-migration protocol owns upgrades.',
      );
    },
    beforeOpen: (details) async {
      await customStatement('PRAGMA foreign_keys = ON');
      await customStatement('PRAGMA journal_mode = WAL');
      // Derived indexes are VERSIONLESS schema: re-applied idempotently on
      // every open (IF NOT EXISTS), not only in onCreate, so an index added
      // in a newer build reaches existing libraries without a schema-version
      // bump or a SafeMigrator step. Cheap when nothing is missing — SQLite
      // resolves each IF NOT EXISTS against the schema table without
      // touching data. (onCreate also runs this for a fresh file; the
      // duplicate call is a no-op.)
      await _createDerivedIndexes();
    },
  );

  /// Indexes + partial-unique constraints that drift's table definitions can't
  /// express directly. Re-run with IF NOT EXISTS so a migration that recreates
  /// the schema stays idempotent; applied from BOTH onCreate and beforeOpen so
  /// a newly added index reaches existing libraries on their next open.
  Future<void> _createDerivedIndexes() async {
    // Single-slot rule: at most one LIVE accompaniment and one LIVE reference
    // track per practice (two offline devices each importing a backing must
    // not produce two live lanes — slot-level LWW resolves the rest).
    await customStatement(
      'CREATE UNIQUE INDEX IF NOT EXISTS ux_track_slot '
      'ON tracks (practice_id, kind) '
      "WHERE deleted_at IS NULL AND kind IN ('accompaniment', 'reference')",
    );
    // Dedup helper (NON-unique on purpose: identical bytes under two asset
    // ULIDs across devices is the normal content-addressed steady state).
    await customStatement(
      'CREATE INDEX IF NOT EXISTS ix_assets_hash ON assets (content_hash)',
    );
    // Hot reads: list practices by recency; load one practice's whole graph;
    // a technique's marker usage; draining the oplog.
    await customStatement(
      'CREATE INDEX IF NOT EXISTS ix_practices_updated '
      'ON practices (deleted_at, updated_at)',
    );
    await customStatement(
      'CREATE INDEX IF NOT EXISTS ix_interp_practice '
      'ON interpretations (practice_id)',
    );
    await customStatement(
      'CREATE INDEX IF NOT EXISTS ix_tracks_practice ON tracks (practice_id)',
    );
    await customStatement(
      'CREATE INDEX IF NOT EXISTS ix_clips_track ON clips (track_id)',
    );
    await customStatement(
      'CREATE INDEX IF NOT EXISTS ix_notes_track ON practice_notes (track_id)',
    );
    await customStatement(
      'CREATE INDEX IF NOT EXISTS ix_markers_track '
      'ON technique_markers (track_id)',
    );
    await customStatement(
      'CREATE INDEX IF NOT EXISTS ix_markers_technique '
      'ON technique_markers (technique_id)',
    );
    await customStatement(
      'CREATE INDEX IF NOT EXISTS ix_buses_practice ON fx_buses (practice_id)',
    );
    await customStatement(
      'CREATE INDEX IF NOT EXISTS ix_media_technique '
      'ON technique_media (technique_id)',
    );
    await customStatement(
      'CREATE INDEX IF NOT EXISTS ix_oplog_unsynced ON oplog (synced, seq)',
    );
    // Boot HLC seeding (Persistence._seedClock) runs SELECT MAX(hlc_packed)
    // over every hlc-carrying table; the synced tables are small, but the
    // oplog gains a row per save and is only trimmed by ack/compaction, so
    // give its scan an index (MAX resolves off the btree tail).
    await customStatement(
      'CREATE INDEX IF NOT EXISTS ix_oplog_hlc ON oplog (hlc_packed)',
    );
    // Per-entity history: the dependency carry-forward in boot compaction
    // (Persistence.compactOplog) and in OplogDrain.ack correlates a subquery
    // on (entity_table, entity_id) for EVERY surviving entry, and the GC's
    // pending-journal guard probes the same pair per asset. Unindexed, each
    // probe scans the whole ledger — quadratic in a ledger nothing acks
    // today (one entry per row ever written): ~6 s at 10k entities, ~24 s at
    // 20k, on every boot before the window shows. seq rides along so the
    // ORDER BY seq DESC LIMIT 1 resolves off the index tail.
    await customStatement(
      'CREATE INDEX IF NOT EXISTS ix_oplog_entity '
      'ON oplog (entity_table, entity_id, seq)',
    );
  }
}
