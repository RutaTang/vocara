import VocaraVerify.Digits

/-!
# Canonical JSON ignores key order

Mirrors `SyncWriter._canonical` (`lib/data/sync/sync_writer.dart`, ported to
`vocara_store::sync::sync_writer`): before a row's content columns are hashed
for sync change detection, every map's entries are sorted by key. Two devices
that serialized the same map with its keys in different orders must compute
the same hash, or every save would look like an edit to the other device.

Model: a map is a list of `(key, value)` entries with distinct keys (keys as
character-code lists; Dart sorts with `String.compareTo`, this
`lexCmp`), and canonicalization is an insertion sort by key. Proved:

* `canon_mem`: canonicalization keeps exactly the entries;
* `canon_sorted`: its keys are strictly increasing;
* `canon_eq_of_same_entries`: **two maps with the same entries canonicalize
  to the same list**, whatever order their keys were inserted in.

Nested maps are canonicalized the same way, level by level, so the result
lifts to whole JSON values.
-/

namespace Vocara

variable {β : Type}

abbrev Entry (β : Type) := List Nat × β

def insertSorted (e : Entry β) : List (Entry β) → List (Entry β)
  | [] => [e]
  | x :: xs => if lexCmp e.1 x.1 = .gt then x :: insertSorted e xs else e :: x :: xs

def canon : List (Entry β) → List (Entry β)
  | [] => []
  | e :: es => insertSorted e (canon es)

/-- Strictly increasing keys. -/
def Sorted : List (Entry β) → Prop
  | [] => True
  | [_] => True
  | x :: y :: rest => lexCmp x.1 y.1 = .lt ∧ Sorted (y :: rest)

/-- Pairwise distinct keys, as in a Dart map. -/
def KeysNodup : List (Entry β) → Prop
  | [] => True
  | e :: es => (∀ y ∈ es, e.1 ≠ y.1) ∧ KeysNodup es

theorem mem_insertSorted (e x : Entry β) (l : List (Entry β)) :
    x ∈ insertSorted e l ↔ x = e ∨ x ∈ l := by
  induction l with
  | nil => simp [insertSorted]
  | cons y ys ih =>
    simp only [insertSorted]
    split
    · simp [ih]; constructor <;> rintro (h | h | h) <;> simp [h]
    · simp

theorem canon_mem (l : List (Entry β)) (x : Entry β) : x ∈ canon l ↔ x ∈ l := by
  induction l with
  | nil => simp [canon]
  | cons e es ih => simp [canon, mem_insertSorted, ih]

theorem lt_trans' {a b c : List Nat} (hab : lexCmp a b = .lt) (hbc : lexCmp b c = .lt) :
    lexCmp a c = .lt := by
  have le1 : lexCmp a b ≠ .gt := by simp [hab]
  have le2 : lexCmp b c ≠ .gt := by simp [hbc]
  have hac := lexCmp_le_trans _ _ _ le1 le2
  cases h : lexCmp a c with
  | lt => rfl
  | gt => exact absurd h hac
  | eq =>
    rw [(lexCmp_eq_iff _ _).mp h] at hab
    rw [lexCmp_swap, hbc] at hab
    simp [Ordering.swap] at hab

/-- The head of a sorted list is below everything after it. -/
theorem Sorted.head_lt {x : Entry β} {xs : List (Entry β)} (h : Sorted (x :: xs)) :
    ∀ y ∈ xs, lexCmp x.1 y.1 = .lt := by
  induction xs generalizing x with
  | nil => simp
  | cons z zs ih =>
    intro y hy
    simp only [Sorted] at h
    simp only [List.mem_cons] at hy
    rcases hy with rfl | hy
    · exact h.1
    · exact lt_trans' h.1 (ih h.2 y hy)

theorem Sorted.tail {x : Entry β} {xs : List (Entry β)} (h : Sorted (x :: xs)) : Sorted xs := by
  cases xs with
  | nil => trivial
  | cons _ _ => exact h.2

theorem sorted_insertSorted (e : Entry β) (l : List (Entry β)) (hs : Sorted l)
    (hk : ∀ y ∈ l, e.1 ≠ y.1) : Sorted (insertSorted e l) := by
  induction l with
  | nil => trivial
  | cons x xs ih =>
    simp only [insertSorted]
    split
    · rename_i hgt
      have ih' := ih hs.tail (fun y hy => hk y (by simp [hy]))
      -- the new head of the tail is e or the old head of xs: both are above x
      cases xs with
      | nil => simp [insertSorted, Sorted]; rw [lexCmp_swap, hgt]; rfl
      | cons z zs =>
        simp only [insertSorted] at ih' ⊢
        split
        · rename_i hgt2
          rw [if_pos hgt2] at ih'
          exact ⟨(hs : _ ∧ _).1, ih'⟩
        · rename_i hgt2
          rw [if_neg hgt2] at ih'
          refine ⟨?_, ih'⟩
          rw [lexCmp_swap, hgt]; rfl
    · rename_i hng
      refine ⟨?_, hs⟩
      cases h : lexCmp e.1 x.1 with
      | lt => rfl
      | gt => exact absurd h hng
      | eq => exact absurd ((lexCmp_eq_iff _ _).mp h) (hk x (by simp))

theorem canon_sorted (l : List (Entry β)) (hd : KeysNodup l) : Sorted (canon l) := by
  induction l with
  | nil => trivial
  | cons e es ih =>
    simp only [canon]
    apply sorted_insertSorted _ _ (ih hd.2)
    intro y hy
    exact hd.1 y ((canon_mem es y).mp hy)

/-- Two sorted lists with the same entries are equal. -/
theorem sorted_unique : ∀ (a b : List (Entry β)), Sorted a → Sorted b →
    (∀ x, x ∈ a ↔ x ∈ b) → a = b
  | [], [], _, _, _ => rfl
  | [], y :: _, _, _, h => absurd ((h y).mpr (by simp)) (by simp)
  | x :: _, [], _, _, h => absurd ((h x).mp (by simp)) (by simp)
  | x :: xs, y :: ys, ha, hb, h => by
    have hxy : x = y := by
      have hx := (h x).mp (by simp)
      have hy := (h y).mpr (by simp)
      simp only [List.mem_cons] at hx hy
      rcases hx with hx | hx
      · exact hx
      · rcases hy with hy | hy
        · exact hy.symm
        · have l1 := Sorted.head_lt hb x hx
          have l2 := Sorted.head_lt ha y hy
          rw [lexCmp_swap, l1] at l2
          simp [Ordering.swap] at l2
    subst hxy
    have rest : ∀ z, z ∈ xs ↔ z ∈ ys := by
      intro z
      have hz := h z
      simp only [List.mem_cons] at hz
      constructor
      · intro hzx
        have hne : z ≠ x := by
          intro e; subst e
          have := Sorted.head_lt ha z hzx
          rw [lexCmp_self] at this; simp at this
        rcases hz.mp (Or.inr hzx) with e | e
        · exact absurd e hne
        · exact e
      · intro hzy
        have hne : z ≠ x := by
          intro e; subst e
          have := Sorted.head_lt hb z hzy
          rw [lexCmp_self] at this; simp at this
        rcases hz.mpr (Or.inr hzy) with e | e
        · exact absurd e hne
        · exact e
    rw [sorted_unique xs ys ha.tail hb.tail rest]

/-- **Same entries, same canonical form**, whatever order the keys were
inserted in. -/
theorem canon_eq_of_same_entries (a b : List (Entry β)) (ha : KeysNodup a) (hb : KeysNodup b)
    (h : ∀ x, x ∈ a ↔ x ∈ b) : canon a = canon b :=
  sorted_unique _ _ (canon_sorted a ha) (canon_sorted b hb)
    (by intro x; rw [canon_mem, canon_mem]; exact h x)

end Vocara
