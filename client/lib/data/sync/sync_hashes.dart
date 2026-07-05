import '../db/app_database.dart';
import 'sync_writer.dart';

/// An aggregate loaded from the store together with its per-row BASE content
/// hashes: the persisted state of the parent row and of every reconciled
/// child-collection row as of that read, keyed by row id. Row ids are ULIDs
/// (globally unique across tables), so one flat map covers the whole aggregate
/// without collisions.
///
/// The base map is what turns a later save into a THREE-WAY merge instead of a
/// blind two-way diff: reconcile compares BASE (what this editor loaded), MINE
/// (what the editor wants now) and the DB (what is persisted at save time), so
/// a remote change that landed while the aggregate was open is neither
/// reverted nor tombstoned by a stale save. Callers hold the map unchanged
/// between load and save, and swap in the freshly returned map after every
/// successful save.
class Loaded<T> {
  const Loaded(this.aggregate, this.baseHashes);

  final T aggregate;

  /// rowId -> content hash of the row as persisted when [aggregate] was read.
  final Map<String, String> baseHashes;
}

// ---------------------------------------------------------------------------
// ONE content-hash projection per synced entity.
//
// The base/hydrate path and the save path MUST hash through the same function,
// or the two sides silently drift apart and every "unchanged" check breaks
// (phantom updates at best, wrongly-skipped writes at worst). Each entity has:
//   * xxxContentHash(...) — the single definition of the projection: which
//     content columns participate and in which order. Named parameters pin
//     every value to its column so a call site can't reorder fields.
//   * xxxRowHash(row)     — the projection of a loaded drift row, used by both
//     the hydrate path (base hashes) and the save path (current-DB hashes).
// The save path calls xxxContentHash directly with domain-derived values.
//
// Projections cover CONTENT columns only — SyncCols (id/HLC/timestamps/
// tombstone/schema_v) are bookkeeping and never part of content equality.
// JSON columns are compared as their stored STRINGS: both sides produce them
// with the same deterministic encoder, so an unchanged value round-trips
// byte-identically (this is what already made no-op saves emit nothing).
// ---------------------------------------------------------------------------

// --- practices --------------------------------------------------------------

String practiceContentHash({
  required String title,
  required int timelineDurationMs,
  required double masterVolume,
  required String? activeInterpretationId,
  required String? separationAssetId,
  required String? reverbJson,
  required String? lyricsJson,
}) => SyncWriter.contentHashOf([
  title,
  timelineDurationMs,
  masterVolume,
  activeInterpretationId,
  separationAssetId,
  reverbJson,
  lyricsJson,
]);

String practiceRowHash(PracticeRow r) => practiceContentHash(
  title: r.title,
  timelineDurationMs: r.timelineDurationMs,
  masterVolume: r.masterVolume,
  activeInterpretationId: r.activeInterpretationId,
  separationAssetId: r.separationAssetId,
  reverbJson: r.reverbJson,
  lyricsJson: r.lyricsJson,
);

// --- interpretations ---------------------------------------------------------

String interpretationContentHash({
  required String name,
  required int orderIndex,
}) => SyncWriter.contentHashOf([name, orderIndex]);

String interpretationRowHash(InterpretationRow r) =>
    interpretationContentHash(name: r.name, orderIndex: r.orderIndex);

// --- fx_buses ----------------------------------------------------------------

String fxBusContentHash({
  required String name,
  required int? colorValue,
  required double volume,
  required double pan,
  required bool muted,
  required bool soloed,
  required String? outputBusId,
  required String fxJson,
  required String sendsJson,
  required int orderIndex,
}) => SyncWriter.contentHashOf([
  name,
  colorValue,
  volume,
  pan,
  muted,
  soloed,
  outputBusId,
  fxJson,
  sendsJson,
  orderIndex,
]);

/// Row projection. The null fallbacks are the pre-existing row-side semantics:
/// a NULL sends_json equals the save side's empty-list encoding ('[]'), while
/// a NULL fx_json ('' here) can never equal a save-side encoding, so such a
/// legacy/remote row is rewritten once on its next save.
String fxBusRowHash(FxBusRow r) => fxBusContentHash(
  name: r.name,
  colorValue: r.colorValue,
  volume: r.volume,
  pan: r.pan,
  muted: r.muted,
  soloed: r.soloed,
  outputBusId: r.outputBusId,
  fxJson: r.fxJson ?? '',
  sendsJson: r.sendsJson ?? '[]',
  orderIndex: r.orderIndex,
);

// --- tracks ------------------------------------------------------------------

String trackContentHash({
  required String kind,
  required String? interpretationId,
  required String displayName,
  required int? colorValue,
  required bool muted,
  required bool soloed,
  required double volume,
  required double pan,
  required String? busId,
  required String? fxJson,
  required String sendsJson,
  required int orderIndex,
}) => SyncWriter.contentHashOf([
  kind,
  interpretationId,
  displayName,
  colorValue,
  muted,
  soloed,
  volume,
  pan,
  busId,
  fxJson,
  sendsJson,
  orderIndex,
]);

/// Row projection. fx_json passes through as-is (a null row value equals the
/// save side's null for "no FX chain"); a NULL sends_json equals the save
/// side's empty-list encoding ('[]') — both pre-existing semantics, kept.
String trackRowHash(TrackRow r) => trackContentHash(
  kind: r.kind,
  interpretationId: r.interpretationId,
  displayName: r.displayName,
  colorValue: r.colorValue,
  muted: r.muted,
  soloed: r.soloed,
  volume: r.volume,
  pan: r.pan,
  busId: r.busId,
  fxJson: r.fxJson,
  sendsJson: r.sendsJson ?? '[]',
  orderIndex: r.orderIndex,
);

// --- clips ---------------------------------------------------------------

String clipContentHash({
  required String assetId,
  required int startMs,
  required int durationMs,
  required int orderIndex,
}) => SyncWriter.contentHashOf([assetId, startMs, durationMs, orderIndex]);

String clipRowHash(ClipRow r) => clipContentHash(
  assetId: r.assetId,
  startMs: r.startMs,
  durationMs: r.durationMs,
  orderIndex: r.orderIndex,
);

// --- practice_notes ----------------------------------------------------------

String practiceNoteContentHash({
  required int positionMs,
  required int? endMs,
  required String content,
}) => SyncWriter.contentHashOf([positionMs, endMs, content]);

String practiceNoteRowHash(PracticeNoteRow r) => practiceNoteContentHash(
  positionMs: r.positionMs,
  endMs: r.endMs,
  content: r.content,
);

// --- technique_markers -------------------------------------------------------

String techniqueMarkerContentHash({
  required int positionMs,
  required int? endMs,
  required String techniqueId,
  required String techniqueName,
}) => SyncWriter.contentHashOf([positionMs, endMs, techniqueId, techniqueName]);

String techniqueMarkerRowHash(TechniqueMarkerRow r) =>
    techniqueMarkerContentHash(
      positionMs: r.positionMs,
      endMs: r.endMs,
      techniqueId: r.techniqueId,
      techniqueName: r.techniqueNameSnapshot,
    );

// --- techniques ----------------------------------------------------------

String techniqueContentHash({
  required String name,
  required String aliasesJson,
  required String tagsJson,
  required String summary,
  required String description,
  required String cues,
  required String exercises,
  required String acousticSignature,
}) => SyncWriter.contentHashOf([
  name,
  aliasesJson,
  tagsJson,
  summary,
  description,
  cues,
  exercises,
  acousticSignature,
]);

/// Row projection. NULL alias/tag JSON equals the save side's empty-list
/// encoding ('[]') — pre-existing semantics, kept.
String techniqueRowHash(TechniqueRow r) => techniqueContentHash(
  name: r.name,
  aliasesJson: r.aliasesJson ?? '[]',
  tagsJson: r.tagsJson ?? '[]',
  summary: r.summary,
  description: r.description,
  cues: r.cues,
  exercises: r.exercises,
  acousticSignature: r.acousticSignature,
);

// --- technique_media -----------------------------------------------------

String techniqueMediaContentHash({
  required String assetId,
  required String kind,
  required String label,
  required int orderIndex,
}) => SyncWriter.contentHashOf([assetId, kind, label, orderIndex]);

String techniqueMediaRowHash(TechniqueMediaRow r) => techniqueMediaContentHash(
  assetId: r.assetId,
  kind: r.kind,
  label: r.label,
  orderIndex: r.orderIndex,
);
