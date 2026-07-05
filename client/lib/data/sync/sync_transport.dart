import 'sync_row.dart';

/// A transient transport failure (offline, iCloud unreachable, request timed
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

/// The per-row outcome of a [SyncTransport.pushRows] call.
///
/// [accepted] means the server durably stored [row]. Otherwise the push was
/// rejected because the server already holds a DIFFERENT version of the record
/// (CloudKit's `serverRecordChanged`), and [serverRow] carries that current
/// server state so the caller can resolve. Resolution is the ENGINE's job, not
/// the transport's — see [SyncTransport.pushRows].
class PushRowResult {
  const PushRowResult.accepted(this.row) : serverRow = null;

  const PushRowResult.conflicted(this.row, {required SyncRow this.serverRow});

  /// The row as pushed (identifies which input this result is for).
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

/// The device-to-cloud seam of the sync engine — the interface the Swift /
/// CloudKit bridge implements in Phase 1 (P0 ships only the in-memory fake
/// used by tests).
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
/// can no longer honor a token (CloudKit's expired change token) should throw
/// — clearing the token and re-pulling from scratch is a policy decision that
/// lives above the transport.
abstract class SyncTransport {
  /// Uploads full row states. Returns one result per row, in input order.
  ///
  /// A row the server already holds in a DIFFERENT version comes back as
  /// [PushRowResult.conflicted] with the server's current record — CloudKit's
  /// `serverRecordChanged` shape. The ENGINE resolves: if the local row's
  /// hlcPacked is strictly greater than the server row's, it retries the push
  /// (which the server then accepts); otherwise it drops the local push and
  /// lets the newer server state arrive via [fetchChanges].
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
