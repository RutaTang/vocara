import VocaraVerify.Digits

/-!
# The Hybrid Logical Clock

Mirrors `vocara_store::sync::hlc` (port of `lib/data/sync/hlc.dart`). Every
local write is stamped by `tick`, and every remote stamp the clock accepts
goes through `advance` (`receive` / `mergeVerified`). Entity-level LWW and the
oplog depend on these facts, proved here:

* `tick_gt`: a tick is strictly after the clock's last stamp, whatever the
  wall clock does (including the 6-digit counter spilling into the next
  millisecond);
* `advance_gt_last` and `advance_gt_remote`: a merge is strictly after both
  the clock's last stamp and the remote one;
* `tick_ctr_le` / `advance_ctr_le`: the counter never exceeds 999999, so the
  packed form keeps its fixed width;
* `packed_order`: the packed string (`0000000001000-000002-node`), which the
  store compares as TEXT, orders exactly as the clock does.

A stamp's order is Dart's `compareTo`: wall time, then counter, then the node
string. As a list that is the lexicographic order of `wall :: ctr :: node`.
-/

namespace Vocara

structure Hlc where
  wall : Nat
  ctr : Nat
  /-- The node id's character codes. -/
  node : List Nat
  deriving DecidableEq, Repr

def Hlc.key (h : Hlc) : List Nat := h.wall :: h.ctr :: h.node

/-- Dart's `Hlc.compareTo`. -/
def hlcCmp (a b : Hlc) : Ordering := lexCmp a.key b.key

def maxCounter : Nat := 999999

/-- The common last step of `tick` and `_advance`: past 999999 the counter
spills into the next millisecond, keeping the packed counter at six digits. -/
def finish (wall ctr : Nat) (node : List Nat) : Hlc :=
  if ctr > maxCounter then ⟨wall + 1, 0, node⟩ else ⟨wall, ctr, node⟩

def tickWall (last : Hlc) (now : Nat) : Nat := if now > last.wall then now else last.wall

def tickCtr (last : Hlc) (now : Nat) : Nat :=
  if tickWall last now = last.wall then last.ctr + 1 else 0

/-- `HlcClock.tick` with the clock's last stamp, its node and the wall clock. -/
def tick (last : Hlc) (node : List Nat) (now : Nat) : Hlc :=
  finish (tickWall last now) (tickCtr last now) node

def advWall (last remote : Hlc) (now : Nat) : Nat := max (max now last.wall) remote.wall

def advCtr (last remote : Hlc) (now : Nat) : Nat :=
  let wall := advWall last remote now
  if wall = last.wall ∧ wall = remote.wall then max last.ctr remote.ctr + 1
  else if wall = last.wall then last.ctr + 1
  else if wall = remote.wall then remote.ctr + 1
  else 0

/-- `HlcClock._advance`: the merge behind `receive` and `mergeVerified`. -/
def advance (last remote : Hlc) (node : List Nat) (now : Nat) : Hlc :=
  finish (advWall last remote now) (advCtr last remote now) node

theorem gt_of_wall {a b : Hlc} (h : b.wall < a.wall) : hlcCmp a b = .gt := by
  have : ¬ a.wall < b.wall := by omega
  simp [hlcCmp, Hlc.key, lexCmp, h, this]

theorem gt_of_ctr {a b : Hlc} (hw : a.wall = b.wall) (h : b.ctr < a.ctr) :
    hlcCmp a b = .gt := by
  have : ¬ a.ctr < b.ctr := by omega
  simp [hlcCmp, Hlc.key, lexCmp, hw, h, this]

/-- `finish` lands after any stamp it beats on wall time, or ties on wall time
and beats on the counter, spill or no spill. -/
theorem finish_gt (wall ctr : Nat) (node : List Nat) (h : Hlc)
    (hgt : h.wall < wall ∨ (h.wall = wall ∧ h.ctr < ctr)) :
    hlcCmp (finish wall ctr node) h = .gt := by
  unfold finish
  split
  · exact gt_of_wall (by simp; omega)
  · rcases hgt with hw | ⟨hw, hc⟩
    · exact gt_of_wall (by simp; omega)
    · exact gt_of_ctr (by simp; omega) (by simp; omega)

theorem finish_ctr_le (wall ctr : Nat) (node : List Nat) :
    (finish wall ctr node).ctr ≤ maxCounter := by
  unfold finish; split <;> simp <;> omega

/-- **A tick is strictly after the last stamp.** -/
theorem tick_gt (last : Hlc) (node : List Nat) (now : Nat) :
    hlcCmp (tick last node now) last = .gt := by
  apply finish_gt
  unfold tickCtr tickWall
  by_cases hn : now > last.wall
  · left; simp [hn]
  · right; simp [hn]

/-- **A merge is strictly after the clock's last stamp.** -/
theorem advance_gt_last (last remote : Hlc) (node : List Nat) (now : Nat) :
    hlcCmp (advance last remote node now) last = .gt := by
  apply finish_gt
  have hw : last.wall ≤ advWall last remote now := by unfold advWall; omega
  dsimp only [advCtr]
  generalize advWall last remote now = w at *
  by_cases h1 : w = last.wall
  · right
    refine ⟨h1.symm, ?_⟩
    by_cases h2 : w = remote.wall
    · rw [if_pos ⟨h1, h2⟩]; omega
    · rw [if_neg (fun h => h2 h.2), if_pos h1]; omega
  · left; omega

/-- **A merge is strictly after the remote stamp.** -/
theorem advance_gt_remote (last remote : Hlc) (node : List Nat) (now : Nat) :
    hlcCmp (advance last remote node now) remote = .gt := by
  apply finish_gt
  have hw : remote.wall ≤ advWall last remote now := by unfold advWall; omega
  dsimp only [advCtr]
  generalize advWall last remote now = w at *
  by_cases h2 : w = remote.wall
  · right
    refine ⟨h2.symm, ?_⟩
    by_cases h1 : w = last.wall
    · rw [if_pos ⟨h1, h2⟩]; omega
    · rw [if_neg (fun h => h1 h.1), if_neg h1, if_pos h2]; omega
  · left; omega

/-- The counter keeps its six digits. -/
theorem tick_ctr_le (last : Hlc) (node : List Nat) (now : Nat) :
    (tick last node now).ctr ≤ maxCounter := finish_ctr_le _ _ _

theorem advance_ctr_le (last remote : Hlc) (node : List Nat) (now : Nat) :
    (advance last remote node now).ctr ≤ maxCounter := finish_ctr_le _ _ _

/-! ## The packed form -/

def dash : Nat := 45
def digitCode (d : Nat) : Nat := 48 + d

/-- `Hlc.packed`: the wall time zero-padded to 13 digits, `-`, the counter
zero-padded to 6, `-`, the node. -/
def packed (h : Hlc) : List Nat :=
  (digits 10 13 h.wall).map digitCode ++ [dash] ++
    (digits 10 6 h.ctr).map digitCode ++ [dash] ++ h.node

theorem digitCode_mono : ∀ x y, x < 10 → y < 10 → (digitCode x < digitCode y ↔ x < y) := by
  intro x y _ _; simp [digitCode]

theorem lexCmp_numField (w n m : Nat) (hn : n < 10 ^ w) (hm : m < 10 ^ w) :
    lexCmp ((digits 10 w n).map digitCode) ((digits 10 w m).map digitCode) = compare n m := by
  rw [lexCmp_map digitCode 10 digitCode_mono _ _ (digits_lt 10 w n (by decide))
    (digits_lt 10 w m (by decide))]
  exact lexCmp_digits 10 w n m (by decide) hn hm

theorem lexCmp_cons_compare (a b : Nat) (s t : List Nat) :
    lexCmp (a :: s) (b :: t) =
      match compare a b with
      | .eq => lexCmp s t
      | o => o := by
  simp only [lexCmp, compare, compareOfLessAndEq]
  by_cases h1 : a < b
  · simp [h1]
  · by_cases h2 : b < a
    · have : a ≠ b := by omega
      simp [h1, h2, this]
    · have : a = b := by omega
      simp [h1, h2, this]

theorem lexCmp_cons_self (x : Nat) (s t : List Nat) :
    lexCmp (x :: s) (x :: t) = lexCmp s t := by
  simp [lexCmp]

/-- **The packed string orders as the clock.** Valid while the wall time has
13 digits (until the year 2286) and the counter its 6 (always: see
`tick_ctr_le`). -/
theorem packed_order (a b : Hlc) (ha : a.wall < 10 ^ 13) (hb : b.wall < 10 ^ 13)
    (hac : a.ctr < 10 ^ 6) (hbc : b.ctr < 10 ^ 6) :
    lexCmp (packed a) (packed b) = hlcCmp a b := by
  simp only [packed, List.append_assoc, hlcCmp, Hlc.key]
  rw [lexCmp_append _ _ _ _ (by simp [digits_length]), lexCmp_numField 13 _ _ ha hb,
    lexCmp_cons_compare a.wall]
  cases compare a.wall b.wall with
  | lt => rfl
  | gt => rfl
  | eq =>
    simp only [List.singleton_append, List.cons_append, List.nil_append]
    rw [lexCmp_cons_self, lexCmp_append ((digits 10 6 a.ctr).map digitCode)
        ((digits 10 6 b.ctr).map digitCode) _ _ (by simp [digits_length]),
      lexCmp_numField 6 _ _ hac hbc, lexCmp_cons_compare a.ctr]
    cases compare a.ctr b.ctr with
    | lt => rfl
    | gt => rfl
    | eq => exact lexCmp_cons_self _ _ _

end Vocara
