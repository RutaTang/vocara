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

## Trust model

The server is a relay the user operates — trusted for availability and
durability, never believed blindly. **An ack IS the persistence claim**: when
the server answers `accepted`, the client deletes the covered oplog entries
on that word alone, so the client validates everything about a response that
is cheaply verifiable before acting on it:

- **Structural row validation** — `SyncRow.fromJson` refuses non-scalar
  fields, mistyped identity, and an envelope that disagrees with the field
  map.
- **HLC quad/packed consistency** — the packed LWW key must re-pack exactly
  from the `hlc_wall`/`hlc_ctr`/`hlc_node` quad (one shared routine on both
  the pull door and the push conflict path), so a forged far-future
  `hlcPacked` cannot ride an honest-looking quad.
- **Push-ack identity and count** — the result list must match the pushed
  rows 1:1 by `(table, id)`; short, wrong-row, or duplicated acks fail the
  run before anything is acked.
- **CAS-reject monotonicity** — a `conflicted` result's `serverRow` must
  exist, carry the same `(table, id)`, be internally consistent, and be
  strictly newer by `hlcPacked` than the pushed row (a conforming server
  accepts anything it holds nothing newer than).
- **Quarantine isolation** — a record failing any client-side gate parks
  alone; one bad record never rolls back a page or stalls the change token.

Anything a violation of these checks could cheat is therefore closed. What
remains is out of the client's defensible model by design: a server that
fabricates an internally-consistent, strictly-newer row is indistinguishable
from one that legitimately holds it, and a server that withholds rows on
pull simply looks caught-up. Both reduce to the durability trust already
placed in the relay — protecting against them would take signed rows, which
is a different protocol.

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

## Derived-stamp discipline (node-space derivation)

Client-side derived writes (integrity repair heals, slot-resolution
tombstones, the FK sweep's collision restore) stamp
`(base.wall, base.counter, base.node + '!' + deviceNode)` — the SAME
(wall, counter) as the row state they derive from, with the node EXTENDED
(`HlcClock.derived`). A prefix extension sorts strictly after the prefix,
so the derived stamp:

* out-ranks `base` — the write replicates over every peer holding base;
* sorts strictly BELOW every stamp that out-ranks base — a held-wall author
  edit at `(wall, counter + 1)`, a node-space tie winner at
  `(wall, counter, biggerNode)`, and any later tick.

Deriving in NODE space (never counter space) closes both halves of the
lost update the earlier drafts documented:

* **counter escalation**: chained repairs under a counter+1 derivation
  stamped base+1 then base+2, and base+2 bookkeeping beat a base+1 REAL
  edit (two scalar heals on one practices row within a single pass reach
  this). A re-derivation now just extends the node again and still loses to
  every real edit above the ORIGINAL base.
* **the restore collision**: a parked update that won its slot by NODE
  alone — `(w, c, B)` over prior `(w, c, A)` — was beaten by a counter+1
  restore tombstone, so its quarantine replay settled as an LWW skip and
  the update was lost forever. `(w, c, A!device)` sits strictly between A
  and B: the tombstone replicates AND the replay lands.

The device suffix keeps derived writes from different devices distinct
(same base, different content must not share one stamp). The node grammar
on the wire is `[0-9A-Za-z!]{1,384}` with `'!'` RESERVED for derivation —
real device nodes are 26 Crockford characters, enforced at mint. Wire
validation also pins the packed encoding's numeric bounds (13-digit wall,
6-digit counter): out-of-range values would "repack consistently" while
their string order diverged from the tuple order.

Do NOT replace the derivation with a fresh clock tick — a fresh-tick stamp
was the original resurrection bug (seed `0xab1a57`): user-grade intent
minted for bookkeeping let a scalar heal out-rank a genuinely later delete.
Regressions for all three failure shapes live in `sync_applier_test.dart`
(derived-node group, NODE-ONLY winner) and `sync_sim_regressions_test.dart`.

**Pre-release note**: this protocol has never shipped; there are no old
clients minting counter+1 derived stamps, so no mixed-version migration is
defined. If the derivation ever changes after release, that changes — a
compatibility story becomes mandatory.

**Server collation requirement**: every LWW comparison depends on BINARY
(byte-wise/ASCII) ordering of `hlc_packed` — `'!' < '0' < 'A'` and so on.
A server-side comparison (SQL `ORDER BY`, unique-conflict resolution) MUST
use binary collation, never a locale collation (which may ignore or reorder
punctuation). The reference server pins `COLLATE BINARY` / bytewise
comparison on every `hlc_packed` touch.

## Known design gap (register)

**Entity-level LWW loses concurrent field edits on one row.** Full-row
snapshots mean the LWW loser's OTHER fields are overwritten even when the
two edits touched disjoint columns. The reserved `fieldClocks` slot is the
upgrade path (column-level LWW); it is NOT implemented — the builder emits
null, the applier ignores it, and no sidecar table exists. The node-space
derivation above removes the bookkeeping-vs-user cases; genuine
user-vs-user concurrent row edits still resolve whole-row. Tracked as an
open design item, not silently absorbed.

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
