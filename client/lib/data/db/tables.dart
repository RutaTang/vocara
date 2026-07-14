import 'package:drift/drift.dart';

/// Columns every SYNCED row carries. Mixed into each domain table so identity,
/// the Hybrid Logical Clock (for entity-level Last-Writer-Wins), soft-delete
/// tombstones, and the record schema version are uniform everywhere.
///
/// IMPORTANT: these columns are bookkeeping, NEVER part of a row's content
/// equality — the reconcile-diff compares only a table's domain columns, so a
/// fader nudge that rewrites a sibling's HLC can't make it look "changed".
mixin SyncCols on Table {
  /// Stable ULID primary key; doubles as the wire record key.
  TextColumn get id => text().withLength(min: 26, max: 36)();

  /// Creation / last-modification wall time (epoch ms).
  IntColumn get createdAt => integer()();
  IntColumn get updatedAt => integer()();

  /// Tombstone: NULL = live. Reads filter `deleted_at IS NULL`; deletes set it.
  IntColumn get deletedAt => integer().nullable()();

  /// Hybrid Logical Clock triple (the LWW key). hlcPacked is the fixed-width
  /// lexicographic encoding stored for a one-comparison conflict check.
  IntColumn get hlcWall => integer()();
  IntColumn get hlcCtr => integer()();
  TextColumn get hlcNode => text()();
  TextColumn get hlcPacked => text()();

  /// The app/record schema version that produced this row (cross-device skew
  /// gate + JSON serializer version).
  IntColumn get schemaV => integer().withDefault(const Constant(1))();
}

/// A song practice (the top-level aggregate). Atomic config (reverb, lyric
/// lines) rides as JSON because it is always edited as one unit.
@DataClassName('PracticeRow')
class Practices extends Table with SyncCols {
  TextColumn get title => text().withDefault(const Constant(''))();
  IntColumn get timelineDurationMs =>
      integer().withDefault(const Constant(0))();
  RealColumn get masterVolume => real().withDefault(const Constant(1.0))();

  /// Which interpretation is active. Logical nullable pointer (self-healed by
  /// the IntegrityChecker if it ever resolves to a tombstoned interpretation).
  TextColumn get activeInterpretationId => text().nullable()();

  /// The pre-separation full mix this practice's stems came from (an asset).
  TextColumn get separationAssetId =>
      text().nullable().references(Assets, #id)();

  TextColumn get reverbJson => text().nullable()();
  TextColumn get lyricsJson => text().nullable()();

  @override
  Set<Column> get primaryKey => {id};
}

/// A named interpretation ("reading") grouping a practice's vocal takes.
@DataClassName('InterpretationRow')
class Interpretations extends Table with SyncCols {
  TextColumn get practiceId => text().references(Practices, #id)();
  TextColumn get name => text().withDefault(const Constant(''))();
  IntColumn get orderIndex => integer().withDefault(const Constant(0))();

  @override
  Set<Column> get primaryKey => {id};
}

/// A timeline lane: a vocal take (with an interpretation), or the practice-level
/// accompaniment / reference. Mixer state + FX chain (JSON) live here.
@DataClassName('TrackRow')
class Tracks extends Table with SyncCols {
  TextColumn get practiceId => text().references(Practices, #id)();
  TextColumn get kind => text()(); // vocal | accompaniment | reference
  TextColumn get interpretationId =>
      text().nullable().references(Interpretations, #id)();
  TextColumn get displayName => text().withDefault(const Constant(''))();
  IntColumn get colorValue => integer().nullable()();
  BoolColumn get muted => boolean().withDefault(const Constant(false))();
  BoolColumn get soloed => boolean().withDefault(const Constant(false))();
  RealColumn get volume => real().withDefault(const Constant(1.0))();
  RealColumn get pan => real().withDefault(const Constant(0.0))();
  TextColumn get busId => text().nullable().references(FxBuses, #id)();
  TextColumn get fxJson => text().nullable()();
  TextColumn get sendsJson => text().nullable()();
  IntColumn get orderIndex => integer().withDefault(const Constant(0))();

  @override
  Set<Column> get primaryKey => {id};

  @override
  List<String> get customConstraints => [
    "CHECK (kind IN ('vocal', 'accompaniment', 'reference'))",
  ];
}

/// A clip on a track. References an immutable audio asset (never a path).
@DataClassName('ClipRow')
class Clips extends Table with SyncCols {
  TextColumn get trackId => text().references(Tracks, #id)();
  TextColumn get assetId => text().references(Assets, #id)();
  IntColumn get startMs => integer().withDefault(const Constant(0))();
  IntColumn get durationMs => integer().withDefault(const Constant(0))();
  IntColumn get orderIndex => integer().withDefault(const Constant(0))();

  @override
  Set<Column> get primaryKey => {id};

  @override
  List<String> get customConstraints => ['CHECK (duration_ms >= 0)'];
}

/// A free-text note pinned to a track at a point (or range) on the timeline.
@DataClassName('PracticeNoteRow')
class PracticeNotes extends Table with SyncCols {
  TextColumn get trackId => text().references(Tracks, #id)();
  IntColumn get positionMs => integer().withDefault(const Constant(0))();
  IntColumn get endMs => integer().nullable()();
  TextColumn get content => text().withDefault(const Constant(''))();

  @override
  Set<Column> get primaryKey => {id};

  @override
  List<String> get customConstraints => [
    'CHECK (end_ms IS NULL OR end_ms > position_ms)',
  ];
}

/// A technique annotation pinned to a track. [techniqueId] is a LOGICAL ref
/// into the global library (NOT an enforced FK — it may dangle by design); the
/// name snapshot keeps the marker readable if the technique is gone.
@DataClassName('TechniqueMarkerRow')
class TechniqueMarkers extends Table with SyncCols {
  TextColumn get trackId => text().references(Tracks, #id)();
  IntColumn get positionMs => integer().withDefault(const Constant(0))();
  IntColumn get endMs => integer().nullable()();
  TextColumn get techniqueId => text()();
  TextColumn get techniqueNameSnapshot => text()();

  @override
  Set<Column> get primaryKey => {id};

  @override
  List<String> get customConstraints => [
    'CHECK (end_ms IS NULL OR end_ms > position_ms)',
    "CHECK (technique_name_snapshot <> '')",
  ];
}

/// A practice-level FX group/aux bus tracks can route into.
@DataClassName('FxBusRow')
class FxBuses extends Table with SyncCols {
  TextColumn get practiceId => text().references(Practices, #id)();
  TextColumn get name => text().withDefault(const Constant(''))();
  IntColumn get colorValue => integer().nullable()();
  RealColumn get volume => real().withDefault(const Constant(1.0))();
  RealColumn get pan => real().withDefault(const Constant(0.0))();
  BoolColumn get muted => boolean().withDefault(const Constant(false))();
  BoolColumn get soloed => boolean().withDefault(const Constant(false))();
  TextColumn get outputBusId => text().nullable()();
  TextColumn get fxJson => text().nullable()();
  TextColumn get sendsJson => text().nullable()();
  IntColumn get orderIndex => integer().withDefault(const Constant(0))();

  @override
  Set<Column> get primaryKey => {id};
}

/// A technique in the global, cross-practice library. Alias/tag sets are atomic
/// JSON (mirrored to a local FTS index later).
@DataClassName('TechniqueRow')
class Techniques extends Table with SyncCols {
  TextColumn get name => text().withDefault(const Constant(''))();
  TextColumn get aliasesJson => text().nullable()();
  TextColumn get tagsJson => text().nullable()();
  TextColumn get summary => text().withDefault(const Constant(''))();
  TextColumn get description => text().withDefault(const Constant(''))();
  TextColumn get cues => text().withDefault(const Constant(''))();
  TextColumn get exercises => text().withDefault(const Constant(''))();
  TextColumn get acousticSignature => text().withDefault(const Constant(''))();
  IntColumn get orderIndex => integer().withDefault(const Constant(0))();

  @override
  Set<Column> get primaryKey => {id};
}

/// An example clip belonging to a technique. References an immutable asset.
@DataClassName('TechniqueMediaRow')
class TechniqueMedia extends Table with SyncCols {
  TextColumn get techniqueId => text().references(Techniques, #id)();
  TextColumn get assetId => text().references(Assets, #id)();
  TextColumn get kind => text().withDefault(const Constant('audio'))();
  TextColumn get label => text().withDefault(const Constant(''))();
  IntColumn get orderIndex => integer().withDefault(const Constant(0))();

  @override
  Set<Column> get primaryKey => {id};
}

/// The asset indirection. The RECORD syncs; the BYTES sync out-of-band as a
/// content-addressed blob keyed by [contentHash]. The physical [localRelPath]
/// and the sync state columns are PER-DEVICE (stripped from the wire
/// projection).
@DataClassName('AssetRow')
class Assets extends Table with SyncCols {
  TextColumn get contentHash => text()(); // sha256, dedup + integrity
  IntColumn get byteSize => integer()();
  TextColumn get ext => text().withDefault(const Constant(''))();
  TextColumn get kind => text().withDefault(const Constant('audio'))();

  // Per-device, local-only (not in the synced projection).
  TextColumn get localRelPath => text().nullable()();
  TextColumn get recordSyncState =>
      text().withDefault(const Constant('localOnly'))();
  TextColumn get bytesSyncState =>
      text().withDefault(const Constant('localOnly'))();
  TextColumn get remoteRef => text().nullable()();

  @override
  Set<Column> get primaryKey => {id};

  @override
  List<String> get customConstraints => ['CHECK (byte_size >= 0)'];
}

// ---------------------------------------------------------------------------
// LOCAL-ONLY tables (never synced; no SyncCols). Bookkeeping + derived caches.
// ---------------------------------------------------------------------------

/// The durability ledger / change journal. A row mutation and its oplog entry
/// commit in the SAME transaction (an entry exists iff the mutation committed).
/// The sync pusher drains it; [dependsOnAssetId] holds a row back until that
/// asset's bytes are uploaded.
@DataClassName('OplogRow')
class Oplog extends Table {
  IntColumn get seq => integer().autoIncrement()();
  TextColumn get entityTable => text()();
  TextColumn get entityId => text()();
  TextColumn get op => text()(); // insert | update | tombstone
  TextColumn get hlcPacked => text()();
  TextColumn get payloadHash => text()();
  BoolColumn get synced => boolean().withDefault(const Constant(false))();
  TextColumn get dependsOnAssetId => text().nullable()();
  IntColumn get createdAt => integer()();
}

/// RESERVED — currently unwired: nothing in the app reads or writes this
/// table. It was designed as an in-progress-recording registry (a pid-tied
/// anchor so an age-based sweep could never reclaim a long live recording),
/// but no such protection is needed today: every destructive sweep runs ONLY
/// at boot (Persistence.open), when no recording can be in flight. Kept
/// rather than dropped to avoid schema churn; it MUST be wired up before any
/// sweep is ever scheduled to run while the app is live.
@DataClassName('RecordingInflightRow')
class RecordingsInflight extends Table {
  TextColumn get id => text()(); // the asset ULID being recorded
  TextColumn get practiceId => text()();
  IntColumn get pid => integer()();
  IntColumn get startedAt => integer()();

  @override
  Set<Column> get primaryKey => {id};
}

/// Local-only waveform cache, keyed by (content_hash, algo_version) so it is a
/// pure function of immutable bytes + analysis code version and is regenerated
/// per device — never synced, never stale across app upgrades.
@DataClassName('WaveformCacheRow')
class WaveformCache extends Table {
  TextColumn get contentHash => text()();
  IntColumn get algoVersion => integer()();
  BlobColumn get peaks => blob()();

  @override
  Set<Column> get primaryKey => {contentHash, algoVersion};
}

/// Local-only key-value state for the sync engine: the server change token,
/// pagination cursors and misc bookkeeping. Values are strings (callers encode
/// anything richer as JSON). Writes are plain drift ops so they compose into a
/// caller-provided transaction — the change token MUST be able to commit
/// atomically with the batch it acknowledges (see SyncStateStore).
@DataClassName('SyncStateRow')
class SyncState extends Table {
  TextColumn get key => text()();
  TextColumn get value => text()();

  @override
  Set<Column> get primaryKey => {key};
}

/// Local-only quarantine parking lot: the LATEST rejected remote state per
/// (table, id) — a record whose HLC was implausibly far in the future, or one
/// the local schema could not apply (forward-incompatible). Kept so nothing is
/// silently dropped and a later app upgrade / manual review can re-apply it.
///
/// SAFETY: the remote stamp column is deliberately named remote_hlc_packed,
/// NOT hlc_packed. Persistence._seedClock seeds the boot HLC from MAX of every
/// table carrying an `hlc_packed` column, and the synced-table registry keys
/// off the same SyncCols column names — quarantined stamps are by definition
/// suspect (often far-future), so reusing the `hlc_packed` name would poison
/// the local clock at boot, which is exactly what quarantine exists to prevent.
@DataClassName('SyncQuarantineRow')
class SyncQuarantine extends Table {
  /// The synced table the rejected record targets ("tbl": not a SyncCols name
  /// on purpose, this row is ABOUT a remote record, not a synced row itself).
  TextColumn get tbl => text()();

  /// The rejected record's row id within [tbl].
  TextColumn get id => text()();

  /// The remote record's packed HLC as received (see class doc for the name).
  TextColumn get remoteHlcPacked => text()();

  /// The schema_v the remote record claims (forward-incompat diagnostics).
  IntColumn get schemaV => integer()();

  /// The full record in the SyncRow wire format (JSON), replayable later.
  TextColumn get payload => text()();

  /// Why it was quarantined (human-readable, stable prefix for tests).
  TextColumn get reason => text()();

  /// Wall time (epoch ms) the record was quarantined at.
  IntColumn get quarantinedAt => integer()();

  @override
  Set<Column> get primaryKey => {tbl, id};
}

/// Local-only asset upload markers for Phase 2 of sync: which content hashes
/// this device has durably uploaded (bytes sync out-of-band from records).
/// Created in schema v2 alongside the other sync tables to avoid another
/// migration later; unused until the pusher lands.
@DataClassName('AssetSyncRow')
class AssetSync extends Table {
  TextColumn get hash => text()();
  IntColumn get uploadedAt => integer()();

  @override
  Set<Column> get primaryKey => {hash};
}
