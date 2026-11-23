import VocaraVerify.Digits

/-!
# ULIDs sort by creation and never repeat

Mirrors `vocara_model::util::id` (port of `lib/utils/id.dart`): a ULID is a
48-bit millisecond timestamp and 80 bits of randomness, written as 26
Crockford base-32 characters. The ids are the library's primary keys and its
wire record keys, and the code relies on two facts this file proves:

* `encode` is order-preserving and injective, so string order is creation
  order (`ulid_order`, `ulid_injective`) and the first character is `0`–`7`;
* the minter only ever moves forward, within a millisecond, when the clock
  steps back, and across the 80-bit overflow spill (`mint_increases`), so
  ids minted by one process are strictly increasing and never repeat.
-/

namespace Vocara

/-- The Crockford alphabet as byte codes, in the order the encoder indexes it:
`0123456789ABCDEFGHJKMNPQRSTVWXYZ`. -/
def crockford : List Nat :=
  [48, 49, 50, 51, 52, 53, 54, 55, 56, 57,
   65, 66, 67, 68, 69, 70, 71, 72, 74, 75, 77, 78, 80, 81, 82, 83, 84, 86, 87, 88, 89, 90]

def crockfordCode (d : Nat) : Nat := crockford.getD d 0

/-- The encoder: 26 base-32 digits, most significant first, each written as
its Crockford character (Rust: `util::id::encode`). -/
def encodeUlid (v : Nat) : List Nat := (digits 32 26 v).map crockfordCode

theorem crockford_strictMono' :
    ∀ x, x < 32 → ∀ y, y < 32 → (crockfordCode x < crockfordCode y ↔ x < y) := by
  decide

theorem crockford_strictMono :
    ∀ x y, x < 32 → y < 32 → (crockfordCode x < crockfordCode y ↔ x < y) :=
  fun x y hx hy => crockford_strictMono' x hx y hy

/-- **String order is value order** for every 128-bit value. -/
theorem ulid_order (a b : Nat) (ha : a < 2 ^ 128) (hb : b < 2 ^ 128) :
    lexCmp (encodeUlid a) (encodeUlid b) = compare a b := by
  have h : (2 : Nat) ^ 128 ≤ 32 ^ 26 := by decide
  unfold encodeUlid
  rw [lexCmp_map crockfordCode 32 crockford_strictMono _ _
      (digits_lt 32 26 a (by decide)) (digits_lt 32 26 b (by decide))]
  exact lexCmp_digits 32 26 a b (by decide) (by omega) (by omega)

/-- **Distinct values, distinct ids.** -/
theorem ulid_injective (a b : Nat) (ha : a < 2 ^ 128) (hb : b < 2 ^ 128)
    (h : encodeUlid a = encodeUlid b) : a = b := by
  have := ulid_order a b ha hb
  rw [h, lexCmp_self] at this
  exact Nat.compare_eq_eq.mp this.symm

theorem ulid_length (v : Nat) : (encodeUlid v).length = 26 := by
  simp [encodeUlid, digits_length]

/-- The minter's state: the last timestamp handed out and the randomness of
the last id minted at it (`Minter` in Rust). -/
structure Minter where
  lastMs : Nat
  lastRand : Nat

/-- The 128-bit value of the id a state stands for. -/
def Minter.value (m : Minter) : Nat := m.lastMs * 2 ^ 80 + m.lastRand

/-- One mint (Rust: `new_id_at`), with the fresh randomness `fresh < 2^80`
supplied by the caller. -/
def mint (m : Minter) (nowMs fresh : Nat) : Minter :=
  if nowMs ≤ m.lastMs then
    if m.lastRand + 1 ≥ 2 ^ 80 then
      -- 80-bit overflow: spill into the next millisecond.
      { lastMs := m.lastMs + 1, lastRand := fresh }
    else { lastMs := m.lastMs, lastRand := m.lastRand + 1 }
  else { lastMs := nowMs, lastRand := fresh }

/-- The randomness always stays inside its 80 bits. -/
theorem mint_rand_lt (m : Minter) (now fresh : Nat) (hf : fresh < 2 ^ 80)
    (hm : m.lastRand < 2 ^ 80) : (mint m now fresh).lastRand < 2 ^ 80 := by
  unfold mint
  split
  · split
    · exact hf
    · simp only; omega
  · exact hf

/-- **Every mint is strictly after the previous one**, whatever the clock
does. -/
theorem mint_increases (m : Minter) (now fresh : Nat) (hm : m.lastRand < 2 ^ 80) :
    m.value < (mint m now fresh).value := by
  unfold mint Minter.value
  split
  · split
    · simp only
      rw [Nat.add_mul, Nat.one_mul]
      omega
    · simp only; omega
  · rename_i h
    simp only
    have : m.lastMs * 2 ^ 80 + 2 ^ 80 ≤ now * 2 ^ 80 := by
      rw [← Nat.succ_mul]; exact Nat.mul_le_mul_right _ (by omega)
    omega

/-- The first character is `0`–`7`: a 128-bit value leaves the top two bits
of the 130 the 26 characters carry empty. -/
theorem ulid_first_digit (v : Nat) (hv : v < 2 ^ 128) :
    (digits 32 26 v).head? = some (v / 32 ^ 25) ∧ v / 32 ^ 25 < 8 := by
  have hlt : v / 32 ^ 25 < 8 := Nat.div_lt_of_lt_mul (by
    have : (32 : Nat) ^ 25 * 8 = 2 ^ 128 := by decide
    omega)
  refine ⟨?_, hlt⟩
  simp [digits, Nat.mod_eq_of_lt (Nat.lt_trans hlt (by decide : 8 < 32))]

end Vocara
