import VocaraVerify.Digits

/-!
# The update check's version order

Mirrors `UpdateCheckService.isNewer` (`lib/data/services/update_check_service.dart`,
ported to `vocara_services::update_check`). Versions are dotted numbers
compared with missing parts read as zero (`1.2` = `1.2.0`); the RUNNING build
may be a prerelease (`1.2.0-rc.1`), which its own GA beats; a prerelease
CANDIDATE never parses, so it is never offered.

The parsing is replayed against the Rust code by the test vectors; this file
proves the comparison is a strict order, so the update banner cannot flip-flop:

* `isNewer_irrefl`: a build is never newer than itself;
* `isNewer_asymm`: if A is newer than B, B is not newer than A;
* `isNewer_trans`: newer than newer is newer;
* `ga_beats_own_prerelease` and `prerelease_candidate_never_newer`.
-/

namespace Vocara

/-- Dart's loop: compare part by part, a missing part reading as 0. -/
def cmpPadded : List Nat → List Nat → Ordering
  | [], [] => .eq
  | x :: xs, [] => match compare x 0 with | .eq => cmpPadded xs [] | o => o
  | [], y :: ys => match compare 0 y with | .eq => cmpPadded [] ys | o => o
  | x :: xs, y :: ys => match compare x y with | .eq => cmpPadded xs ys | o => o

/-- Trailing zero parts dropped: the representative padding compares. -/
def strip : List Nat → List Nat
  | [] => []
  | x :: xs =>
    match strip xs with
    | [] => if x = 0 then [] else [x]
    | s => x :: s

theorem strip_cons_of_ne (x : Nat) (xs : List Nat) (h : strip xs ≠ []) :
    strip (x :: xs) = x :: strip xs := by
  cases hs : strip xs with
  | nil => exact absurd hs h
  | cons s ss => simp [strip, hs]

theorem strip_cons_nil (x : Nat) (xs : List Nat) (h : strip xs = []) :
    strip (x :: xs) = if x = 0 then [] else [x] := by
  simp only [strip, h]

theorem lexCmp_nil_right (l : List Nat) : lexCmp l [] = if l = [] then .eq else .gt := by
  cases l <;> simp [lexCmp]

theorem lexCmp_nil_left (l : List Nat) : lexCmp [] l = if l = [] then .eq else .lt := by
  cases l <;> simp [lexCmp]

theorem compare_nat (a b : Nat) :
    compare a b = if a < b then .lt else if b < a then .gt else .eq := by
  simp only [compare, compareOfLessAndEq]
  by_cases h1 : a < b
  · simp [h1]
  · by_cases h2 : a = b
    · simp [h1, h2]
    · have : b < a := by omega
      simp [h1, h2, this]

theorem cmpPadded_cons (x y : Nat) (xs ys : List Nat) :
    cmpPadded (x :: xs) (y :: ys) =
      if x < y then .lt else if y < x then .gt else cmpPadded xs ys := by
  simp only [cmpPadded, compare_nat]
  by_cases h1 : x < y
  · simp [h1]
  · by_cases h2 : y < x <;> simp [h1, h2]

theorem cmpPadded_cons_nil (x : Nat) (xs : List Nat) :
    cmpPadded (x :: xs) [] = if x = 0 then cmpPadded xs [] else .gt := by
  simp only [cmpPadded, compare_nat]
  by_cases h : x = 0
  · simp [h]
  · have : 0 < x := by omega
    simp [h, this]

theorem cmpPadded_nil_cons (y : Nat) (ys : List Nat) :
    cmpPadded [] (y :: ys) = if y = 0 then cmpPadded [] ys else .lt := by
  simp only [cmpPadded, compare_nat]
  by_cases h : y = 0
  · simp [h]
  · have : 0 < y := by omega
    simp [h, this]

theorem lexCmp_cons_cons (x y : Nat) (xs ys : List Nat) :
    lexCmp (x :: xs) (y :: ys) =
      if x < y then .lt else if y < x then .gt else lexCmp xs ys := rfl

/-- Padded comparison is the lexicographic order of the stripped forms. -/
theorem cmpPadded_eq_lexCmp (a b : List Nat) :
    cmpPadded a b = lexCmp (strip a) (strip b) := by
  induction a generalizing b with
  | nil =>
    induction b with
    | nil => simp [cmpPadded, strip, lexCmp]
    | cons y ys ih =>
      rw [cmpPadded_nil_cons, ih]
      by_cases hs : strip ys = []
      · rw [strip_cons_nil y ys hs, hs]
        by_cases hy : y = 0 <;> simp [hy, strip, lexCmp]
      · rw [strip_cons_of_ne y ys hs]
        by_cases hy : y = 0
        · simp [hy, strip, lexCmp_nil_left, hs, lexCmp]
        · simp [hy, strip, lexCmp]
  | cons x xs ih =>
    cases b with
    | nil =>
      rw [cmpPadded_cons_nil, ih]
      by_cases hs : strip xs = []
      · rw [strip_cons_nil x xs hs, hs]
        by_cases hx : x = 0 <;> simp [hx, strip, lexCmp]
      · rw [strip_cons_of_ne x xs hs]
        by_cases hx : x = 0
        · simp [hx, strip, lexCmp_nil_right, hs, lexCmp]
        · simp [hx, strip, lexCmp]
    | cons y ys =>
      rw [cmpPadded_cons, ih ys]
      by_cases hx : strip xs = [] <;> by_cases hy : strip ys = []
      · rw [strip_cons_nil x xs hx, strip_cons_nil y ys hy, hx, hy]
        by_cases a0 : x = 0 <;> by_cases b0 : y = 0
        · subst a0; subst b0; simp [lexCmp]
        · subst a0
          have : 0 < y := by omega
          simp [b0, this, lexCmp]
        · subst b0
          have : 0 < x := by omega
          have : ¬ x < 0 := by omega
          simp [a0, *, lexCmp]
        · simp only [a0, b0, if_false, lexCmp_cons_cons]
          try simp [lexCmp]
      · rw [strip_cons_nil x xs hx, strip_cons_of_ne y ys hy, hx, lexCmp_nil_left]
        by_cases a0 : x = 0
        · subst a0
          by_cases b0 : 0 < y
          · simp [b0, lexCmp]
          · have : y = 0 := by omega
            subst this; simp [hy, lexCmp]
        · simp only [a0, if_false, lexCmp_cons_cons, lexCmp_nil_left, hy]
      · rw [strip_cons_of_ne x xs hx, strip_cons_nil y ys hy, hy, lexCmp_nil_right]
        by_cases b0 : y = 0
        · subst b0
          by_cases a0 : 0 < x
          · have : ¬ x < 0 := by omega
            simp [a0, this, lexCmp]
          · have : x = 0 := by omega
            subst this; simp [hx, lexCmp]
        · simp only [b0, if_false, lexCmp_cons_cons, lexCmp_nil_right, hx]
      · rw [strip_cons_of_ne x xs hx, strip_cons_of_ne y ys hy, lexCmp_cons_cons]

/-- `isNewer(candidate, current)` on parsed versions: `none` is a version
that did not parse (a prerelease candidate, junk); the flag marks a running
prerelease build. -/
def isNewer (cand : Option (List Nat)) (cur : Option (List Nat × Bool)) : Bool :=
  match cand, cur with
  | some a, some (b, pre) =>
    match cmpPadded a b with
    | .gt => true
    | .lt => false
    | .eq => pre
  | _, _ => false

theorem cmpPadded_self (a : List Nat) : cmpPadded a a = .eq := by
  rw [cmpPadded_eq_lexCmp]; exact lexCmp_self _

theorem cmpPadded_swap (a b : List Nat) : cmpPadded b a = (cmpPadded a b).swap := by
  rw [cmpPadded_eq_lexCmp, cmpPadded_eq_lexCmp]; exact lexCmp_swap _ _

theorem isNewer_irrefl (a : List Nat) : isNewer (some a) (some (a, false)) = false := by
  simp [isNewer, cmpPadded_self]

theorem isNewer_asymm (a b : List Nat) (h : isNewer (some a) (some (b, false)) = true) :
    isNewer (some b) (some (a, false)) = false := by
  simp only [isNewer] at h ⊢
  rw [cmpPadded_swap a b]
  cases hc : cmpPadded a b <;> simp_all [Ordering.swap]

theorem isNewer_trans (a b c : List Nat)
    (hab : isNewer (some a) (some (b, false)) = true)
    (hbc : isNewer (some b) (some (c, false)) = true) :
    isNewer (some a) (some (c, false)) = true := by
  simp only [isNewer] at hab hbc ⊢
  rw [cmpPadded_eq_lexCmp] at hab hbc ⊢
  have gab : lexCmp (strip a) (strip b) = .gt := by
    cases h : lexCmp (strip a) (strip b) <;> simp_all
  have gbc : lexCmp (strip b) (strip c) = .gt := by
    cases h : lexCmp (strip b) (strip c) <;> simp_all
  -- c ≤ b ≤ a, and a ≠ c (or b would be both above and below a).
  have hcb : lexCmp (strip c) (strip b) ≠ .gt := by
    rw [lexCmp_swap, gbc]; simp [Ordering.swap]
  have hba : lexCmp (strip b) (strip a) ≠ .gt := by
    rw [lexCmp_swap, gab]; simp [Ordering.swap]
  have hca := lexCmp_le_trans _ _ _ hcb hba
  have hac : lexCmp (strip a) (strip c) ≠ .lt := by
    intro h; rw [lexCmp_swap, h] at hca; simp [Ordering.swap] at hca
  cases h : lexCmp (strip a) (strip c) with
  | gt => rfl
  | lt => exact absurd h hac
  | eq =>
    rw [(lexCmp_eq_iff _ _).mp h] at gab
    rw [lexCmp_swap, gbc] at gab
    simp [Ordering.swap] at gab

theorem ga_beats_own_prerelease (a : List Nat) :
    isNewer (some a) (some (a, true)) = true := by
  simp [isNewer, cmpPadded_self]

theorem prerelease_candidate_never_newer (cur : Option (List Nat × Bool)) :
    isNewer none cur = false := by
  cases cur <;> rfl

end Vocara
