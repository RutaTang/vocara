/-!
# The three-way reconcile of a saved aggregate

Mirrors `reconcile_collection` / `reconcile_op_for`
(`vocara_store::sync::reconcile`, ported from `lib/data/sync/reconcile.dart`):
when an editor saves an aggregate, each child row is decided from three
content hashes —

* `mine`: the row as the editor wants it now (`none` = removed / never held),
* `db`:   the row as persisted at save time (`none` = not live),
* `base`: the row as the editor LOADED it (`none` = never held).

The code states seven rules (plain edit, untouched-under-remote-edit, true
conflict, remote create, remote delete while held, explicit removal, new
row). Proved here: they all collapse to ONE statement —

* `after_spec`: after the save a row holds **`mine` if the editor changed it
  since load, else whatever is persisted** (`if mine = base then db else
  mine`);
* `writes_iff_changes`: a write (insert, update or tombstone) is emitted
  **exactly** when it changes the row: no phantom writes, no missed ones;

and the guarantees the rules exist for, as corollaries:

* `untouched_keeps_persisted`: a row the editor did not touch keeps the
  persisted state — a save never reverts a remote edit or resurrects a
  remote delete it did not see;
* `edits_land`: every change the editor made is what the row ends up as;
* `two_way_when_no_base`: a no-base (legacy) save, `base = db`, is a plain
  overwrite;
* `never_tombstones_unseen`: a row the editor never held is never deleted.
-/

namespace Vocara

/-- One row's three sides (hashes as numbers). -/
structure Row where
  mine : Option Nat
  db : Option Nat
  base : Option Nat
  deriving Repr, DecidableEq

/-- What the reconcile does to one row. `keep` is rule 4's skip (a row
created remotely while the aggregate was open), `skip` the other skips. -/
inductive Op where
  | insert | update | skip | unchanged | tombstone | keep
  deriving Repr, DecidableEq

/-- `reconcile_op_for(mine, db, base)` for a desired row. -/
def opFor (m : Nat) (db base : Option Nat) : Op :=
  match db with
  | none => if base = some m then .skip else .insert
  | some d =>
    if m = d then .unchanged
    else match base with
      | some b => if d ≠ b ∧ m = b then .skip else .update
      | none => .update

/-- The pass over one row id: desired rows go through [opFor]; a live row
missing from desired is tombstoned when the editor held it (rule 6) and kept
otherwise (rule 4); an id live nowhere and not desired is not visited. -/
def classify (r : Row) : Option Op :=
  match r.mine with
  | some m => some (opFor m r.db r.base)
  | none =>
    match r.db, r.base with
    | some _, some _ => some .tombstone
    | some _, none => some .keep
    | none, _ => none

/-- The row as persisted after the pass. -/
def after (r : Row) : Option Nat :=
  match classify r with
  | some .insert | some .update => r.mine
  | some .tombstone => none
  | _ => r.db

/-- Whether the pass emits a write (and a journal entry) for the row. -/
def writes (r : Row) : Bool :=
  match classify r with
  | some .insert | some .update | some .tombstone => true
  | _ => false

theorem after_spec (r : Row) : after r = if r.mine = r.base then r.db else r.mine := by
  rcases r with ⟨_ | m, _ | d, _ | b⟩ <;>
    simp only [after, classify, opFor] <;>
    first
    | (by_cases h1 : m = d <;> by_cases h2 : m = b <;> by_cases h3 : d = b <;> simp_all)
    | (by_cases h1 : m = d <;> simp_all)
    | (by_cases h2 : b = m <;> by_cases h4 : m = b <;> simp_all)
    | simp_all

theorem writes_iff_changes (r : Row) : writes r = true ↔ after r ≠ r.db := by
  rcases r with ⟨_ | m, _ | d, _ | b⟩ <;>
    simp only [writes, after, classify, opFor] <;>
    first
    | (by_cases h1 : m = d <;> by_cases h2 : m = b <;> by_cases h3 : d = b <;> simp_all)
    | (by_cases h1 : m = d <;> simp_all)
    | (by_cases h2 : b = m <;> by_cases h4 : m = b <;> simp_all)
    | simp_all

theorem untouched_keeps_persisted (r : Row) (h : r.mine = r.base) : after r = r.db := by
  rw [after_spec, if_pos h]

theorem edits_land (r : Row) (h : r.mine ≠ r.base) : after r = r.mine := by
  rw [after_spec, if_neg h]

theorem two_way_when_no_base (r : Row) (h : r.base = r.db) : after r = r.mine := by
  rw [after_spec]
  split
  · rename_i hm; rw [hm, h]
  · rfl

theorem never_tombstones_unseen (r : Row) (h : r.base = none) :
    classify r ≠ some .tombstone := by
  rcases r with ⟨_ | m, _ | d, _ | b⟩ <;> simp only [classify, opFor] at * <;>
    first
    | (by_cases h1 : m = d <;> by_cases h2 : m = b <;> by_cases h3 : d = b <;> simp_all)
    | (by_cases h1 : m = d <;> simp_all)
    | (by_cases h2 : b = m <;> by_cases h4 : m = b <;> simp_all)
    | simp_all

end Vocara
