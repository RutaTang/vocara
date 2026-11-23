import VocaraVerify.Hlc

/-!
# Entity-level last-writer-wins converges

Mirrors the applier's rule (`lib/data/sync/sync_applier.dart`, ported to
`vocara_store::sync::sync_applier`): an incoming record replaces the held row
only when its packed stamp is strictly greater; on a tie the held row stays.
`packed_order` (in `Hlc`) shows the packed TEXT comparison is `hlcCmp`.

Stamps identify writes: a stamp carries the writing device's node, and a
device never reuses one (`tick_gt`, `advance_gt_last`). Under that one
assumption (`StampsDetermine`), this file proves the property sync exists
for: **replicas that received the same writes hold the same row**, whatever
the order of delivery and however often a write was re-delivered
(`convergence`). Idempotence, order independence and re-delivery are the
special cases `lww_idem`, `lww_comm` and `lww_redelivery`.
-/

namespace Vocara

structure Version (α : Type) where
  stamp : Hlc
  payload : α

/-- The applier's decision for one incoming record. -/
def lww {α : Type} (held incoming : Version α) : Version α :=
  if hlcCmp incoming.stamp held.stamp = .gt then incoming else held

/-- Applying a sequence of deliveries to a held row. -/
def applyAll {α : Type} (held : Version α) (deliveries : List (Version α)) : Version α :=
  deliveries.foldl lww held

theorem hlcCmp_self (a : Hlc) : hlcCmp a a = .eq := lexCmp_self _

theorem hlcCmp_swap (a b : Hlc) : hlcCmp b a = (hlcCmp a b).swap := lexCmp_swap _ _

theorem hlcCmp_eq (a b : Hlc) (h : hlcCmp a b = .eq) : a = b := by
  have hk := (lexCmp_eq_iff _ _).mp h
  cases a; cases b
  simp [Hlc.key] at hk
  simp [hk]

theorem hlcCmp_le_trans (a b c : Hlc) (hab : hlcCmp a b ≠ .gt) (hbc : hlcCmp b c ≠ .gt) :
    hlcCmp a c ≠ .gt := lexCmp_le_trans _ _ _ hab hbc

/-- The result of applying deliveries is the held row or one of them. -/
theorem applyAll_mem {α : Type} (held : Version α) (l : List (Version α)) :
    applyAll held l = held ∨ applyAll held l ∈ l := by
  induction l generalizing held with
  | nil => simp [applyAll]
  | cons x xs ih =>
    simp only [applyAll, List.foldl_cons] at ih ⊢
    rcases ih (lww held x) with h | h
    · rw [h]; unfold lww; split
      · right; simp
      · left; rfl
    · right; exact List.mem_cons_of_mem _ h

/-- Nothing the replica saw is newer than what it holds. -/
theorem applyAll_max {α : Type} (held : Version α) (l : List (Version α)) :
    ∀ v, (v = held ∨ v ∈ l) → hlcCmp v.stamp (applyAll held l).stamp ≠ .gt := by
  induction l generalizing held with
  | nil =>
    intro v hv
    simp at hv
    simp [applyAll, hv, hlcCmp_self]
  | cons x xs ih =>
    intro v hv
    simp only [applyAll, List.foldl_cons] at ih ⊢
    have step : ∀ w, (w = held ∨ w = x) → hlcCmp w.stamp (lww held x).stamp ≠ .gt := by
      intro w hw
      unfold lww
      split
      · rename_i hgt
        rcases hw with rfl | rfl
        · rw [hlcCmp_swap, hgt]; simp [Ordering.swap]
        · simp [hlcCmp_self]
      · rename_i hng
        rcases hw with rfl | rfl
        · simp [hlcCmp_self]
        · exact hng
    have top := ih (lww held x) (lww held x) (Or.inl rfl)
    simp only [List.mem_cons] at hv
    rcases hv with rfl | rfl | hv
    · exact hlcCmp_le_trans _ _ _ (step _ (Or.inl rfl)) top
    · exact hlcCmp_le_trans _ _ _ (step _ (Or.inr rfl)) top
    · exact ih (lww held x) v (Or.inr hv)

/-- Stamps identify writes within a set of versions. -/
def StampsDetermine {α : Type} (S : Version α → Prop) : Prop :=
  ∀ v w, S v → S w → v.stamp = w.stamp → v = w

/-- **Convergence.** Two replicas that saw the same versions (their held rows
plus deliveries, in any order, with any repetition) end up holding the same
one. -/
theorem convergence {α : Type} (h1 h2 : Version α) (l1 l2 : List (Version α))
    (hsame : ∀ v, (v = h1 ∨ v ∈ l1) ↔ (v = h2 ∨ v ∈ l2))
    (hdet : StampsDetermine (fun v => v = h1 ∨ v ∈ l1)) :
    applyAll h1 l1 = applyAll h2 l2 := by
  have m1 : applyAll h1 l1 = h1 ∨ applyAll h1 l1 ∈ l1 := applyAll_mem h1 l1
  have m2 : applyAll h2 l2 = h1 ∨ applyAll h2 l2 ∈ l1 :=
    (hsame _).mpr (applyAll_mem h2 l2)
  have le12 := applyAll_max h2 l2 _ ((hsame _).mp m1)
  have le21 := applyAll_max h1 l1 _ m2
  have heq : hlcCmp (applyAll h1 l1).stamp (applyAll h2 l2).stamp = .eq := by
    rw [hlcCmp_swap] at le21
    cases h : hlcCmp (applyAll h1 l1).stamp (applyAll h2 l2).stamp with
    | eq => rfl
    | gt => exact absurd h le12
    | lt => rw [h] at le21; simp [Ordering.swap] at le21
  exact hdet _ _ m1 m2 (hlcCmp_eq _ _ heq)

theorem lww_idem {α : Type} (v : Version α) : lww v v = v := by
  simp [lww, hlcCmp_self]

/-- Two writes arriving in either order leave the same row. -/
theorem lww_comm {α : Type} (c a b : Version α)
    (hdet : StampsDetermine (fun v => v = c ∨ v ∈ [a, b])) :
    lww (lww c a) b = lww (lww c b) a := by
  have := convergence c c [a, b] [b, a]
    (by intro v; simp only [List.mem_cons, List.not_mem_nil, or_false]
        constructor <;> rintro (h | h | h) <;> simp [h]) hdet
  simpa [applyAll] using this

/-- A re-delivered write changes nothing. -/
theorem lww_redelivery {α : Type} (c a : Version α) :
    lww (lww c a) a = lww c a := by
  unfold lww
  by_cases h : hlcCmp a.stamp c.stamp = .gt
  · simp [h, hlcCmp_self]
  · simp [h]

end Vocara
