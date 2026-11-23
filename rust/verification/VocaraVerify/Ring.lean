/-!
# The engine's lock-free ring is a bounded FIFO

Mirrors `vocara_engine::ring` (the SPSC ring between the audio callback and
the recording writer, the meters and the capture pump). The producer owns
`tail`, the consumer `head`; both are machine words that wrap, the occupancy
is `tail.wrapping_sub(head)`, and a slot is `index & (cap - 1)` with `cap` a
power of two.

Proved for the sequential interleavings (each push or pop is one step; the
atomics' acquire/release pairing is what makes the concurrent case refine
these steps and is not modelled here):

* `push_ok` / `push_full`: a push succeeds exactly when the ring holds fewer
  than `cap` items, and appends to the queue;
* `pop_ok` / `pop_empty`: a pop returns the OLDEST item, and only fails when
  the ring is empty;
* so every interleaving of pushes and pops delivers items in order, never more
  than `cap` at a time (`Inv`);
* `wrapping_len` and `wrapping_slot`: the word-sized counters the Rust code
  keeps (mod 2^64) give the same occupancy and slots as the unbounded ones
  modelled here, and `mask_slot`: `i & (cap - 1)` is `i % cap`.
-/

namespace Vocara

structure Ring (α : Type) where
  cap : Nat
  head : Nat
  tail : Nat
  buf : Nat → α

/-- The ring holds exactly the queue `q`, oldest first. -/
structure Inv {α : Type} (r : Ring α) (q : List α) : Prop where
  cap_pos : 0 < r.cap
  len : r.tail = r.head + q.length
  bound : q.length ≤ r.cap
  slots : ∀ i (h : i < q.length), r.buf ((r.head + i) % r.cap) = q[i]

def push {α : Type} (r : Ring α) (v : α) : Option (Ring α) :=
  if r.tail - r.head ≥ r.cap then none
  else some { r with
    buf := fun j => if j = r.tail % r.cap then v else r.buf j,
    tail := r.tail + 1 }

def pop {α : Type} (r : Ring α) : Option (α × Ring α) :=
  if r.head = r.tail then none
  else some (r.buf (r.head % r.cap), { r with head := r.head + 1 })

/-- Two positions closer than `cap` sit in different slots. -/
theorem slot_ne {a b cap : Nat} (hab : a < b) (hd : b - a < cap) :
    a % cap ≠ b % cap := by
  intro h
  have := Nat.sub_mod_eq_zero_of_mod_eq h.symm
  rw [Nat.mod_eq_of_lt hd] at this
  omega

theorem push_full {α : Type} (r : Ring α) (q : List α) (v : α)
    (hi : Inv r q) (hf : q.length = r.cap) : push r v = none := by
  have hl := hi.len
  unfold push
  rw [if_pos (by omega)]

theorem push_ok {α : Type} (r : Ring α) (q : List α) (v : α)
    (hi : Inv r q) (hn : q.length < r.cap) :
    ∃ r', push r v = some r' ∧ Inv r' (q ++ [v]) := by
  have hl := hi.len
  refine ⟨{ r with
      buf := fun j => if j = r.tail % r.cap then v else r.buf j,
      tail := r.tail + 1 }, ?_, ?_⟩
  · unfold push
    rw [if_neg (by omega)]
  · constructor
    · exact hi.cap_pos
    · show r.tail + 1 = r.head + (q ++ [v]).length
      simp; omega
    · show (q ++ [v]).length ≤ r.cap
      simp; omega
    · intro i hi'
      show (if (r.head + i) % r.cap = r.tail % r.cap then v else r.buf ((r.head + i) % r.cap))
        = (q ++ [v])[i]
      by_cases hlt : i < q.length
      · have hne : (r.head + i) % r.cap ≠ r.tail % r.cap :=
          slot_ne (by omega) (by omega)
        rw [if_neg hne, List.getElem_append_left hlt]
        exact hi.slots i hlt
      · have heq : i = q.length := by simp at hi'; omega
        subst heq
        rw [if_pos (by rw [hl])]
        simp

theorem pop_empty {α : Type} (r : Ring α) (hi : Inv r []) : pop r = none := by
  have hl := hi.len
  unfold pop
  rw [if_pos (by simp at hl; omega)]

theorem pop_ok {α : Type} (r : Ring α) (x : α) (q : List α) (hi : Inv r (x :: q)) :
    ∃ r', pop r = some (x, r') ∧ Inv r' q := by
  have hl := hi.len
  have hx := hi.slots 0 (by simp)
  simp only [Nat.add_zero, List.getElem_cons_zero] at hx
  refine ⟨{ r with head := r.head + 1 }, ?_, ?_⟩
  · unfold pop
    rw [if_neg (by simp at hl; omega), hx]
  · constructor
    · exact hi.cap_pos
    · show r.tail = r.head + 1 + q.length
      simp at hl; omega
    · have := hi.bound
      show q.length ≤ r.cap
      simp at this; omega
    · intro i hi'
      have := hi.slots (i + 1) (by simp; omega)
      simp only [List.getElem_cons_succ] at this
      show r.buf ((r.head + 1 + i) % r.cap) = q[i]
      rw [Nat.add_assoc, Nat.add_comm 1 i]
      exact this

/-! ## The word-sized counters -/

/-- Rust's `a.wrapping_sub(b)` on words modulo `M`. -/
def wrappingSub (M a b : Nat) : Nat := (a + M - b) % M

/-- **The wrapping occupancy is the true occupancy** while it is below the
word size (it never exceeds `cap`). -/
theorem wrapping_len (M h d : Nat) (hM : 0 < M) (hd : d < M) :
    wrappingSub M ((h + d) % M) (h % M) = d := by
  unfold wrappingSub
  have hh := Nat.mod_lt h hM
  rw [Nat.add_mod]
  generalize h % M = a at *
  rw [Nat.mod_eq_of_lt hd]
  by_cases hs : a + d < M
  · rw [Nat.mod_eq_of_lt hs]
    have : a + d + M - a = d + M := by omega
    rw [this, Nat.add_mod_right, Nat.mod_eq_of_lt hd]
  · have h1 : (a + d) % M = a + d - M := by
      rw [Nat.mod_eq_sub_mod (by omega), Nat.mod_eq_of_lt (by omega)]
    rw [h1]
    have : a + d - M + M - a = d := by omega
    rw [this, Nat.mod_eq_of_lt hd]

/-- **The wrapped counter picks the same slot**: the word size is a multiple
of the (power-of-two) capacity. -/
theorem wrapping_slot (M cap i : Nat) (hd : cap ∣ M) : (i % M) % cap = i % cap :=
  Nat.mod_mod_of_dvd i hd

/-- **The mask is the remainder**: `i & (cap - 1)` with `cap = 2^k`. -/
theorem mask_slot (i k : Nat) : i &&& (2 ^ k - 1) = i % 2 ^ k :=
  Nat.and_two_pow_sub_one_eq_mod i k

end Vocara
