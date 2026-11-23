/-!
# The oplog's asset gate across an ack and a boot compaction

Mirrors `OplogDrain::read_pending` / `OplogDrain::ack`
(`vocara_store::sync::oplog_drain`) and `Persistence::compact_oplog`
(`vocara_services::persistence`), ported from `oplog_drain.dart` and
`persistence.dart`.

Every journaled mutation appends an entry `(seq, table, id, op,
depends_on_asset_id)`. The pusher reads the OLDEST raw entries (a prefix of
the ledger), collapses them to the latest entry per row, and holds a row back
while its resolved dependency's bytes are not uploaded. The collapse resolves
a row's dependency by folding [step] over the row's entries in order: a
TOMBSTONE resolves to its own (explicitly journaled) dependency and inherits
nothing — a delete references no bytes — and any other op keeps its own
dependency or else inherits the one resolved so far.

An ack deletes the prefix the server accepted; a boot compaction deletes all
but each row's latest entry. Either way the deleted entries may be what a
surviving entry's dependency was inherited from, so both first CARRY the
inherited value onto the survivors. Entries of different rows never interact
(the collapse and the carry both correlate on the row), so the model is ONE
row's entries in sequence order; the ack deletes a prefix of them and a read
sees a prefix of what is left.

Proved:

* `ack_preserves_resolve`: after an ack of the first `a` entries, a read
  whose window holds `b ≥ 1` of the row's remaining entries resolves the row
  exactly as a read of the same entries before the ack did, so the gate holds
  a row back iff it would have;
* `compact_preserves_resolve`: the same for the boot compaction;
* `carry_eq_carryOld_without_tombstones`: on a row with no tombstone the
  carry is the one the Dart app shipped (`carryOld`, "the latest non-null
  dependency at or before the entry");
* `carryOld_breaks_the_gate`: with a tombstone, that old carry is wrong. An
  insert gated on an asset, its delete, and a revive with no dependency
  resolve to NO dependency; after an ack of the insert, the old carry hands
  the revive the insert's asset. Once that asset is collected (its row was
  deleted) the revive is held forever, and with it the ack watermark and
  every later entry of the ledger. The Rust carry stops at the latest
  tombstone, as the collapse does.
-/

namespace Vocara

/-- One row's oplog entry: whether it is a tombstone, and its journaled
asset dependency. -/
structure OpEntry where
  tomb : Bool
  dep : Option Nat
  deriving DecidableEq, Repr

/-- One collapse step of `read_pending`: the dependency resolved so far, then
the next entry. -/
def opStep (d : Option Nat) (e : OpEntry) : Option Nat :=
  if e.tomb then e.dep else e.dep.or d

/-- The dependency a read of [xs] resolves the row to, from [d]. -/
def resolve (d : Option Nat) (xs : List OpEntry) : Option Nat := xs.foldl opStep d

/-- The carry: every non-tombstone gets the dependency the collapse resolves
at it (`ACK_CARRY_SQL` / `COMPACT_CARRY_SQL`: the latest non-null dependency
at or after the latest tombstone before the entry). -/
def carry : Option Nat → List OpEntry → List OpEntry
  | _, [] => []
  | d, e :: es =>
    let d' := opStep d e
    { e with dep := if e.tomb then e.dep else d' } :: carry d' es

/-- The carry as the Dart app shipped it: the latest non-null dependency at
or before the entry, tombstones no barrier. -/
def carryOld : Option Nat → List OpEntry → List OpEntry
  | _, [] => []
  | d, e :: es =>
    let d' := e.dep.or d
    { e with dep := if e.tomb then e.dep else d' } :: carryOld d' es

/-- `ack` of the first [a] entries: carry, then delete them. -/
def ackOp (xs : List OpEntry) (a : Nat) : List OpEntry := (carry none xs).drop a

def ackOld (xs : List OpEntry) (a : Nat) : List OpEntry := (carryOld none xs).drop a

/-- The boot compaction: carry, then keep only the latest entry. -/
def compactOp (xs : List OpEntry) : List OpEntry :=
  (carry none xs).drop (xs.length - 1)

def compactOld (xs : List OpEntry) : List OpEntry :=
  (carryOld none xs).drop (xs.length - 1)

/-- Every entry holds the dependency the collapse resolves at it. -/
def Coherent : Option Nat → List OpEntry → Prop
  | _, [] => True
  | d, e :: es => opStep d e = e.dep ∧ Coherent e.dep es

theorem opStep_carried (d : Option Nat) (e : OpEntry) :
    opStep d { e with dep := if e.tomb then e.dep else opStep d e } = opStep d e := by
  cases e with
  | mk t x => cases t <;> cases x <;> cases d <;> simp [opStep, Option.or]

theorem carried_dep (d : Option Nat) (e : OpEntry) :
    (if e.tomb then e.dep else opStep d e) = opStep d e := by
  cases e with
  | mk t x => cases t <;> simp [opStep]

theorem resolve_carry : ∀ (d : Option Nat) (xs : List OpEntry),
    resolve d (carry d xs) = resolve d xs
  | _, [] => rfl
  | d, e :: es => by
    simp only [carry, resolve, List.foldl_cons]
    rw [opStep_carried]
    exact resolve_carry (opStep d e) es

theorem carry_coherent : ∀ (d : Option Nat) (xs : List OpEntry), Coherent d (carry d xs)
  | _, [] => trivial
  | d, e :: es => by
    simp only [carry, Coherent]
    refine ⟨?_, ?_⟩
    · rw [opStep_carried, carried_dep]
    · rw [carried_dep]; exact carry_coherent (opStep d e) es

theorem carry_take : ∀ (d : Option Nat) (xs : List OpEntry) (n : Nat),
    (carry d xs).take n = carry d (xs.take n)
  | _, [], n => by simp [carry]
  | _, _ :: _, 0 => by simp [carry]
  | d, e :: es, n + 1 => by
    simp only [carry, List.take_succ_cons]
    rw [carry_take (opStep d e) es n]

theorem carry_length : ∀ (d : Option Nat) (xs : List OpEntry), (carry d xs).length = xs.length
  | _, [] => rfl
  | d, _ :: es => by simp [carry, carry_length _ es]

/-- A coherent list is coherent from no dependency too: its first entry
resolves to its own dependency whatever came before it. -/
theorem coherent_none : ∀ (d : Option Nat) (xs : List OpEntry), Coherent d xs → Coherent none xs
  | _, [], _ => trivial
  | d, e :: es, ⟨h, hs⟩ => by
    refine ⟨?_, hs⟩
    cases e with
    | mk t x => cases t <;> cases x <;> simp_all [opStep, Option.or]

theorem coherent_take : ∀ (d : Option Nat) (xs : List OpEntry) (n : Nat),
    Coherent d xs → Coherent d (xs.take n)
  | _, [], _, _ => by simp [Coherent]
  | _, _ :: _, 0, _ => by simp [Coherent]
  | _, e :: es, n + 1, ⟨h, hs⟩ => ⟨h, coherent_take e.dep es n hs⟩

/-- The entries after a prefix are coherent from what the prefix resolves. -/
theorem coherent_append : ∀ (d : Option Nat) (p s : List OpEntry),
    Coherent d (p ++ s) → Coherent (resolve d p) s
  | _, [], _, h => h
  | d, e :: es, s, ⟨h, hs⟩ => by
    simp only [resolve, List.foldl_cons]
    rw [h]
    exact coherent_append e.dep es s hs

/-- Two coherent starts resolve a non-empty list alike. -/
theorem resolve_coherent_start (d₁ d₂ : Option Nat) :
    ∀ (s : List OpEntry), s ≠ [] → Coherent d₁ s → Coherent d₂ s →
      resolve d₁ s = resolve d₂ s
  | [], h, _, _ => absurd rfl h
  | e :: es, _, ⟨h₁, _⟩, ⟨h₂, _⟩ => by
    simp only [resolve, List.foldl_cons]
    rw [h₁, h₂]

/-- A non-empty tail of a coherent list resolves from nothing as the whole
list does. -/
theorem resolve_suffix (d : Option Nat) (p s : List OpEntry) (hs : s ≠ [])
    (h : Coherent d (p ++ s)) : resolve none s = resolve d (p ++ s) := by
  have hc := coherent_append d p s h
  have hn := coherent_none _ s hc
  rw [resolve_coherent_start none (resolve d p) s hs hn hc]
  simp [resolve, List.foldl_append]

/-- After an ack of the first [a] entries, a read that sees [b ≥ 1] of the
row's remaining entries resolves the row as a read of those same entries did
before the ack. -/
theorem ack_preserves_resolve (xs : List OpEntry) (a b : Nat) (hb : 0 < b)
    (hlen : a + b ≤ xs.length) :
    resolve none ((ackOp xs a).take b) = resolve none (xs.take (a + b)) := by
  let c := carry none xs
  have hcl : c.length = xs.length := carry_length none xs
  -- The window is the tail of the first a + b carried entries.
  have hw : (c.drop a).take b = (c.take (a + b)).drop a := by
    rw [List.drop_take]; congr 1; omega
  have hsplit : c.take (a + b) = (c.take (a + b)).take a ++ (c.take (a + b)).drop a :=
    (List.take_append_drop a _).symm
  have hne : (c.take (a + b)).drop a ≠ [] := by
    intro h
    have := congrArg List.length h
    simp [List.length_drop, List.length_take] at this
    omega
  have hcoh : Coherent none (c.take (a + b)) := coherent_take none c (a + b) (carry_coherent none xs)
  unfold ackOp
  show resolve none ((c.drop a).take b) = _
  rw [hw]
  have hcoh' : Coherent none ((c.take (a + b)).take a ++ (c.take (a + b)).drop a) := by
    rw [← hsplit]; exact hcoh
  rw [resolve_suffix none _ _ hne hcoh', ← hsplit, carry_take, resolve_carry]

/-- The boot compaction keeps a row's latest entry resolving as the whole
row did. -/
theorem compact_preserves_resolve (xs : List OpEntry) (h : xs ≠ []) :
    resolve none (compactOp xs) = resolve none xs := by
  have hlen : 0 < xs.length := by
    cases xs with
    | nil => exact absurd rfl h
    | cons _ _ => simp
  have := ack_preserves_resolve xs (xs.length - 1) 1 (by decide) (by omega)
  have hc : (ackOp xs (xs.length - 1)).take 1 = compactOp xs := by
    unfold ackOp compactOp
    apply List.take_of_length_le
    simp [List.length_drop, carry_length]
    omega
  rw [← hc, this]
  congr 1
  apply List.take_of_length_le
  omega

/-- Without tombstones the carry is the one the Dart app shipped. -/
theorem carry_eq_carryOld_without_tombstones :
    ∀ (d : Option Nat) (xs : List OpEntry), (∀ e ∈ xs, e.tomb = false) →
      carry d xs = carryOld d xs
  | _, [], _ => rfl
  | d, e :: es, h => by
    have he : e.tomb = false := h e (by simp)
    have hes : ∀ x ∈ es, x.tomb = false := fun x hx => h x (by simp [hx])
    simp only [carry, carryOld, opStep, he]
    simp only [Bool.false_eq_true, if_false]
    rw [carry_eq_carryOld_without_tombstones _ es hes]

/-- An insert gated on asset 7, its delete, a revive with no dependency. -/
def reviveRow : List OpEntry :=
  [⟨false, some 7⟩, ⟨true, none⟩, ⟨false, none⟩]

/-- The old carry breaks the gate: after an ack of the insert the revive is
held on the deleted insert's asset, which the row no longer resolves to; the
boot compaction did the same. -/
theorem carryOld_breaks_the_gate :
    resolve none reviveRow = none ∧
      resolve none (ackOld reviveRow 1) = some 7 ∧
      resolve none (compactOld reviveRow) = some 7 ∧
      resolve none (ackOp reviveRow 1) = none ∧
      resolve none (compactOp reviveRow) = none := by
  decide

end Vocara
