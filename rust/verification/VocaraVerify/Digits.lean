/-!
# Fixed-width digit strings sort like their numbers

The HLC's packed form writes the wall time and the counter as zero-padded
decimal fields, and a ULID writes its 128-bit value as 26 base-32 digits.
Both rely on one fact: fixed-width big-endian digit lists, compared
lexicographically, order exactly as the numbers they encode. This file
proves it for any base and width.
-/

namespace Vocara

/-- Lexicographic comparison of digit lists (a shorter prefix sorts first). -/
def lexCmp : List Nat → List Nat → Ordering
  | [], [] => .eq
  | [], _ :: _ => .lt
  | _ :: _, [] => .gt
  | a :: as, b :: bs =>
    if a < b then .lt else if b < a then .gt else lexCmp as bs

/-- The `w` least significant base-`b` digits of `n`, most significant first. -/
def digits (b : Nat) : Nat → Nat → List Nat
  | 0, _ => []
  | w + 1, n => (n / b ^ w) % b :: digits b w (n % b ^ w)

theorem digits_length (b w n : Nat) : (digits b w n).length = w := by
  induction w generalizing n with
  | zero => rfl
  | succ w ih => simp [digits, ih]

/-- Every digit is below the base. -/
theorem digits_lt (b w n : Nat) (hb : 0 < b) : ∀ d ∈ digits b w n, d < b := by
  induction w generalizing n with
  | zero => simp [digits]
  | succ w ih =>
    intro d hd
    simp only [digits, List.mem_cons] at hd
    rcases hd with rfl | hd
    · exact Nat.mod_lt _ hb
    · exact ih _ d hd

/-- Comparing two concatenations whose first parts have equal length: the
first parts decide, and only a tie looks at the rest. -/
theorem lexCmp_append (p q s t : List Nat) (h : p.length = q.length) :
    lexCmp (p ++ s) (q ++ t) =
      match lexCmp p q with
      | .eq => lexCmp s t
      | o => o := by
  induction p generalizing q with
  | nil =>
    cases q with
    | nil => simp [lexCmp]
    | cons _ _ => simp at h
  | cons a as ih =>
    cases q with
    | nil => simp at h
    | cons b bs =>
      simp only [List.length_cons, Nat.add_right_cancel_iff] at h
      simp only [List.cons_append, lexCmp]
      by_cases h1 : a < b
      · simp [h1]
      · by_cases h2 : b < a
        · simp [h1, h2]
        · simp [h1, h2, ih bs h]

/-- A strictly increasing map on the symbols keeps the lexicographic order. -/
theorem lexCmp_map (f : Nat → Nat) (bound : Nat)
    (hf : ∀ x y, x < bound → y < bound → (f x < f y ↔ x < y))
    (l r : List Nat) (hl : ∀ d ∈ l, d < bound) (hr : ∀ d ∈ r, d < bound) :
    lexCmp (l.map f) (r.map f) = lexCmp l r := by
  induction l generalizing r with
  | nil => cases r <;> simp [lexCmp]
  | cons a as ih =>
    cases r with
    | nil => simp [lexCmp]
    | cons b bs =>
      have ha := hl a (by simp)
      have hb := hr b (by simp)
      have e1 := hf a b ha hb
      have e2 := hf b a hb ha
      simp only [List.map_cons, lexCmp, e1, e2]
      rw [ih bs (fun d hd => hl d (by simp [hd])) (fun d hd => hr d (by simp [hd]))]

theorem lexCmp_self (l : List Nat) : lexCmp l l = .eq := by
  induction l with
  | nil => rfl
  | cons a as ih => simp [lexCmp, ih]

/-- Splitting a number below `P * b` into a leading digit and a remainder. -/
private theorem split_lt {n P b : Nat} (h : n < P * b) :
    n / P < b := by
  exact Nat.div_lt_of_lt_mul h

private theorem cmp_of_lead_lt {n m P : Nat} (hP : 0 < P)
    (h : n / P < m / P) : n < m := by
  have hn := Nat.div_add_mod n P
  have hm := Nat.div_add_mod m P
  have hr := Nat.mod_lt n hP
  -- n = P*qn + rn < P*(qn+1) <= P*qm <= m
  have h1 : n < P * (n / P + 1) := by
    rw [Nat.mul_add, Nat.mul_one]; omega
  have h2 : P * (n / P + 1) ≤ P * (m / P) := Nat.mul_le_mul_left _ h
  have h3 : P * (m / P) ≤ m := by omega
  omega

private theorem cmp_of_lead_eq {n m P : Nat} (h : n / P = m / P) :
    compare n m = compare (n % P) (m % P) := by
  have hn := Nat.div_add_mod n P
  have hm := Nat.div_add_mod m P
  rw [h] at hn
  -- n = P*q + rn and m = P*q + rm: the order is the remainders' order.
  have e1 : (n < m) = (n % P < m % P) := propext (by omega)
  have e2 : (n = m) = (n % P = m % P) := propext (by omega)
  simp only [compare, compareOfLessAndEq, e1, e2]

/-- **Fixed-width digits sort like their numbers.** For `n, m < b^w`, the
lexicographic order of their `w`-digit base-`b` expansions is the numeric
order. -/
theorem lexCmp_digits (b w n m : Nat) (hb : 0 < b)
    (hn : n < b ^ w) (hm : m < b ^ w) :
    lexCmp (digits b w n) (digits b w m) = compare n m := by
  induction w generalizing n m with
  | zero =>
    simp at hn hm
    subst hn; subst hm
    simp [digits, lexCmp, compare, compareOfLessAndEq]
  | succ w ih =>
    have hP : 0 < b ^ w := Nat.pow_pos hb
    rw [Nat.pow_succ] at hn hm
    have qn := split_lt hn
    have qm := split_lt hm
    simp only [digits, lexCmp, Nat.mod_eq_of_lt qn, Nat.mod_eq_of_lt qm]
    by_cases hlt : n / b ^ w < m / b ^ w
    · have := cmp_of_lead_lt hP hlt
      simp [hlt, compare, compareOfLessAndEq, this]
    · by_cases hgt : m / b ^ w < n / b ^ w
      · have := cmp_of_lead_lt hP hgt
        have hne : n ≠ m := by omega
        have hnlt : ¬ n < m := by omega
        simp [hlt, hgt, compare, compareOfLessAndEq, hnlt, hne]
      · have heq : n / b ^ w = m / b ^ w := by omega
        simp only [hlt, hgt, if_false]
        rw [ih _ _ (Nat.mod_lt _ hP) (Nat.mod_lt _ hP), cmp_of_lead_eq heq]

/-! ## `lexCmp` is a lawful total order -/

theorem lexCmp_swap (l r : List Nat) : lexCmp r l = (lexCmp l r).swap := by
  induction l generalizing r with
  | nil => cases r <;> rfl
  | cons a as ih =>
    cases r with
    | nil => rfl
    | cons b bs =>
      simp only [lexCmp]
      by_cases h1 : a < b
      · have : ¬ b < a := by omega
        simp [h1, this, Ordering.swap]
      · by_cases h2 : b < a
        · simp [h1, h2, Ordering.swap]
        · simp [h1, h2, ih bs]

theorem lexCmp_eq_iff (l r : List Nat) : lexCmp l r = .eq ↔ l = r := by
  constructor
  · intro h
    induction l generalizing r with
    | nil => cases r <;> simp_all [lexCmp]
    | cons a as ih =>
      cases r with
      | nil => simp [lexCmp] at h
      | cons b bs =>
        simp only [lexCmp] at h
        by_cases h1 : a < b
        · simp [h1] at h
        · by_cases h2 : b < a
          · simp [h1, h2] at h
          · simp only [h1, h2, if_false] at h
            have : a = b := by omega
            rw [this, ih bs h]
  · intro h; subst h; exact lexCmp_self l

/-- "Not after" is transitive. -/
theorem lexCmp_le_trans (a b c : List Nat)
    (hab : lexCmp a b ≠ .gt) (hbc : lexCmp b c ≠ .gt) : lexCmp a c ≠ .gt := by
  induction a generalizing b c with
  | nil => cases c <;> simp [lexCmp]
  | cons x xs ih =>
    cases b with
    | nil => simp [lexCmp] at hab
    | cons y ys =>
      cases c with
      | nil => simp [lexCmp] at hbc
      | cons z zs =>
        simp only [lexCmp] at hab hbc ⊢
        by_cases hxy : x < y
        · have : x < z := by
            by_cases hyz : y < z
            · omega
            · by_cases hzy : z < y
              · simp [hyz, hzy] at hbc
              · omega
          simp [this]
        · by_cases hyx : y < x
          · simp [hxy, hyx] at hab
          · have exy : x = y := by omega
            simp only [hxy, hyx, if_false] at hab
            by_cases hyz : y < z
            · have : x < z := by omega
              simp [this]
            · by_cases hzy : z < y
              · simp [hyz, hzy] at hbc
              · simp only [hyz, hzy, if_false] at hbc
                have e1 : ¬ x < z := by omega
                have e2 : ¬ z < x := by omega
                simp only [e1, e2, if_false]
                exact ih ys zs hab hbc

/-- Fixed-width digit expansions are injective below `b^w`. -/
theorem digits_injective (b w n m : Nat) (hb : 0 < b)
    (hn : n < b ^ w) (hm : m < b ^ w)
    (h : digits b w n = digits b w m) : n = m := by
  have := lexCmp_digits b w n m hb hn hm
  rw [h, lexCmp_self] at this
  exact (Nat.compare_eq_eq.mp this.symm)

end Vocara
