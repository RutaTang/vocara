# Vocara Sync Protocol — server wire contract

The client's sync engine (`client/lib/data/sync/`) is transport-agnostic: it
talks to `SyncTransport`, and this document is the contract any real backend
must satisfy. The reference test implementation is
`client/test/support/fake_sync_transport.dart`; the randomized convergence
simulation (`client/test/sync_simulation_test.dart`) is the executable
definition of correct end-to-end behavior.

The server is a **dumb, durable relay**. All conflict resolution (HLC
last-writer-wins), merging, tombstones, and integrity live on the clients.
The server never interprets payloads.

## Data model

One namespace ("zone") per user account. Two stores per zone:

1. **Rows** — latest state per `(table, id)`:
   - `table`: string (client table name)
   - `id`: ULID string (26–36 chars)
   - `hlcPacked`: string — lexicographically comparable hybrid logical clock;
     the ONLY value the server ever compares
   - `schemaV`: int (opaque to the server)
   - `fields`: JSON object — the full row state (opaque to the server)
   - `serverSeq`: int64 — assigned by the server on every accepted write,
     strictly monotonic per zone
2. **Blobs** — immutable, content-addressed by SHA-256 hex; write-once.

A per-zone **change log** ordered by `serverSeq` (the Rows store with an index
on `serverSeq` is sufficient; compaction to latest-per-row is allowed since
rows are full states, but see Ordering below).

## Endpoints

All requests carry `Authorization: Bearer <token>` (account → zone). All
bodies are JSON. Server errors use conventional HTTP codes; clients treat any
transport failure as retry-later (at-least-once; see Idempotency).

### POST /v1/sync/push

Request: `{ "rows": [ SyncRow, ... ] }` — at most 400 rows. `SyncRow` is
`{ table, id, schemaV, hlcPacked, fields, fieldClocks? }` exactly as encoded
by `client/lib/data/sync/sync_row.dart` (`fieldClocks` is reserved, null in
v1; the server stores it verbatim if present).

Per row, in input order, the server MUST:

- If no stored row exists for `(table, id)`, or the stored row's `hlcPacked`
  equals the pushed one (idempotent re-push) or is lexicographically LESS:
  store the pushed row, assign the next `serverSeq`, result `accepted`.
- Otherwise (stored `hlcPacked` is strictly greater): do not store; result
  `conflicted` with the stored row.

Response: `{ "results": [ {"status":"accepted"} |
{"status":"conflicted","serverRow": SyncRow}, ... ] }` — parallel to input.

The client resolves conflicts (re-push if local wins by HLC, else drop and
let pull deliver). The server never merges.

### GET /v1/sync/changes?since=<token>&limit=<n>

Returns `{ "rows": [ SyncRow, ... ], "nextToken": "<token>", "hasMore": bool }`.

- `token` is opaque to the client. Reference implementation: the highest
  `serverSeq` covered, as a decimal string; `since` absent/empty = from zero.
- Rows are returned in ascending `serverSeq` order.
- `nextToken` must advance whenever rows are returned. Returning an unchanged
  token with an empty list is the "caught up" signal.
- The server MUST NOT return a device's own pushes back to it if it can avoid
  it cheaply (optional optimization: track a `deviceId` per push and filter);
  echoes are harmless (LWW no-ops) but waste bandwidth.
- A token the server can no longer honor (log compacted past it) → HTTP 410;
  the client clears its token and re-pulls from scratch (full LWW re-merge is
  safe and idempotent).

### PUT /v1/assets/{sha256}  ·  GET /v1/assets/{sha256}  ·  HEAD /v1/assets/{sha256}

Content-addressed blob store. PUT is write-once: if the hash exists, return
200 without storing (dedup); the server SHOULD verify the body hashes to the
path hash and reject mismatches (400). GET streams the bytes; HEAD checks
existence (the client's lazy-download probe).

## Ordering — the one hard server requirement

**Delivery order must preserve acceptance order** (ascending `serverSeq`,
never reordered across pages), and `serverSeq` must be assigned in request
order within a push batch. The client pushes parents before children
(`SyncRowBuilder.tableRanks`) and relies on this: a puller that receives a
child insert before its parent in a paged fetch can drop the child via the
FK-orphan sweep — found as BUG 1 by simulation seed `0x5eed0001`, see
`client/test/sync_sim_regressions_test.dart`.

If the server ever compacts the change log to latest-per-row, compaction must
preserve relative `serverSeq` order of the surviving rows (it does naturally —
compaction only removes superseded entries).

## Idempotency & crash windows (why at-least-once suffices)

- Client crash between a successful push and its oplog ack → the same rows
  are re-pushed; equal `hlcPacked` → `accepted` again, no new state.
- Client crash between applying a fetched page and persisting the token →
  the page is re-fetched and re-applied; the applier is LWW-idempotent.
- Server loss is survivable WITHOUT backups of the Rows store: any client can
  re-push its full state (all rows are full-state LWW). Blob backups are
  still recommended (audio bytes may exist on only one device).

## Auth (Phase 1 decision)

The transport carries an opaque bearer token; account/identity management is
outside this contract. v1 options, smallest first: manually provisioned
tokens (self-host), email magic link, Sign in with Apple. The client engine
needs only: 401 → surface "signed out" state and pause (mirrors the account
handling the engine already models).

## Non-goals (v1)

- Server-side merging, validation, or schema knowledge (payloads opaque).
- Realtime push (clients poll; SSE/WebSocket is a later additive endpoint
  `GET /v1/sync/events` that just says "something changed, pull now").
- Multi-user sharing (zones are single-account; sharing would be a new
  grant model on top, not a protocol change).
