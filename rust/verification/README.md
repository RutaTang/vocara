# Vocara's formal verification (Lean 4)

Lean 4 models of Vocara's core algorithms, with machine-checked proofs of the
properties the app relies on, and a generator of test vectors that the Rust
tests replay against the Rust code. The vectors are what tie a proof to the
code that ships: each model is computed by the same definitions its theorems
are about, and a Rust test fails the moment the Rust code disagrees with it.

| Module | Models | Proved | Replayed by |
|---|---|---|---|
| `Digits` | fixed-width base-*b* digit strings | they sort lexicographically exactly as their numbers (`lexCmp_digits`); `lexCmp` is a lawful total order | (used by the others) |
| `Ulid` | `vocara_model::util::id` | the encoding is order-preserving and injective, 26 characters, first one `0`–`7`; the minter strictly increases whatever the clock does, across the 80-bit spill | `vocara_model/tests/lean_vectors.rs` |
| `Hlc` | `vocara_store::sync::hlc` | `tick` and merge are strictly after the last stamp (and the remote one); the counter keeps six digits; the packed TEXT form orders exactly as the clock | `vocara_store/tests/lean_vectors.rs` |
| `Lww` | the sync applier's entity-level last-writer-wins | replicas that saw the same writes, in any order and with any re-delivery, hold the same row (`convergence`) | `vocara_store/tests/lean_vectors_lww.rs` (through `SyncApplier::apply`) |
| `Version` | `UpdateCheckService.isNewer` | a strict order (irreflexive, asymmetric, transitive): the update banner cannot flip-flop; a GA beats its own prerelease; a prerelease candidate is never offered | `vocara_services/tests/update_check_pins.rs` |
| `Canonical` | `vocara_store::sync::sync_writer` (`content_hash_of`) | canonicalization keeps exactly the entries, sorts keys strictly (by UTF-16 code unit, as Dart's `compareTo`), and gives the same result for two maps with the same entries whatever their key order, so devices agree on a row's hash | `vocara_store/tests/lean_vectors.rs` |
| `Reconcile` | `vocara_store::sync::reconcile` (`reconcile_collection`, `reconcile_op_for`) | the seven three-way rules collapse to one: after a save a row holds the editor's version if the editor changed it since load, else the persisted one; a write is emitted exactly when that changes the row; so an untouched row is never reverted, a no-base save is a plain overwrite, and a row the editor never held is never deleted | `vocara_store/tests/lean_vectors_reconcile.rs` |
| `Lru` | `vocara_model::util::lru_bytes_cache` (the budget's trim) | afterwards the total fits the budget or only the inserted key is left; the inserted key is never evicted (an oversized result is still served whole); the trim only removes, keeping order; eviction takes the least recently used first (the unkept survivors are a suffix of the unkept entries); nothing goes while the total fits | `vocara_model/tests/lean_vectors_lru.rs` (one cache, and three caches sharing a budget) |
| `Ring` | `vocara_engine::ring` (SPSC) | a bounded FIFO: pushes succeed exactly below capacity, pops return the oldest item; the wrapping word counters and the mask index give the same slots and occupancy | `vocara_services/tests/lean_vectors_ring.rs` |

## Building

The toolchain is pinned in `lean-toolchain` (Lean 4.21.0, core library only,
no Mathlib). With `lean` and `lake` on `PATH`:

```sh
lake build                 # checks every proof
lake exe vectors vectors   # regenerates vectors/*.json
```

`lake build` must report no errors and no `sorry` (`grep -rn sorry
VocaraVerify/` finds none). After changing a model, regenerate the vectors and
commit them with the change, so the Rust replays test the new behavior.
