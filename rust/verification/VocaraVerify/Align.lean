/-!
# The lyrics line alignment

Mirrors `align_lines` (`vocara_model::util::lyrics_format`, ported from
`alignLines` in `lib/utils/lyrics_format.dart`), the alignment the lyrics
sync cursor uses to carry each line's time across an edit: for each line of
`next`, the index of the line of `prev` it continues, or `none`.

The code first matches the lines both texts share at the start (`lo` of
them) and at the end (`k` of them), as they stand. The changed middles left
between, `a` of `prev` and `b` of `next`, are aligned by a longest common
subsequence table — unless `|a| * |b|` passes `cell_limit`, when the middle
is left all new. The walk over the table takes a pair of equal lines at
once and, between unequal ones, skips `prev`'s line when that keeps the
common subsequence longest (`down`), else `next`'s.

Model: lines are any type with decidable equality (the Rust compares the
strings, through ids that are equal exactly when the strings are). The table
holds `lcsLen` of every pair of suffixes (`lcsRow`, row by row from the
bottom as the Rust fills it; `lcs_eq`), and the walk reads it as the Rust's
`down` bit does. Proved:

* `lcsLen_spec`: `lcsLen a b` is the length of a longest common subsequence
  (every common subsequence is at most that long, and one is that long);
* `align_length`: one entry per line of `next`;
* `align_sound`: whatever the cell limit, the matched pairs `(i, j)` (line
  `j` of `next` continues line `i` of `prev`) are strictly increasing in
  both texts, and match only equal lines;
* `align_optimal`: within the cell limit the number of matched lines IS the
  LCS length of the whole texts — no alignment in order matches more
  (`sound_le_lcsLen`: any sound matching has at most that many pairs);
* `align_fallback`: past the cell limit exactly the shared start and end are
  matched, nothing in the middle, and what that loses is exactly the LCS of
  the middles: `matched + lcsLen a b = lcsLen prev next`.
-/

namespace Vocara

section Align

variable {α : Type}

/-! ## Matchings -/

/-- The pairs `(i, j)` an alignment matches, `j` counted from `j`. -/
def pairsFrom : Nat → List (Option Nat) → List (Nat × Nat)
  | _, [] => []
  | j, none :: r => pairsFrom (j + 1) r
  | j, some i :: r => (i, j) :: pairsFrom (j + 1) r

/-- The pairs `(i, j)` an alignment matches: line `j` of `next` continues
line `i` of `prev`. -/
def matched (out : List (Option Nat)) : List (Nat × Nat) := pairsFrom 0 out

/-- A matching of `X`'s lines to `Y`'s: strictly increasing in both, and
only equal lines. -/
def Sound (X Y : List α) (P : List (Nat × Nat)) : Prop :=
  P.Pairwise (fun p q => p.1 < q.1 ∧ p.2 < q.2) ∧
    ∀ p ∈ P, ∃ v, X[p.1]? = some v ∧ Y[p.2]? = some v

def shift (di dj : Nat) (p : Nat × Nat) : Nat × Nat := (p.1 + di, p.2 + dj)

theorem sound_nil (X Y : List α) : Sound X Y [] := by simp [Sound]

theorem sound_append {X X' Y Y' : List α} {P Q : List (Nat × Nat)}
    (hP : Sound X Y P) (hQ : Sound X' Y' Q) :
    Sound (X ++ X') (Y ++ Y') (P ++ Q.map (shift X.length Y.length)) := by
  obtain ⟨hP1, hP2⟩ := hP
  obtain ⟨hQ1, hQ2⟩ := hQ
  refine ⟨?_, ?_⟩
  · rw [List.pairwise_append]
    refine ⟨hP1, ?_, ?_⟩
    · rw [List.pairwise_map]
      exact hQ1.imp (fun h => by simp only [shift]; omega)
    · intro p hp q hq
      obtain ⟨v, h1, h2⟩ := hP2 p hp
      rw [List.mem_map] at hq
      obtain ⟨q', _, rfl⟩ := hq
      have := (List.getElem?_eq_some_iff.mp h1).1
      have := (List.getElem?_eq_some_iff.mp h2).1
      simp only [shift]; omega
  · intro p hp
    rw [List.mem_append] at hp
    rcases hp with hp | hp
    · obtain ⟨v, h1, h2⟩ := hP2 p hp
      have l1 := (List.getElem?_eq_some_iff.mp h1).1
      have l2 := (List.getElem?_eq_some_iff.mp h2).1
      exact ⟨v, by rw [List.getElem?_append_left l1, h1], by rw [List.getElem?_append_left l2, h2]⟩
    · rw [List.mem_map] at hp
      obtain ⟨q, hq, rfl⟩ := hp
      obtain ⟨v, h1, h2⟩ := hQ2 q hq
      refine ⟨v, ?_, ?_⟩
      · simp only [shift]
        rw [List.getElem?_append_right (by omega)]
        simpa using h1
      · simp only [shift]
        rw [List.getElem?_append_right (by omega)]
        simpa using h2

theorem pairsFrom_append (j : Nat) (A B : List (Option Nat)) :
    pairsFrom j (A ++ B) = pairsFrom j A ++ pairsFrom (j + A.length) B := by
  induction A generalizing j with
  | nil => simp [pairsFrom]
  | cons o r ih =>
    cases o with
    | none =>
      simp only [List.cons_append, pairsFrom, ih, List.length_cons]
      rw [Nat.add_assoc, Nat.add_comm 1]
    | some v =>
      simp only [List.cons_append, pairsFrom, ih, List.length_cons, List.cons_append]
      rw [Nat.add_assoc, Nat.add_comm 1]

/-- Shifting the entries and the start shifts the pairs. -/
theorem pairsFrom_map (j i : Nat) (l : List (Option Nat)) :
    pairsFrom j (l.map (Option.map (· + i))) = (pairsFrom 0 l).map (shift i j) := by
  suffices h : ∀ j', pairsFrom (j' + j) (l.map (Option.map (· + i))) =
      (pairsFrom j' l).map (shift i j) by simpa using h 0
  induction l with
  | nil => simp [pairsFrom]
  | cons o r ih =>
    intro j'
    have e := ih (j' + 1)
    rw [show j' + 1 + j = j' + j + 1 by omega] at e
    cases o with
    | none => simp only [List.map_cons, Option.map_none, pairsFrom, e]
    | some v => simp only [List.map_cons, Option.map_some, pairsFrom, e, shift]

theorem pairsFrom_eq_map (j : Nat) (l : List (Option Nat)) :
    pairsFrom j l = (pairsFrom 0 l).map (shift 0 j) := by
  have := pairsFrom_map j 0 l
  simpa using this

theorem pairsFrom_replicate_none (j n : Nat) : pairsFrom j (List.replicate n none) = [] := by
  induction n generalizing j with
  | zero => rfl
  | succ n ih => simp [List.replicate_succ, pairsFrom, ih]

variable [DecidableEq α]

/-! ## The longest common subsequence -/

/-- The LCS length, by its recurrence (the specification). -/
def lcsLen : List α → List α → Nat
  | [], _ => 0
  | _ :: _, [] => 0
  | x :: xs, y :: ys =>
    if x = y then lcsLen xs ys + 1 else max (lcsLen xs (y :: ys)) (lcsLen (x :: xs) ys)

@[simp] theorem lcsLen_nil_left (b : List α) : lcsLen [] b = 0 := by
  cases b <;> simp [lcsLen]

@[simp] theorem lcsLen_nil_right (a : List α) : lcsLen a [] = 0 := by
  cases a <;> simp [lcsLen]

theorem lcsLen_cons (x y : α) (xs ys : List α) :
    lcsLen (x :: xs) (y :: ys) =
      if x = y then lcsLen xs ys + 1 else max (lcsLen xs (y :: ys)) (lcsLen (x :: xs) ys) := by
  simp [lcsLen]

/-- Every common subsequence is at most `lcsLen` long. -/
theorem lcsLen_ge (a b c : List α) (ha : c.Sublist a) (hb : c.Sublist b) :
    c.length ≤ lcsLen a b := by
  induction a generalizing b c with
  | nil => simp [List.sublist_nil.mp ha]
  | cons x xs iha =>
    induction b generalizing c with
    | nil => simp [List.sublist_nil.mp hb]
    | cons y ys ihb =>
      rw [lcsLen_cons]
      cases c with
      | nil => simp
      | cons z zs =>
        by_cases hxy : x = y
        · subst hxy
          simp only [if_true]
          have h1 : zs.Sublist xs := by
            rcases List.sublist_cons_iff.mp ha with h | ⟨r, hr, h⟩
            · exact (List.sublist_cons_self z zs).trans h
            · obtain ⟨-, rfl⟩ := List.cons.inj hr; exact h
          have h2 : zs.Sublist ys := by
            rcases List.sublist_cons_iff.mp hb with h | ⟨r, hr, h⟩
            · exact (List.sublist_cons_self z zs).trans h
            · obtain ⟨-, rfl⟩ := List.cons.inj hr; exact h
          simpa using iha ys zs h1 h2
        · simp only [hxy, if_false]
          rcases List.sublist_cons_iff.mp ha with h | ⟨r, hr, h⟩
          · exact Nat.le_trans (iha (y :: ys) _ h hb) (Nat.le_max_left _ _)
          · rcases List.sublist_cons_iff.mp hb with h' | ⟨r', hr', _⟩
            · exact Nat.le_trans (ihb _ ha h') (Nat.le_max_right _ _)
            · simp at hr hr'
              exact absurd (hr.1.symm.trans hr'.1) hxy

/-- Some common subsequence is `lcsLen` long. -/
theorem lcsLen_witness (a b : List α) :
    ∃ c : List α, c.Sublist a ∧ c.Sublist b ∧ c.length = lcsLen a b := by
  induction a generalizing b with
  | nil => exact ⟨[], by simp, by simp, by simp⟩
  | cons x xs iha =>
    induction b with
    | nil => exact ⟨[], by simp, by simp, by simp⟩
    | cons y ys ihb =>
      rw [lcsLen_cons]
      by_cases hxy : x = y
      · subst hxy
        obtain ⟨c, h1, h2, h3⟩ := iha ys
        exact ⟨x :: c, h1.cons₂ x, h2.cons₂ x, by simp [h3]⟩
      · simp only [hxy, if_false]
        by_cases hle : lcsLen (x :: xs) ys ≤ lcsLen xs (y :: ys)
        · obtain ⟨c, h1, h2, h3⟩ := iha (y :: ys)
          exact ⟨c, h1.cons x, h2, by rw [h3]; omega⟩
        · obtain ⟨c, h1, h2, h3⟩ := ihb
          exact ⟨c, h1, h2.cons y, by rw [h3]; omega⟩

/-- `lcsLen a b` is the length of a longest common subsequence. -/
theorem lcsLen_spec (a b : List α) :
    (∀ c : List α, c.Sublist a → c.Sublist b → c.length ≤ lcsLen a b) ∧
      ∃ c : List α, c.Sublist a ∧ c.Sublist b ∧ c.length = lcsLen a b :=
  ⟨lcsLen_ge a b, lcsLen_witness a b⟩

theorem lcsLen_reverse (a b : List α) : lcsLen a.reverse b.reverse = lcsLen a b := by
  apply Nat.le_antisymm
  · obtain ⟨c, h1, h2, h3⟩ := lcsLen_witness a.reverse b.reverse
    rw [← h3, ← List.length_reverse]
    apply lcsLen_ge
    · have : c.reverse.reverse.Sublist a.reverse := by simpa using h1
      exact List.reverse_sublist.mp this
    · have : c.reverse.reverse.Sublist b.reverse := by simpa using h2
      exact List.reverse_sublist.mp this
  · obtain ⟨c, h1, h2, h3⟩ := lcsLen_witness a b
    rw [← h3, ← List.length_reverse]
    exact lcsLen_ge _ _ _ (List.reverse_sublist.mpr h1) (List.reverse_sublist.mpr h2)

theorem lcsLen_prefix (p a b : List α) : lcsLen (p ++ a) (p ++ b) = p.length + lcsLen a b := by
  induction p with
  | nil => simp
  | cons x xs ih => simp [lcsLen_cons, ih]; omega

theorem lcsLen_suffix (a b s : List α) : lcsLen (a ++ s) (b ++ s) = lcsLen a b + s.length := by
  rw [← lcsLen_reverse, List.reverse_append, List.reverse_append, lcsLen_prefix, lcsLen_reverse,
    List.length_reverse]
  omega

/-- Adding a line to one side adds at most one to the LCS. -/
theorem lcsLen_cons_left_le (x : α) (xs b : List α) : lcsLen (x :: xs) b ≤ lcsLen xs b + 1 := by
  obtain ⟨c, h1, h2, h3⟩ := lcsLen_witness (x :: xs) b
  rw [← h3]
  have : c.tail.Sublist xs := by
    rcases List.sublist_cons_iff.mp h1 with h | ⟨r, hr, h⟩
    · exact (List.tail_sublist c).trans h
    · subst hr; exact h
  have := lcsLen_ge xs b c.tail this ((List.tail_sublist c).trans h2)
  simp at this; omega

theorem lcsLen_cons_right_le (y : α) (a ys : List α) : lcsLen a (y :: ys) ≤ lcsLen a ys + 1 := by
  obtain ⟨c, h1, h2, h3⟩ := lcsLen_witness a (y :: ys)
  rw [← h3]
  have : c.tail.Sublist ys := by
    rcases List.sublist_cons_iff.mp h2 with h | ⟨r, hr, h⟩
    · exact (List.tail_sublist c).trans h
    · subst hr; exact h
  have := lcsLen_ge a ys c.tail ((List.tail_sublist c).trans h1) this
  simp at this; omega

/-! ## The table -/

/-- One row of the table from the row below it (`row` from `below`, filled
right to left): `below` holds `lcsLen xs` of every suffix of `b`, longest
first, and the result `lcsLen (x :: xs)` of them. -/
def stepRow (x : α) : List α → List Nat → List Nat
  | [], _ => [0]
  | y :: ys, below =>
    let row := stepRow x ys below.tail
    (if x = y then below.tail.headD 0 + 1 else max (below.headD 0) (row.headD 0)) :: row

/-- The table's top row for `a`: `lcsLen a` of every suffix of `b`. -/
def lcsRow : List α → List α → List Nat
  | [], b => List.replicate (b.length + 1) 0
  | x :: xs, b => stepRow x b (lcsRow xs b)

/-- The table's entry for `a` and `b`. -/
def lcs (a b : List α) : Nat := (lcsRow a b).headD 0

/-- Every suffix of a list, longest first. -/
def suffixes : List α → List (List α)
  | [] => [[]]
  | y :: ys => (y :: ys) :: suffixes ys

omit [DecidableEq α] in
theorem suffixes_eq (b : List α) : suffixes b = b :: (suffixes b).tail := by
  cases b <;> rfl

theorem stepRow_spec (x : α) (xs b : List α) :
    stepRow x b ((suffixes b).map (lcsLen xs)) = (suffixes b).map (lcsLen (x :: xs)) := by
  induction b with
  | nil => simp [stepRow, suffixes]
  | cons y ys ih =>
    simp only [suffixes, List.map_cons, stepRow, List.tail_cons, ih, List.headD_cons]
    rw [suffixes_eq ys]
    simp [lcsLen_cons]

theorem lcsRow_spec (a b : List α) : lcsRow a b = (suffixes b).map (lcsLen a) := by
  induction a with
  | nil =>
    simp only [lcsRow, lcsLen_nil_left]
    induction b with
    | nil => simp [suffixes]
    | cons y ys ih => simp [suffixes, ih, List.replicate_succ]
  | cons x xs ih => simp [lcsRow, ih, stepRow_spec]

theorem lcs_eq (a b : List α) : lcs a b = lcsLen a b := by
  rw [lcs, lcsRow_spec, suffixes_eq]
  rfl

/-! ## The walk -/

/-- The walk over the table (the Rust's last loop), from `prev`'s line `i`:
for each line of `b`, `some` the index of the line of `a` it continues or
`none`. Equal lines match at once; between unequal ones `prev`'s line is
skipped when that keeps the common subsequence longest (`down`). -/
def walk (i : Nat) : List α → List α → List (Option Nat)
  | _, [] => []
  | [], _ :: ys => none :: walk i [] ys
  | x :: xs, y :: ys =>
    if x = y then some i :: walk (i + 1) xs ys
    else if lcs xs (y :: ys) ≥ lcs (x :: xs) ys then walk (i + 1) xs (y :: ys)
    else none :: walk i (x :: xs) ys

/-! ## The alignment -/

/-- How many lines two texts share at the start. -/
def commonPrefix : List α → List α → Nat
  | x :: xs, y :: ys => if x = y then commonPrefix xs ys + 1 else 0
  | _, _ => 0

/-- The shared start `lo`, the shared end `k` (of what is left after the
start), and the middles left between them. -/
structure Middle (α : Type) where
  lo : Nat
  k : Nat
  a : List α
  b : List α

def middle (prev next : List α) : Middle α :=
  let lo := commonPrefix prev next
  let p := prev.drop lo
  let q := next.drop lo
  let k := commonPrefix p.reverse q.reverse
  ⟨lo, k, p.take (p.length - k), q.take (q.length - k)⟩

/-- `align_lines(prev, next, cell_limit)`. -/
def alignLines (prev next : List α) (cellLimit : Nat) : List (Option Nat) :=
  let s := middle prev next
  let mid :=
    if s.a.length = 0 ∨ s.b.length = 0 ∨ cellLimit < s.a.length * s.b.length then
      List.replicate s.b.length none
    else walk s.lo s.a s.b
  (List.range s.lo).map some ++ mid ++ (List.range s.k).map (fun t => some (s.lo + s.a.length + t))

theorem walk_add (i d : Nat) (a b : List α) :
    walk (i + d) a b = (walk i a b).map (Option.map (· + d)) := by
  induction a generalizing i b with
  | nil =>
    induction b with
    | nil => simp [walk]
    | cons y ys ih => simp [walk, ih]
  | cons x xs iha =>
    induction b generalizing i with
    | nil => simp [walk]
    | cons y ys ihb =>
      simp only [walk]
      by_cases hxy : x = y
      · simp only [hxy, if_true, List.map_cons, Option.map_some]
        rw [show i + d + 1 = (i + 1) + d by omega, iha (i + 1)]
      · simp only [hxy, if_false]
        split
        · rw [show i + d + 1 = (i + 1) + d by omega, iha (i + 1)]
        · simp only [List.map_cons, Option.map_none, ihb i]

/-- The walk from `prev`'s line `i` is the walk from line 0, shifted. -/
theorem walk_shift (i : Nat) (a b : List α) :
    walk i a b = (walk 0 a b).map (Option.map (· + i)) := by
  simpa using walk_add 0 i a b

theorem walk_length (i : Nat) (a b : List α) : (walk i a b).length = b.length := by
  fun_induction walk i a b <;> simp_all

theorem walk_nil_pairs (j : Nat) (b : List α) : pairsFrom j (walk 0 [] b) = [] := by
  induction b generalizing j with
  | nil => simp [walk, pairsFrom]
  | cons y ys ih => simp [walk, pairsFrom, ih]

/-- The pairs of the walk's three steps: a match, a skip of `prev`'s line,
a skip of `next`'s. -/
theorem walk_pairs_match (x : α) (xs ys : List α) :
    matched (walk 0 (x :: xs) (x :: ys)) = (0, 0) :: (matched (walk 0 xs ys)).map (shift 1 1) := by
  simp only [matched, walk, if_true, pairsFrom]
  rw [walk_shift, pairsFrom_map]

theorem walk_pairs_down (x y : α) (xs ys : List α) (hxy : x ≠ y)
    (h : lcs xs (y :: ys) ≥ lcs (x :: xs) ys) :
    matched (walk 0 (x :: xs) (y :: ys)) = (matched (walk 0 xs (y :: ys))).map (shift 1 0) := by
  simp only [matched, walk, hxy, if_false, h, if_true]
  rw [walk_shift, pairsFrom_map]

theorem walk_pairs_right (x y : α) (xs ys : List α) (hxy : x ≠ y)
    (h : ¬ lcs xs (y :: ys) ≥ lcs (x :: xs) ys) :
    matched (walk 0 (x :: xs) (y :: ys)) = (matched (walk 0 (x :: xs) ys)).map (shift 0 1) := by
  simp only [matched, walk, hxy, if_false, h, pairsFrom]
  rw [pairsFrom_eq_map]

/-- What the walk matches is a sound matching. -/
theorem walk_sound (a b : List α) : Sound a b (matched (walk 0 a b)) := by
  induction a generalizing b with
  | nil => simp [matched, walk_nil_pairs, sound_nil]
  | cons x xs iha =>
    induction b with
    | nil => simp [matched, walk, pairsFrom, sound_nil]
    | cons y ys ihb =>
      by_cases hxy : x = y
      · subst hxy
        rw [walk_pairs_match]
        simpa using sound_append (X := [x]) (Y := [x]) (P := [(0, 0)]) ⟨by simp, by simp⟩ (iha ys)
      · by_cases h : lcs xs (y :: ys) ≥ lcs (x :: xs) ys
        · rw [walk_pairs_down x y xs ys hxy h]
          simpa using sound_append (X := [x]) (Y := []) (P := []) (sound_nil _ _) (iha (y :: ys))
        · rw [walk_pairs_right x y xs ys hxy h]
          simpa using sound_append (X := []) (Y := [y]) (P := []) (sound_nil _ _) ihb

/-- The walk matches as many lines as the LCS. -/
theorem walk_count (a b : List α) : (matched (walk 0 a b)).length = lcsLen a b := by
  induction a generalizing b with
  | nil => simp [matched, walk_nil_pairs]
  | cons x xs iha =>
    induction b with
    | nil => simp [matched, walk, pairsFrom]
    | cons y ys ihb =>
      rw [lcsLen_cons]
      by_cases hxy : x = y
      · subst hxy
        rw [walk_pairs_match]
        simp [iha ys]
      · simp only [hxy, if_false]
        by_cases h : lcs xs (y :: ys) ≥ lcs (x :: xs) ys
        · rw [walk_pairs_down x y xs ys hxy h, List.length_map, iha (y :: ys)]
          rw [lcs_eq, lcs_eq] at h
          omega
        · rw [walk_pairs_right x y xs ys hxy h, List.length_map, ihb]
          rw [lcs_eq, lcs_eq] at h
          omega

/-! ## The shared start and end -/

theorem commonPrefix_le (a b : List α) : commonPrefix a b ≤ a.length ∧ commonPrefix a b ≤ b.length := by
  induction a generalizing b with
  | nil => simp [commonPrefix]
  | cons x xs ih =>
    cases b with
    | nil => simp [commonPrefix]
    | cons y ys =>
      simp only [commonPrefix]
      split
      · have := ih ys; simp; omega
      · simp

theorem commonPrefix_take (a b : List α) : a.take (commonPrefix a b) = b.take (commonPrefix a b) := by
  induction a generalizing b with
  | nil => simp [commonPrefix]
  | cons x xs ih =>
    cases b with
    | nil => simp [commonPrefix]
    | cons y ys =>
      simp only [commonPrefix]
      split
      · rename_i h; subst h; simp [ih ys]
      · simp

/-- The texts are the shared start, the middles, and the shared end. -/
theorem middle_split (prev next : List α) (s : Middle α) (hs : middle prev next = s) :
    ∃ pre suf, prev = pre ++ s.a ++ suf ∧ next = pre ++ s.b ++ suf ∧
      pre.length = s.lo ∧ suf.length = s.k := by
  subst hs
  simp only [middle]
  generalize hlo : commonPrefix prev next = lo
  have h1 := commonPrefix_le prev next
  have ht := commonPrefix_take prev next
  rw [hlo] at h1 ht
  generalize hp : prev.drop lo = p
  generalize hq : next.drop lo = q
  generalize hk : commonPrefix p.reverse q.reverse = k
  have h2 := commonPrefix_le p.reverse q.reverse
  have ht2 := commonPrefix_take p.reverse q.reverse
  rw [hk] at h2 ht2
  simp only [List.length_reverse] at h2
  refine ⟨prev.take lo, p.drop (p.length - k), ?_, ?_, ?_, ?_⟩
  · rw [List.append_assoc, List.take_append_drop, ← hp, List.take_append_drop]
  · have : p.drop (p.length - k) = q.drop (q.length - k) := by
      rw [List.take_reverse, List.take_reverse] at ht2
      simpa using congrArg List.reverse ht2
    rw [this, List.append_assoc, List.take_append_drop, ← hq, ht, List.take_append_drop]
  · simp; omega
  · simp; omega

/-- The pairs of `n` lines matched as they stand. -/
def diag (n : Nat) : List (Nat × Nat) := (List.range n).map fun t => (t, t)

theorem pairsFrom_range (j n : Nat) :
    pairsFrom j ((List.range n).map some) = (diag n).map (shift 0 j) := by
  induction n with
  | zero => rfl
  | succ n ih =>
    simp only [diag, List.range_succ, List.map_append, List.map_cons, List.map_nil,
      pairsFrom_append, ih, List.length_map, List.length_range, pairsFrom, shift]
    simp [diag, shift, Nat.add_comm]

omit [DecidableEq α] in
theorem diag_sound (X : List α) : Sound X X (diag X.length) := by
  refine ⟨?_, ?_⟩
  · simp only [diag, List.pairwise_map]
    exact List.pairwise_lt_range.imp (fun h => ⟨h, h⟩)
  · intro p hp
    simp only [diag, List.mem_map, List.mem_range] at hp
    obtain ⟨t, ht, rfl⟩ := hp
    exact ⟨X[t], by simp [ht], by simp [ht]⟩

/-! ## The theorems -/

theorem align_length (prev next : List α) (c : Nat) :
    (alignLines prev next c).length = next.length := by
  obtain ⟨pre, suf, _, hn, hl, hk⟩ := middle_split prev next _ rfl
  simp only [alignLines]
  generalize middle prev next = s at *
  rw [hn]
  split <;> simp [walk_length, hl, hk] <;> omega

/-- The matched pairs of the alignment, part by part: the shared start, the
middle (`none` when it is left all new), the shared end. -/
def alignPairs (s : Middle α) (mid : Option (List (Nat × Nat))) : List (Nat × Nat) :=
  diag s.lo ++ (mid.getD []).map (shift s.lo s.lo) ++
    (diag s.k).map (shift (s.lo + s.a.length) (s.lo + s.b.length))

theorem align_matched (prev next : List α) (c : Nat) (s : Middle α) (hs : middle prev next = s) :
    matched (alignLines prev next c) = alignPairs s
      (if s.a.length = 0 ∨ s.b.length = 0 ∨ c < s.a.length * s.b.length then none
        else some (matched (walk 0 s.a s.b))) := by
  subst hs
  simp only [alignLines, matched, alignPairs]
  generalize middle prev next = s
  have hsuf : (List.range s.k).map (fun t => some (s.lo + s.a.length + t)) =
      ((List.range s.k).map some).map (Option.map (· + (s.lo + s.a.length))) := by
    simp [Nat.add_comm]
  have hmid : (if s.a.length = 0 ∨ s.b.length = 0 ∨ c < s.a.length * s.b.length then
      List.replicate s.b.length none else walk s.lo s.a s.b).length = s.b.length := by
    split <;> simp [walk_length]
  rw [pairsFrom_append, pairsFrom_append, hsuf, List.length_append, List.length_map,
    List.length_range, hmid, pairsFrom_map, pairsFrom_range, pairsFrom_range]
  simp only [List.map_map]
  congr 2
  · have : shift 0 0 = id := by funext p; simp [shift]
    rw [this, List.map_id]
  · rw [Nat.zero_add]
    split
    · simp [pairsFrom_replicate_none]
    · rw [walk_shift, pairsFrom_map]; rfl
  · funext p
    simp only [Function.comp_apply, shift, Prod.mk.injEq]
    constructor <;> omega

/-- Soundness, whatever the cell limit: strictly increasing in both texts,
and only equal lines. -/
theorem align_sound (prev next : List α) (c : Nat) :
    Sound prev next (matched (alignLines prev next c)) := by
  obtain ⟨pre, suf, hp, hn, hl, hk⟩ := middle_split prev next _ rfl
  rw [align_matched prev next c _ rfl]
  generalize middle prev next = s at *
  have hmid : Sound s.a s.b ((if s.a.length = 0 ∨ s.b.length = 0 ∨ c < s.a.length * s.b.length
      then none else some (matched (walk 0 s.a s.b))).getD []) := by
    split
    · exact sound_nil _ _
    · exact walk_sound _ _
  have h3 := sound_append (diag_sound pre) (sound_append hmid (diag_sound suf))
  rw [hp, hn, List.append_assoc, List.append_assoc]
  rw [hl, hk] at h3
  have e : shift s.lo s.lo ∘ shift s.a.length s.b.length =
      shift (s.lo + s.a.length) (s.lo + s.b.length) := by
    funext p
    simp only [Function.comp_apply, shift, Prod.mk.injEq]
    constructor <;> omega
  simp only [alignPairs, List.map_append, List.map_map, List.append_assoc, e] at h3 ⊢
  exact h3

/-- Optimality: within the cell limit, as many lines match as the LCS of
the whole texts. -/
theorem align_optimal (prev next : List α) (c : Nat)
    (h : (middle prev next).a.length * (middle prev next).b.length ≤ c) :
    (matched (alignLines prev next c)).length = lcsLen prev next := by
  obtain ⟨pre, suf, hp, hn, hl, hk⟩ := middle_split prev next _ rfl
  rw [align_matched prev next c _ rfl]
  generalize middle prev next = s at *
  have : ((if s.a.length = 0 ∨ s.b.length = 0 ∨ c < s.a.length * s.b.length then none
      else some (matched (walk 0 s.a s.b))).getD []).length = lcsLen s.a s.b := by
    split
    · rename_i hc
      rcases hc with hc | hc | hc
      · simp [List.length_eq_zero_iff.mp hc]
      · simp [List.length_eq_zero_iff.mp hc]
      · omega
    · simp [walk_count]
  simp only [alignPairs, List.length_append, List.length_map, this, diag, List.length_range]
  rw [hp, hn, List.append_assoc, List.append_assoc, lcsLen_prefix, lcsLen_suffix, hl, hk]
  omega

/-- Past the cell limit: the shared start and end are matched as they stand,
nothing between, and what that loses is exactly the middles' LCS. -/
theorem align_fallback (prev next : List α) (c : Nat) (s : Middle α) (hs : middle prev next = s)
    (h : c < s.a.length * s.b.length) :
    matched (alignLines prev next c) = alignPairs s none ∧
      (matched (alignLines prev next c)).length + lcsLen s.a s.b = lcsLen prev next := by
  obtain ⟨pre, suf, hp, hn, hl, hk⟩ := middle_split prev next s hs
  have e := align_matched prev next c s hs
  rw [if_pos (Or.inr (Or.inr h))] at e
  refine ⟨e, ?_⟩
  rw [e]
  simp only [alignPairs, List.length_append, List.length_map, diag, List.length_range,
    Option.getD_none, List.map_nil, List.length_nil]
  rw [hp, hn, List.append_assoc, List.append_assoc, lcsLen_prefix, lcsLen_suffix, hl, hk]
  omega

/-- No sound matching has more pairs than the LCS: the alignment within the
cell limit is a largest one. -/
theorem sound_le_lcsLen (X Y : List α) (P : List (Nat × Nat)) (h : Sound X Y P) :
    P.length ≤ lcsLen X Y := by
  induction X generalizing Y P with
  | nil =>
    cases P with
    | nil => simp
    | cons p ps => obtain ⟨v, h1, _⟩ := h.2 p (by simp); simp at h1
  | cons x xs ihx =>
    induction Y generalizing P with
    | nil =>
      cases P with
      | nil => simp
      | cons p ps => obtain ⟨v, _, h2⟩ := h.2 p (by simp); simp at h2
    | cons y ys ihy =>
      cases P with
      | nil => simp
      | cons p ps =>
        obtain ⟨hpw, hmem⟩ := h
        rw [List.pairwise_cons] at hpw
        -- Every pair is at or after the first.
        have hge : ∀ q ∈ p :: ps, p.1 ≤ q.1 ∧ p.2 ≤ q.2 := by
          intro q hq
          rcases List.mem_cons.mp hq with rfl | hq
          · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
          · have := hpw.1 q hq; omega
        by_cases hi : p.1 = 0
        · by_cases hj : p.2 = 0
          · -- The first pair matches the heads: the rest matches the tails.
            obtain ⟨v, h1, h2⟩ := hmem p (by simp)
            rw [hi] at h1; rw [hj] at h2
            simp at h1 h2
            have hrest : Sound xs ys (ps.map (fun q => (q.1 - 1, q.2 - 1))) := by
              refine ⟨?_, ?_⟩
              · rw [List.pairwise_map]
                refine hpw.2.imp_of_mem (fun ha hb hab => ?_)
                have := hpw.1 _ ha; have := hpw.1 _ hb; omega
              · intro q hq
                rw [List.mem_map] at hq
                obtain ⟨q, hq, rfl⟩ := hq
                obtain ⟨w, g1, g2⟩ := hmem q (by simp [hq])
                have := hpw.1 q hq
                refine ⟨w, ?_, ?_⟩
                · rw [show q.1 = (q.1 - 1) + 1 by omega] at g1; simpa using g1
                · rw [show q.2 = (q.2 - 1) + 1 by omega] at g2; simpa using g2
            have := ihx ys _ hrest
            rw [lcsLen_cons, if_pos (h1.trans h2.symm)]
            simpa using this
          · -- Next's first line is unmatched.
            have hrest : Sound (x :: xs) ys ((p :: ps).map (fun q => (q.1, q.2 - 1))) := by
              refine ⟨?_, ?_⟩
              · rw [List.pairwise_map, List.pairwise_cons]
                refine ⟨fun q hq => ?_, hpw.2.imp_of_mem (fun ha hb hab => ?_)⟩
                · have := hpw.1 q hq; omega
                · have := hge _ (List.mem_cons_of_mem _ ha); omega
              · intro q hq
                rw [List.mem_map] at hq
                obtain ⟨q, hq, rfl⟩ := hq
                obtain ⟨w, g1, g2⟩ := hmem q hq
                have := hge q hq
                refine ⟨w, g1, ?_⟩
                rw [show q.2 = (q.2 - 1) + 1 by omega] at g2; simpa using g2
            have := ihy _ hrest
            simp only [List.length_map] at this
            rw [lcsLen_cons]
            split
            · rename_i hxy; subst hxy
              have := lcsLen_cons_left_le x xs ys
              omega
            · exact Nat.le_trans this (Nat.le_max_right _ _)
        · -- Prev's first line is unmatched.
          have hrest : Sound xs (y :: ys) ((p :: ps).map (fun q => (q.1 - 1, q.2))) := by
            refine ⟨?_, ?_⟩
            · rw [List.pairwise_map, List.pairwise_cons]
              refine ⟨fun q hq => ?_, hpw.2.imp_of_mem (fun ha hb hab => ?_)⟩
              · have := hpw.1 q hq; omega
              · have := hge _ (List.mem_cons_of_mem _ ha); omega
            · intro q hq
              rw [List.mem_map] at hq
              obtain ⟨q, hq, rfl⟩ := hq
              obtain ⟨w, g1, g2⟩ := hmem q hq
              have := hge q hq
              refine ⟨w, ?_, g2⟩
              rw [show q.1 = (q.1 - 1) + 1 by omega] at g1; simpa using g1
          have := ihx (y :: ys) _ hrest
          simp only [List.length_map] at this
          rw [lcsLen_cons]
          split
          · rename_i hxy; subst hxy
            have := lcsLen_cons_right_le x xs ys
            omega
          · exact Nat.le_trans this (Nat.le_max_left _ _)

end Align

end Vocara
