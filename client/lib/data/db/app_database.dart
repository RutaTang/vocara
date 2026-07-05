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
    },
  );

  /// Indexes + partial-unique constraints that drift's table definitions can't
  /// express directly. Re-run with IF NOT EXISTS so a migration that recreates
  /// the schema stays idempotent.
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
  }
}
