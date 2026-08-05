import 'sync_row.dart';

/// A transient transport failure (offline, server unreachable, request timed
/// out). The [SyncEngine] treats any exception from a transport call as "try
/// again later": the oplog and the change token are left exactly as they were,
/// so no data is ever lost to a flaky network.
class SyncTransportException implements Exception {
  SyncTransportException(this.message, [this.cause]);

  final String message;
  final Object? cause;

  @override
  String toString() =>
      'SyncTransportException($message${cause == null ? '' : ', cause: $cause'})';
}

/// Thrown by [SyncTransport.fetchChanges] when the server can no longer honor
/// [FetchPage.nextToken]-style resume token it was given — the canonical case
/// is HTTP 410 Gone from a server that compacted its change log past the
/// token. Typed, unlike the generic retry-later [SyncTransportException],
/// because retrying a dead token retries forever; the recovery is a POLICY
/// decision that lives ABOVE the transport (see the class docs): the
/// [SyncEngine] clears the persisted token and re-pulls the zone from
/// scratch, which is safe because the applier is idempotent entity-level LWW.
/// A transport must only throw this — never clear the token itself.
class SyncTokenExpiredException implements Exception {
  SyncTokenExpiredException(this.message);

  final String message;

  @override
  String toString() => 'SyncTokenExpiredException($message)';
}

/// The per-row outcome of a [SyncTransport.pushRows] call.
///
/// [accepted] means the server durably stored [row]. Otherwise the push was
/// rejected because the server already holds a STRICTLY NEWER version of the
/// record by hlcPacked — the only comparison the server ever makes
/// (docs/sync-protocol.md) — and [serverRow] carries that current server
/// state so the caller can resolve. Resolution is the ENGINE's job, not the
/// transport's, and the engine VALIDATES the claim before acting on it — see
/// [SyncTransport.pushRows].
///
/// [row] is a wire ECHO: whatever the transport decoded from the response,
/// nominally the row as pushed. The engine uses it ONLY to correlate the
/// result with the matching row of its own push array (a 1:1 identity match
/// on (table, id)); every decision — conflict validation, retry-or-drop,
/// asset marking — then reads that attempt row, never the echo. This holds
/// even for a future production transport that GUARANTEES [row] is taken
/// verbatim from the request array: the client deliberately does not rely on
/// any such guarantee, because the echo crosses the trust boundary either
/// way and a forged echo would otherwise re-anchor the engine's HLC
/// comparisons (see SyncEngine._pushChunkResolvingConflicts).
class PushRowResult {
  const PushRowResult.accepted(this.row) : serverRow = null;

  const PushRowResult.conflicted(this.row, {required SyncRow this.serverRow});

  /// The wire echo of the pushed row — correlation key ONLY (see the class
  /// docs); decisions bind to the caller's own copy of the row.
  final SyncRow row;

  /// The server's current record when the push conflicted; null when accepted.
  final SyncRow? serverRow;

  bool get accepted => serverRow == null;
}

/// The result of one [SyncTransport.pushRows] call: one [PushRowResult] per
/// pushed row, in the SAME order as the input list.
class PushAck {
  PushAck(List<PushRowResult> results) : results = List.unmodifiable(results);

  final List<PushRowResult> results;

  bool get allAccepted => results.every((r) => r.accepted);

  Iterable<PushRowResult> get conflicts => results.where((r) => !r.accepted);
}

/// One page of remote changes from [SyncTransport.fetchChanges].
class FetchPage {
  FetchPage({
    required List<SyncRow> rows,
    required this.nextToken,
    required this.hasMore,
  }) : rows = List.unmodifiable(rows);

  /// The changed rows in SERVER order (a parent row is always uploaded before
  /// its children by the pushing device, so server order preserves
  /// parent-before-child across page boundaries).
  final List<SyncRow> rows;

  /// The opaque token to persist ATOMICALLY with applying [rows]; passing it
  /// to the next [SyncTransport.fetchChanges] resumes after this page.
  final String nextToken;

  /// Whether more pages are immediately available after [nextToken].
  final bool hasMore;
}

/// The device-to-server seam of the sync engine — the interface a real
/// backend client implements in Phase 1 (an HTTP client against the Vocara
/// sync server; the wire contract lives in docs/sync-protocol.md). P0 ships
/// only the in-memory fake used by tests.
///
/// DELIVERY CONTRACT: everything here is AT-LEAST-ONCE, never exactly-once.
/// A crash between a successful push and the oplog ack re-pushes the same
/// rows; a crash between applying a fetched page and persisting its token
/// re-fetches (and re-applies) the same page; the server itself may re-deliver
/// a page. All of that is safe by construction: rows are full row states keyed
/// by (table, id) and resolved by entity-level HLC Last-Writer-Wins, so a
/// duplicate delivery is a no-op. Implementations must therefore make each
/// call atomic-ish but need NOT dedupe.
///
/// TOKENS are opaque strings minted by the transport ([FetchPage.nextToken]).
/// The engine persists them verbatim and never inspects them. A transport
/// must advance the token whenever it returns rows; returning an unchanged
/// token WITH an empty row list is the "caught up" signal. A transport that
/// can no longer honor a token (e.g. the server compacted its change log past
/// it) must throw [SyncTokenExpiredException] — clearing the token and
/// re-pulling from scratch is a policy decision that lives above the
/// transport (the [SyncEngine]'s pull loop implements it).
abstract class SyncTransport {
  /// Uploads full row states. Returns one result per row, in input order.
  ///
  /// A row the server already holds in a STRICTLY NEWER version comes back
  /// as [PushRowResult.conflicted] with the server's current record — a
  /// conforming server ACCEPTS a push it holds nothing newer than (an older
  /// or equal stored row never conflicts; equal is the idempotent re-push).
  /// The ENGINE resolves, after validating the claim (same identity,
  /// internally consistent HLC, strictly newer — anything else fails the run
  /// as a transport violation, see SyncEngine): a valid conflict means the
  /// server row wins, so the local push is dropped and the newer state
  /// arrives via [fetchChanges]. (The LWW re-push path for a local-newer
  /// conflict is retained in the engine but is unreachable against a
  /// conforming server.)
  ///
  /// Throws (e.g. [SyncTransportException]) on transport failure; the caller
  /// must then leave its ledger untouched and retry later.
  Future<PushAck> pushRows(List<SyncRow> rows);

  /// Fetches changes after [sinceToken] (null = from the beginning of the
  /// zone). See the class docs for the token contract.
  Future<FetchPage> fetchChanges(String? sinceToken);

  /// P2 (asset bytes sync) — uploads the file at [filePath] keyed by its
  /// content hash. Defined now so the engine's asset gate has its release
  /// counterpart on the seam; NOT implemented by the P0 fake.
  Future<void> pushAssetBytes(String contentHash, String filePath);

  /// P2 (asset bytes sync) — downloads the bytes for [contentHash] into
  /// [destPath]. Defined now for the same reason as [pushAssetBytes]; NOT
  /// implemented by the P0 fake.
  Future<void> fetchAssetBytes(String contentHash, String destPath);
}
