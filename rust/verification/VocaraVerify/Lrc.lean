/-!
# LRC: writing timed lyrics and reading them back

Mirrors `to_lrc` and `parse_lrc` (`vocara_model::util::lyrics_format`,
ported from `toLrc` / `parseLrc` in `lib/utils/lyrics_format.dart`).

`to_lrc` writes each timed line as `[mm:ss.cc]text` — minutes (two digits,
three past 99), seconds, and the time's centiseconds, ROUNDED DOWN — and an
untimed line as its bare text, each ending in `\n`. `parse_lrc` splits the
document at `\r\n`, `\n` or a lone `\r`; trims each line (Dart's `trim`:
Unicode white space and U+FEFF); takes every time tag
`\[(\d{1,3}):(\d{2})(?:[.:](\d{1,3}))?\]` on it (one entry per tag, the text
with the tags cut out and trimmed); skips lines without one (untimed lines,
`[ti:]`-style metadata); moves every time sooner by the last `[offset:±n]`
line (held to a day); refuses a document whose expansion passes
`max(2^20, 4 × its UTF-16 length)` characters; and sorts stably by time.

Model: text as lists of characters. The Rust matches bytes, the model
characters: every byte a tag or an offset is made of is ASCII, which no byte
of a multi-byte UTF-8 character is, so both find the same tags. Times are
`Int`, as the Rust's `i64` (`/` truncating as `Int.tdiv`, `rem_euclid` as
`%`). Proved:

* `scan_fmtTag` (the timestamp round trip): for `0 ≤ t` below 1000 minutes,
  the tag `to_lrc` writes, whatever follows it, reads as ONE tag, of `t`
  rounded down to the centisecond (`t / 10 * 10`), and the scan goes on
  right after it (`tagAt_fmtTag`; alone, `scan_fmtTag_alone`). Minutes past
  99 are written with three digits and read back. `scan_fmtTag_long`: from
  1000 minutes on the tag is no tag at all.
* `parseLrc_toLrc` (the document round trip): for `Writable` lines — no
  line break in the text, no time tag in the trimmed text, no negative
  time, and, untimed, not an `[offset:]` line — parsing what `to_lrc` wrote
  succeeds and gives `sortByTime (ls.filterMap survives)`: the timed lines
  below 1000 minutes, each with its text trimmed and its time rounded down
  to the centisecond, sorted stably by time. Untimed lines and lines from
  1000 minutes on are dropped; nothing else is lost, and the size limit is
  never reached.
* `parseLrc_toLrc_inOrder`: timed lines in time order come back in their
  own order;
* `parseLrc_toLrc_normal`: lines already in that form (trimmed, at whole
  centiseconds, in time order) come back unchanged;
* `parseLrc_toLrc_idem`: so what one save read back, a second save writes
  and reads back as itself.

Negative times are outside the theorems (the app makes none: `parse_lrc`
holds times at 0, and the playhead is never negative). One less than a
minute below 0 is written as a time near a minute (`-1500` as
`[00:59.50]`), one further below as no tag; the vectors pin both. The
vectors (`lrc_*.json`) replay the model through the Rust; they do not pin
the size limit's `4 ×` (a document past `2^18` characters overflows the
generator's stack), only its `2^20` floor, which they hit exactly.
-/

namespace Vocara

/-- A lyric line: its text and its time (ms), `none` until synced. -/
structure LyricLine where
  text : List Char
  time : Option Int
  deriving DecidableEq, Repr

/-! ## Characters -/

/-- `dart_string::is_whitespace` (what Dart's `trim` strips): Unicode
`White_Space` and U+FEFF. -/
def isDartWs (c : Char) : Bool :=
  let n := c.toNat
  (9 ≤ n && n ≤ 13) || n == 0x20 || n == 0x85 || n == 0xA0 || n == 0x1680 ||
    (0x2000 ≤ n && n ≤ 0x200A) || n == 0x2028 || n == 0x2029 || n == 0x202F ||
    n == 0x205F || n == 0x3000 || n == 0xFEFF

/-- `is_js_space` (JavaScript's `\s`): the same without U+0085. -/
def isJsSpace (c : Char) : Bool := isDartWs c && c.toNat != 0x85

/-- An ASCII digit (Dart's `\d`). -/
def isDigitC (c : Char) : Bool := 48 ≤ c.toNat && c.toNat ≤ 57

/-- The number a run of ASCII digits spells. -/
def digitsVal (ds : List Char) : Nat := ds.foldl (fun n c => n * 10 + (c.toNat - 48)) 0

/-- Dart's `String.length`: UTF-16 code units. -/
def utf16Len : List Char → Nat
  | [] => 0
  | c :: cs => (if c.toNat < 0x10000 then 1 else 2) + utf16Len cs

/-- `to_ascii_lowercase`. -/
def lowerAscii (c : Char) : Char :=
  if 65 ≤ c.toNat ∧ c.toNat ≤ 90 then Char.ofNat (c.toNat + 32) else c

/-! ## Trimming and lines -/

def trimL (s : List Char) : List Char := s.dropWhile isDartWs

def trimR : List Char → List Char
  | [] => []
  | c :: cs => if (trimR cs).isEmpty && isDartWs c then [] else c :: trimR cs

/-- `dart_string::trim`. -/
def dartTrim (s : List Char) : List Char := trimL (trimR s)

/-- `split_lines`: at `\r\n`, `\n` or a lone `\r`. -/
def splitLines : List Char → List (List Char)
  | [] => [[]]
  | c :: r =>
    if c = '\r' then
      match r with
      | c' :: r' => if c' = '\n' then [] :: splitLines r' else [] :: splitLines (c' :: r')
      | [] => [[], []]
    else if c = '\n' then [] :: splitLines r
    else
      match splitLines r with
      | l :: ls => (c :: l) :: ls
      | [] => [[c]]

/-! ## Time tags -/

/-- `\d{lo,hi}` then `c` (the digit run greedy, as `digit_run`): the digits
and what follows `c`. -/
def digitsThen (lo hi : Nat) (c : Char) (s : List Char) : Option (List Char × List Char) :=
  match s.dropWhile isDigitC with
  | c' :: r =>
    if lo ≤ (s.takeWhile isDigitC).length ∧ (s.takeWhile isDigitC).length ≤ hi ∧ c' = c then
      some (s.takeWhile isDigitC, r)
    else none
  | [] => none

/-- A fraction's milliseconds: tenths (1 digit), hundredths (2) or
milliseconds (3). -/
def fracMs (f : List Char) : Nat :=
  if f.length = 1 then digitsVal f * 100 else if f.length = 2 then digitsVal f * 10 else digitsVal f

/-- The optional `[.:](\d{1,3})` of a tag, up to its closing `]`. -/
def fracPart : List Char → Option (List Char × List Char)
  | c2 :: r3 => if c2 = '.' ∨ c2 = ':' then digitsThen 1 3 ']' r3 else none
  | [] => none

/-- `time_tag_at`: the time tag `\[(\d{1,3}):(\d{2})(?:[.:](\d{1,3}))?\]`
starting at the head of `s`, as its length and its time (ms). -/
def tagAt : List Char → Option (Nat × Nat)
  | [] => none
  | c :: r =>
    if c ≠ '[' then none else
    match digitsThen 1 3 ':' r with
    | none => none
    | some (mm, r1) =>
      match r1 with
      | s1 :: s2 :: r2 =>
        if isDigitC s1 && isDigitC s2 then
          let base := (digitsVal mm * 60 + digitsVal [s1, s2]) * 1000
          match fracPart r2 with
          | some (fr, _) => some (mm.length + fr.length + 6, base + fracMs fr)
          | none => if r2.head? = some ']' then some (mm.length + 5, base) else none
        else none
      | _ => none

theorem tagAt_pos {s : List Char} {n v : Nat} (h : tagAt s = some (n, v)) : 0 < n ∧ s ≠ [] := by
  unfold tagAt at h
  repeat' split at h
  all_goals first
    | (simp at h; done)
    | (simp only [Option.some.injEq, Prod.mk.injEq] at h; exact ⟨by omega, by simp⟩)

/-- `time_tags` and the text with the tags cut out, in one left-to-right
pass (the Rust's loop): a tag at the head is taken whole, else one
character is kept. -/
def scan (s : List Char) : List Nat × List Char :=
  match h : tagAt s with
  | some (n, v) =>
    have : s.length - n < s.length := by
      obtain ⟨hn, hs⟩ := tagAt_pos h
      have : s.length ≠ 0 := by simpa using hs
      omega
    let r := scan (s.drop n)
    (v :: r.1, r.2)
  | none =>
    match s with
    | [] => ([], [])
    | c :: cs =>
      let r := scan cs
      (r.1, c :: r.2)
termination_by s.length

theorem scan_of_tag {s : List Char} {n v : Nat} (h : tagAt s = some (n, v)) :
    scan s = (v :: (scan (s.drop n)).1, (scan (s.drop n)).2) := by
  rw [scan.eq_def]
  split
  · rename_i n' v' h'
    rw [h] at h'
    simp only [Option.some.injEq, Prod.mk.injEq] at h'
    obtain ⟨rfl, rfl⟩ := h'
    rfl
  · rename_i h'
    rw [h] at h'
    simp at h'

theorem scan_of_none {c : Char} {cs : List Char} (h : tagAt (c :: cs) = none) :
    scan (c :: cs) = ((scan cs).1, c :: (scan cs).2) := by
  rw [scan.eq_def]
  split
  · rename_i n' v' h'
    rw [h] at h'
    simp at h'
  · rfl

@[simp] theorem scan_nil : scan [] = ([], []) := by
  rw [scan.eq_def]
  split
  · rename_i h; simp [tagAt] at h
  · rfl

/-! ## Offsets -/

/-- `offset_tag`: the number of a whole-line `[offset: ±n ]` tag
(`^\[offset:\s*([+-]?\d+)\s*\]$`, the name in any case). -/
def offsetTag : List Char → Option (List Char)
  | [] => none
  | c :: rest =>
    if c ≠ '[' ∨ (rest.take 7).map lowerAscii ≠ "offset:".toList then none else
    let r := (rest.drop 7).dropWhile isJsSpace
    let sign := if r.head? = some '+' ∨ r.head? = some '-' then 1 else 0
    let digits := ((r.drop sign).takeWhile isDigitC).length
    if digits = 0 then none else
    if (r.drop (sign + digits)).dropWhile isJsSpace = [']'] then some (r.take (sign + digits))
    else none

/-- `str::parse::<i64>` of a sign and digits: `none` past `i64`. -/
def parseI64 (s : List Char) : Option Int :=
  let v : Int :=
    match s with
    | c :: ds => if c = '-' then -(digitsVal ds : Int) else if c = '+' then (digitsVal ds : Int)
      else (digitsVal s : Int)
    | [] => 0
  if v < -(2 ^ 63) ∨ 2 ^ 63 ≤ v then none else some v

/-- The offset a (trimmed) line sets, if it is an offset line. -/
def offsetOf (line : List Char) : Option Int := (offsetTag line).bind parseI64

/-- The document's offset: the last offset line's, held to a day. -/
def docOffset (lines : List (List Char)) : Int :=
  lines.foldl (fun o raw =>
    match offsetOf (dartTrim raw) with
    | some ms => max (-86400000) (min 86400000 ms)
    | none => o) 0

/-! ## Parsing -/

/-- The entries of the lines, `emitted` characters of text so far; `none`
once they pass the budget. -/
def emit (offset : Int) (budget : Nat) : Nat → List (List Char) → Option (List LyricLine)
  | _, [] => some []
  | e, raw :: rest =>
    let line := dartTrim raw
    if line.isEmpty then emit offset budget e rest else
    let tags := (scan line).1
    if tags.isEmpty then emit offset budget e rest else
    let text := dartTrim (scan line).2
    let e' := e + (utf16Len text + 1) * tags.length
    if budget < e' then none else
    (emit offset budget e' rest).map fun out =>
      tags.map (fun (ms : Nat) => ({ text := text, time := some (max 0 ((ms : Int) - offset)) } : LyricLine))
        ++ out

def timeKey (l : LyricLine) : Int := l.time.getD 0

/-- Inserts before the first line not earlier (so equal times keep their
order). -/
def insertByTime (x : LyricLine) : List LyricLine → List LyricLine
  | [] => [x]
  | y :: ys => if timeKey x ≤ timeKey y then x :: y :: ys else y :: insertByTime x ys

/-- A stable sort by time (`sort_by_key`). -/
def sortByTime : List LyricLine → List LyricLine
  | [] => []
  | x :: xs => insertByTime x (sortByTime xs)

/-- `parse_lrc`; `none` for its error. -/
def parseLrc (content : List Char) : Option (List LyricLine) :=
  let lines := splitLines content
  (emit (docOffset lines) (max (2 ^ 20) (utf16Len content * 4)) 0 lines).map sortByTime

/-! ## Writing -/

def digitChar (d : Nat) : Char := Char.ofNat (48 + d)

/-- `n.to_string()`. -/
def decimal (n : Nat) : List Char :=
  if n < 10 then [digitChar n] else decimal (n / 10) ++ [digitChar (n % 10)]
termination_by n
decreasing_by omega

/-- `i64::to_string`. -/
def intStr (n : Int) : List Char := if n < 0 then '-' :: decimal n.natAbs else decimal n.toNat

/-- `format!("{:0>2}", s)`. -/
def pad2 (s : List Char) : List Char := List.replicate (2 - s.length) '0' ++ s

/-- The `[mm:ss.cc]` tag `to_lrc` writes for time `t`. -/
def fmtTag (t : Int) : List Char :=
  '[' :: pad2 (intStr (t.tdiv 60000)) ++ ':' :: pad2 (intStr (t.tdiv 1000 % 60)) ++
    '.' :: pad2 (intStr (t % 1000 / 10)) ++ [']']

def renderLine (l : LyricLine) : List Char :=
  match l.time with
  | some t => fmtTag t ++ l.text
  | none => l.text

/-- `to_lrc`. -/
def toLrc (ls : List LyricLine) : List Char := ls.flatMap fun l => renderLine l ++ ['\n']

/-! ## Digits -/

theorem digitChar_toNat' : ∀ d : Fin 10, (digitChar d.val).toNat = 48 + d.val := by decide

theorem digitChar_toNat {d : Nat} (h : d < 10) : (digitChar d).toNat = 48 + d :=
  digitChar_toNat' ⟨d, h⟩

theorem isDigitC_digitChar {d : Nat} (h : d < 10) : isDigitC (digitChar d) = true := by
  simp [isDigitC, digitChar_toNat h]; omega

theorem decimal_eq (n : Nat) :
    decimal n = if n < 10 then [digitChar n] else decimal (n / 10) ++ [digitChar (n % 10)] := by
  rw [decimal]

theorem decimal_digits (n : Nat) : ∀ c ∈ decimal n, isDigitC c = true := by
  induction n using Nat.strongRecOn with
  | ind n ih =>
    rw [decimal_eq]
    split
    · intro c hc
      simp at hc; subst hc; exact isDigitC_digitChar (by omega)
    · intro c hc
      simp only [List.mem_append, List.mem_singleton] at hc
      rcases hc with hc | rfl
      · exact ih _ (by omega) c hc
      · exact isDigitC_digitChar (Nat.mod_lt _ (by omega))

theorem digitsVal_snoc (a : List Char) (c : Char) :
    digitsVal (a ++ [c]) = digitsVal a * 10 + (c.toNat - 48) := by
  simp [digitsVal, List.foldl_append]

theorem digitsVal_decimal (n : Nat) : digitsVal (decimal n) = n := by
  induction n using Nat.strongRecOn with
  | ind n ih =>
    rw [decimal_eq]
    split
    · simp [digitsVal, digitChar_toNat (show n < 10 by omega)]
    · rw [digitsVal_snoc, ih _ (by omega), digitChar_toNat (Nat.mod_lt _ (by omega))]
      omega

theorem decimal_length (n : Nat) :
    (decimal n).length = if n < 10 then 1 else (decimal (n / 10)).length + 1 := by
  rw [decimal_eq]; split <;> simp

theorem decimal_length_pos (n : Nat) : 1 ≤ (decimal n).length := by
  rw [decimal_length]; split <;> omega

theorem digitsVal_replicate_zero (k : Nat) (s : List Char) :
    digitsVal (List.replicate k '0' ++ s) = digitsVal s := by
  induction k with
  | zero => simp
  | succ k ih =>
    simp only [List.replicate_succ, List.cons_append, digitsVal, List.foldl_cons] at ih ⊢
    have : (0 * 10 + ('0'.toNat - 48)) = 0 := by decide
    rw [this, ih]

theorem digitsVal_pad2_decimal (n : Nat) : digitsVal (pad2 (decimal n)) = n := by
  simp [pad2, digitsVal_replicate_zero, digitsVal_decimal]

theorem pad2_decimal_digits (n : Nat) : ∀ c ∈ pad2 (decimal n), isDigitC c = true := by
  intro c hc
  simp only [pad2, List.mem_append, List.mem_replicate] at hc
  rcases hc with ⟨-, rfl⟩ | hc
  · decide
  · exact decimal_digits n c hc

theorem pad2_decimal_length_two (n : Nat) (h : n < 100) : (pad2 (decimal n)).length = 2 := by
  have : (decimal n).length ≤ 2 := by
    rw [decimal_length]; split
    · omega
    · rw [decimal_length, if_pos (by omega)]; omega
  simp [pad2]; omega

theorem pad2_decimal_length_three (n : Nat) (h1 : 100 ≤ n) (h2 : n < 1000) :
    (pad2 (decimal n)).length = 3 := by
  have : (decimal n).length = 3 := by
    rw [decimal_length, if_neg (by omega), decimal_length, if_neg (by omega), decimal_length,
      if_pos (by omega)]
  simp [pad2]; omega

theorem pad2_decimal_length_long (n : Nat) (h : 1000 ≤ n) : 4 ≤ (pad2 (decimal n)).length := by
  have : 4 ≤ (decimal n).length := by
    rw [decimal_length, if_neg (by omega), decimal_length, if_neg (by omega), decimal_length,
      if_neg (by omega)]
    have := decimal_length_pos (n / 10 / 10 / 10)
    omega
  simp [pad2]; omega

theorem two_of_length {l : List Char} (h : l.length = 2) : ∃ a b, l = [a, b] := by
  match l, h with
  | [a, b], _ => exact ⟨a, b, rfl⟩

/-! ## The tag `to_lrc` writes -/

theorem takeWhile_digits (ds : List Char) (c : Char) (r : List Char)
    (hds : ∀ d ∈ ds, isDigitC d = true) (hc : isDigitC c = false) :
    (ds ++ c :: r).takeWhile isDigitC = ds ∧ (ds ++ c :: r).dropWhile isDigitC = c :: r := by
  induction ds with
  | nil => simp [List.takeWhile, List.dropWhile, hc]
  | cons d ds ih =>
    have hd := hds d (by simp)
    obtain ⟨h1, h2⟩ := ih (fun x hx => hds x (by simp [hx]))
    simp [List.takeWhile, List.dropWhile, hd, h1, h2]

theorem digitsThen_spec (lo hi : Nat) (c : Char) (ds r : List Char)
    (hds : ∀ d ∈ ds, isDigitC d = true) (hc : isDigitC c = false)
    (h1 : lo ≤ ds.length) (h2 : ds.length ≤ hi) :
    digitsThen lo hi c (ds ++ c :: r) = some (ds, r) := by
  obtain ⟨ht, hd⟩ := takeWhile_digits ds c r hds hc
  simp [digitsThen, ht, hd, h1, h2]

theorem tagAt_shape (mm fr : List Char) (s1 s2 : Char) (x : List Char)
    (hmm : ∀ c ∈ mm, isDigitC c = true) (hm1 : 1 ≤ mm.length) (hm3 : mm.length ≤ 3)
    (h1 : isDigitC s1 = true) (h2 : isDigitC s2 = true)
    (hfr : ∀ c ∈ fr, isDigitC c = true) (hf1 : 1 ≤ fr.length) (hf3 : fr.length ≤ 3) :
    tagAt ('[' :: (mm ++ ':' :: s1 :: s2 :: '.' :: (fr ++ ']' :: x))) =
      some (mm.length + fr.length + 6,
        (digitsVal mm * 60 + digitsVal [s1, s2]) * 1000 + fracMs fr) := by
  have e1 := digitsThen_spec 1 3 ':' mm (s1 :: s2 :: '.' :: (fr ++ ']' :: x)) hmm (by decide) hm1 hm3
  have e2 := digitsThen_spec 1 3 ']' fr x hfr (by decide) hf1 hf3
  simp [tagAt, e1, fracPart, e2, h1, h2]

theorem fmtTag_nat (n : Nat) :
    fmtTag n = '[' :: (pad2 (decimal (n / 60000)) ++ ':' :: (pad2 (decimal (n / 1000 % 60)) ++
      '.' :: (pad2 (decimal (n % 1000 / 10)) ++ [']']))) := by
  have e1 : (n : Int).tdiv 60000 = ((n / 60000 : Nat) : Int) := by simp [Int.tdiv]
  have e2 : (n : Int).tdiv 1000 % 60 = ((n / 1000 % 60 : Nat) : Int) := by
    have : (n : Int).tdiv 1000 = ((n / 1000 : Nat) : Int) := by simp [Int.tdiv]
    rw [this]; omega
  have e3 : (n : Int) % 1000 / 10 = ((n % 1000 / 10 : Nat) : Int) := by omega
  have ei : ∀ k : Nat, intStr (k : Int) = decimal k := by intro k; simp [intStr]
  unfold fmtTag
  rw [e1, e2, e3, ei, ei, ei]
  simp only [List.cons_append, List.append_assoc]

/-- The timestamp round trip, at the head of a line: the tag `to_lrc` writes
for `0 ≤ t < 1000 min` is one tag, of `t` rounded down to the centisecond. -/
theorem tagAt_fmtTag (t : Int) (h0 : 0 ≤ t) (h1 : t < 60000000) (x : List Char) :
    tagAt (fmtTag t ++ x) = some ((fmtTag t).length, (t / 10 * 10).toNat) := by
  obtain ⟨n, rfl⟩ := Int.eq_ofNat_of_zero_le h0
  have hn : n < 60000000 := by omega
  rw [fmtTag_nat]
  obtain ⟨s1, s2, hs⟩ := two_of_length (pad2_decimal_length_two (n / 1000 % 60) (by omega))
  obtain ⟨c1, c2, hc⟩ := two_of_length (pad2_decimal_length_two (n % 1000 / 10) (by omega))
  have hsd := pad2_decimal_digits (n / 1000 % 60)
  have hcd := pad2_decimal_digits (n % 1000 / 10)
  have hsv := digitsVal_pad2_decimal (n / 1000 % 60)
  have hcv := digitsVal_pad2_decimal (n % 1000 / 10)
  rw [hs] at hsd hsv ⊢
  rw [hc] at hcd hcv ⊢
  have hm1 : 1 ≤ (pad2 (decimal (n / 60000))).length ∧ (pad2 (decimal (n / 60000))).length ≤ 3 := by
    by_cases h : n / 60000 < 100
    · rw [pad2_decimal_length_two _ h]; omega
    · rw [pad2_decimal_length_three _ (by omega) (by omega)]; omega
  have e := tagAt_shape (pad2 (decimal (n / 60000))) [c1, c2] s1 s2 x
    (pad2_decimal_digits _) hm1.1 hm1.2 (hsd s1 (by simp)) (hsd s2 (by simp)) hcd
    (by simp) (by simp)
  simp only [List.cons_append, List.append_assoc, List.nil_append] at e ⊢
  rw [e, digitsVal_pad2_decimal, hsv]
  simp only [fracMs, List.length_cons, List.length_nil, hcv]
  simp
  omega

theorem fmtTag_length_pos (t : Int) : (fmtTag t).length ≠ 0 := by simp [fmtTag]

/-- The timestamp round trip: formatting `0 ≤ t < 1000 min` and parsing it
gives one tag, of `t` rounded down to the centisecond, whatever follows. -/
theorem scan_fmtTag (t : Int) (h0 : 0 ≤ t) (h1 : t < 60000000) (x : List Char) :
    scan (fmtTag t ++ x) = ((t / 10 * 10).toNat :: (scan x).1, (scan x).2) := by
  rw [scan_of_tag (tagAt_fmtTag t h0 h1 x), List.drop_left]

theorem scan_fmtTag_alone (t : Int) (h0 : 0 ≤ t) (h1 : t < 60000000) :
    scan (fmtTag t) = ([(t / 10 * 10).toNat], []) := by
  simpa using scan_fmtTag t h0 h1 []

theorem tagAt_not_bracket {c : Char} (r : List Char) (h : c ≠ '[') : tagAt (c :: r) = none := by
  simp [tagAt, h]

/-- Text without a `[` holds no tag: the scan keeps it whole. -/
theorem scan_noBracket (p y : List Char) (hp : ∀ c ∈ p, c ≠ '[') :
    scan (p ++ y) = ((scan y).1, p ++ (scan y).2) := by
  induction p with
  | nil => simp
  | cons c p ih =>
    rw [List.cons_append, scan_of_none (tagAt_not_bracket _ (hp c (by simp))),
      ih (fun x hx => hp x (by simp [hx]))]
    simp

theorem isDigitC_ne {c : Char} (h : isDigitC c = true) :
    c ≠ '[' ∧ c ≠ '\r' ∧ c ≠ '\n' ∧ c ≠ ']' ∧ c ≠ ':' ∧ c ≠ '.' := by
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩ <;> (rintro rfl; revert h; decide)

/-- From 1000 minutes on, `to_lrc`'s tag is no tag: its minutes take four
digits, and nothing in it opens another. -/
theorem scan_fmtTag_long (t : Int) (h : 60000000 ≤ t) (x : List Char) :
    scan (fmtTag t ++ x) = ((scan x).1, fmtTag t ++ (scan x).2) := by
  obtain ⟨n, rfl⟩ := Int.eq_ofNat_of_zero_le (show 0 ≤ t by omega)
  rw [fmtTag_nat]
  have hlong := pad2_decimal_length_long (n / 60000) (by omega)
  have hd := pad2_decimal_digits (n / 60000)
  generalize pad2 (decimal (n / 60000)) = mm at hlong hd
  generalize hrest : (':' :: (pad2 (decimal (n / 1000 % 60)) ++
    '.' :: (pad2 (decimal (n % 1000 / 10)) ++ [']']))) = rest
  have hnb : ∀ c ∈ mm ++ rest, c ≠ '[' := by
    intro c hc
    rw [List.mem_append] at hc
    rcases hc with hc | hc
    · exact (isDigitC_ne (hd c hc)).1
    · rw [← hrest] at hc
      simp only [List.mem_cons, List.mem_append] at hc
      rcases hc with rfl | hc | rfl | hc | hc
      · decide
      · exact (isDigitC_ne (pad2_decimal_digits _ c hc)).1
      · decide
      · exact (isDigitC_ne (pad2_decimal_digits _ c hc)).1
      · simp at hc; subst hc; decide
  have hnone : tagAt ('[' :: (mm ++ rest ++ x)) = none := by
    have ht := takeWhile_digits mm ':' (rest.tail ++ x) hd (by decide)
    have : rest = ':' :: rest.tail := by rw [← hrest]; rfl
    have hdt : digitsThen 1 3 ':' (mm ++ ':' :: (rest.tail ++ x)) = none := by
      simp only [digitsThen, ht.1, ht.2]
      split
      · omega
      · rfl
    rw [this]
    simp only [List.cons_append, List.append_assoc] at hdt ⊢
    simp [tagAt, hdt]
  simp only [List.cons_append, List.append_assoc] at hnone ⊢
  rw [scan_of_none hnone, ← List.append_assoc, scan_noBracket _ _ hnb]
  simp

/-! ## Trimming -/

theorem trimR_append (x y : List Char) :
    trimR (x ++ y) = if trimR y = [] then trimR x else x ++ trimR y := by
  induction x with
  | nil => split <;> simp_all [trimR]
  | cons c x ih =>
    simp only [List.cons_append, trimR, ih]
    split
    · rfl
    · rename_i h
      have : (x ++ trimR y).isEmpty = false := by
        cases x <;> simp [List.isEmpty_iff, h]
      simp [this]

theorem trimR_snoc (x : List Char) (c : Char) (hc : isDartWs c = false) :
    trimR (x ++ [c]) = x ++ [c] := by
  rw [trimR_append]
  have : trimR [c] = [c] := by simp [trimR, hc]
  simp [this]

theorem trimR_trimR (s : List Char) : trimR (trimR s) = trimR s := by
  induction s with
  | nil => rfl
  | cons c cs ih =>
    simp only [trimR]
    split
    · rfl
    · rename_i h
      simp only [trimR, ih]
      simp [h]

theorem dartTrim_trimR (s : List Char) : dartTrim (trimR s) = dartTrim s := by
  simp [dartTrim, trimR_trimR]

theorem fmtTag_snoc (t : Int) : ∃ a, fmtTag t = a ++ [']'] :=
  ⟨'[' :: (pad2 (intStr (t.tdiv 60000)) ++ ':' :: (pad2 (intStr (t.tdiv 1000 % 60)) ++
    '.' :: pad2 (intStr (t % 1000 / 10)))), by simp [fmtTag]⟩

theorem fmtTag_head (t : Int) : ∃ r, fmtTag t = '[' :: r := ⟨_, rfl⟩

/-- A `to_lrc` line, trimmed: the tag stays, the text loses its trailing
white space. -/
theorem dartTrim_fmtTag (t : Int) (text : List Char) :
    dartTrim (fmtTag t ++ text) = fmtTag t ++ trimR text := by
  have hR : trimR (fmtTag t ++ text) = fmtTag t ++ trimR text := by
    rw [trimR_append]
    split
    · rename_i h
      obtain ⟨a, ha⟩ := fmtTag_snoc t
      rw [h, ha, trimR_snoc _ _ (by decide)]; simp
    · rfl
  obtain ⟨r, hr⟩ := fmtTag_head t
  have hb : isDartWs '[' = false := by decide
  rw [dartTrim, hR, hr]
  simp [trimL, List.dropWhile, hb]

theorem scan_noTags : ∀ (s : List Char), (scan s).1 = [] → (scan s).2 = s
  | [] => by simp
  | c :: cs => by
    intro h
    cases ht : tagAt (c :: cs) with
    | some p =>
      obtain ⟨n, v⟩ := p
      rw [scan_of_tag ht] at h
      simp at h
    | none =>
      rw [scan_of_none ht] at h ⊢
      simp only at h ⊢
      rw [scan_noTags cs h]

theorem isDartWs_ne_bracket {c : Char} (h : isDartWs c = true) : c ≠ '[' := by
  rintro rfl; revert h; decide

/-- A text whose trimmed form holds no tag: its untrimmed end holds none
either. -/
theorem scan_trimR (s : List Char) (h : (scan (dartTrim s)).1 = []) :
    scan (trimR s) = ([], trimR s) := by
  have e := @List.takeWhile_append_dropWhile _ isDartWs (trimR s)
  have hw : ∀ c ∈ (trimR s).takeWhile isDartWs, c ≠ '[' :=
    fun c hc => isDartWs_ne_bracket (List.all_eq_true.mp List.all_takeWhile c hc)
  rw [← e, scan_noBracket _ _ hw]
  have hd : (trimR s).dropWhile isDartWs = dartTrim s := rfl
  rw [hd, h, scan_noTags _ h]

/-! ## Lines -/

theorem splitLines_line (line rest : List Char) (h : ∀ c ∈ line, c ≠ '\r' ∧ c ≠ '\n') :
    splitLines (line ++ '\n' :: rest) = line :: splitLines rest := by
  induction line with
  | nil => rw [List.nil_append, splitLines.eq_def]; simp
  | cons c l ih =>
    have hc := h c (by simp)
    rw [List.cons_append, splitLines.eq_def]
    simp only [if_neg hc.1, if_neg hc.2, ih (fun x hx => h x (by simp [hx]))]

theorem pad2_intStr_chars (k : Int) : ∀ c ∈ pad2 (intStr k), c = '-' ∨ isDigitC c = true := by
  intro c hc
  simp only [pad2, List.mem_append, List.mem_replicate] at hc
  rcases hc with ⟨-, rfl⟩ | hc
  · right; decide
  · unfold intStr at hc
    split at hc
    · rcases List.mem_cons.mp hc with rfl | hc
      · left; rfl
      · right; exact decimal_digits _ c hc
    · right; exact decimal_digits _ c hc

/-- What `to_lrc`'s tag is made of. -/
theorem fmtTag_chars (t : Int) :
    ∀ c ∈ fmtTag t, c = '[' ∨ c = ':' ∨ c = '.' ∨ c = ']' ∨ c = '-' ∨ isDigitC c = true := by
  intro c hc
  have p1 := pad2_intStr_chars (t.tdiv 60000) c
  have p2 := pad2_intStr_chars (t.tdiv 1000 % 60) c
  have p3 := pad2_intStr_chars (t % 1000 / 10) c
  simp only [fmtTag, List.cons_append, List.append_assoc, List.mem_cons, List.mem_append,
    List.mem_singleton] at hc
  rcases hc with h | h | h | h | h | h | h <;> simp_all

theorem renderLine_noBreak (l : LyricLine) (hl : ∀ c ∈ l.text, c ≠ '\r' ∧ c ≠ '\n') :
    ∀ c ∈ renderLine l, c ≠ '\r' ∧ c ≠ '\n' := by
  intro c hc
  unfold renderLine at hc
  split at hc
  · rw [List.mem_append] at hc
    rcases hc with hc | hc
    · rcases fmtTag_chars _ c hc with rfl | rfl | rfl | rfl | rfl | h
      any_goals decide
      exact ⟨(isDigitC_ne h).2.1, (isDigitC_ne h).2.2.1⟩
    · exact hl c hc
  · exact hl c hc

theorem toLrc_cons (l : LyricLine) (ls : List LyricLine) :
    toLrc (l :: ls) = renderLine l ++ '\n' :: toLrc ls := by
  simp [toLrc]

theorem splitLines_toLrc (ls : List LyricLine) (h : ∀ l ∈ ls, ∀ c ∈ l.text, c ≠ '\r' ∧ c ≠ '\n') :
    splitLines (toLrc ls) = ls.map renderLine ++ [[]] := by
  induction ls with
  | nil => simp [toLrc, splitLines]
  | cons l ls ih =>
    rw [toLrc_cons, splitLines_line _ _ (renderLine_noBreak l (h l (by simp))),
      ih (fun l' hl' => h l' (by simp [hl']))]
    simp

/-! ## Offsets -/

theorem offsetOf_digit (d : Char) (r : List Char) (hd : isDigitC d = true) :
    offsetOf ('[' :: d :: r) = none := by
  have hl : lowerAscii d = d := by
    simp only [isDigitC, Bool.and_eq_true, decide_eq_true_eq] at hd
    simp only [lowerAscii]
    rw [if_neg (by omega)]
  have hne : d ≠ 'o' := by rintro rfl; revert hd; decide
  have hs : "offset:".toList = ['o', 'f', 'f', 's', 'e', 't', ':'] := rfl
  simp [offsetOf, offsetTag, hs, hl, hne]

theorem offsetOf_fmtTag (t : Int) (h0 : 0 ≤ t) (y : List Char) : offsetOf (fmtTag t ++ y) = none := by
  obtain ⟨n, rfl⟩ := Int.eq_ofNat_of_zero_le h0
  rw [fmtTag_nat]
  have hd := pad2_decimal_digits (n / 60000)
  have hl : (pad2 (decimal (n / 60000))).length ≠ 0 := by
    have := decimal_length_pos (n / 60000)
    rw [pad2, List.length_append]; omega
  generalize pad2 (decimal (n / 60000)) = mm at hd hl
  cases mm with
  | nil => simp at hl
  | cons d r =>
    simp only [List.cons_append, List.append_assoc]
    exact offsetOf_digit d _ (hd d (by simp))

theorem foldl_const {α β : Type} (f : β → α → β) (l : List α) (o : β)
    (h : ∀ x ∈ l, ∀ o, f o x = o) : l.foldl f o = o := by
  induction l generalizing o with
  | nil => rfl
  | cons x xs ih => rw [List.foldl_cons, h x (by simp), ih _ (fun y hy => h y (by simp [hy]))]

theorem docOffset_none (lines : List (List Char)) (h : ∀ raw ∈ lines, offsetOf (dartTrim raw) = none) :
    docOffset lines = 0 := by
  unfold docOffset
  apply foldl_const
  intro raw hr o
  simp [h raw hr]

/-! ## The round trip -/

/-- What `parse_lrc` reads back of a line `to_lrc` wrote: a timed line below
1000 minutes, its text trimmed and its time rounded down to the centisecond;
nothing of an untimed line or a later one. -/
def survives (l : LyricLine) : Option LyricLine :=
  match l.time with
  | some t =>
    if t < 60000000 then some { text := dartTrim l.text, time := some (t / 10 * 10) } else none
  | none => none

/-- A line `to_lrc` writes as one line that reads back as itself: no line
break in its text, no time tag in its trimmed text, a time not negative,
and, untimed, not an `[offset:]` line. -/
structure Writable (l : LyricLine) : Prop where
  noBreak : ∀ c ∈ l.text, c ≠ '\r' ∧ c ≠ '\n'
  noTag : (scan (dartTrim l.text)).1 = []
  nonneg : ∀ t, l.time = some t → 0 ≤ t
  noOffset : l.time = none → offsetOf (dartTrim l.text) = none

/-- The characters a written line adds to `parse_lrc`'s count. -/
def keptCost (l : LyricLine) : Nat :=
  match survives l with
  | some k => utf16Len k.text + 1
  | none => 0

theorem emit_toLrc (budget e : Nat) (ls : List LyricLine) (hw : ∀ l ∈ ls, Writable l)
    (hb : e + (ls.map keptCost).sum ≤ budget) :
    emit 0 budget e (ls.map renderLine ++ [[]]) = some (ls.filterMap survives) := by
  induction ls generalizing e with
  | nil => simp [emit, dartTrim, trimL, trimR]
  | cons l ls ih =>
    have w := hw l (by simp)
    have hw' : ∀ l ∈ ls, Writable l := fun l' h' => hw l' (by simp [h'])
    obtain ⟨text, time⟩ := l
    simp only [List.map_cons, List.cons_append, List.filterMap_cons, List.sum_cons] at hb ⊢
    cases time with
    | none =>
      have hs : survives ⟨text, none⟩ = none := rfl
      have hc : keptCost ⟨text, none⟩ = 0 := rfl
      simp only [hs, hc, renderLine] at hb ⊢
      simp only [emit, w.noTag, List.isEmpty_nil, if_true, ite_self]
      exact ih e hw' (by omega)
    | some t =>
      have h0 := w.nonneg t rfl
      by_cases ht : t < 60000000
      · have hs : survives ⟨text, some t⟩ =
            some { text := dartTrim text, time := some (t / 10 * 10) } := by
          simp [survives, ht]
        have hc : keptCost ⟨text, some t⟩ = utf16Len (dartTrim text) + 1 := by
          simp [keptCost, hs]
        rw [hc] at hb
        rw [hs]
        simp only [renderLine]
        obtain ⟨r, hr⟩ := fmtTag_head t
        have hscan := scan_fmtTag t h0 ht (trimR text)
        rw [scan_trimR text w.noTag] at hscan
        have hne : (fmtTag t ++ trimR text).isEmpty = false := by rw [hr]; rfl
        simp only [emit, dartTrim_fmtTag, hne, hscan, dartTrim_trimR]
        simp only [Bool.false_eq_true, if_false, List.isEmpty_cons, List.length_singleton,
          Nat.mul_one]
        rw [if_neg (by omega), ih _ hw' (by omega)]
        have : ((t / 10 * 10).toNat : Int) = t / 10 * 10 := Int.toNat_of_nonneg (by omega)
        simp only [Option.map_some, List.map_cons, List.map_nil, List.cons_append, List.nil_append,
          this]
        congr 4
        omega
      · have hs : survives ⟨text, some t⟩ = none := by simp [survives, ht]
        have hc : keptCost ⟨text, some t⟩ = 0 := by simp [keptCost, hs]
        rw [hc] at hb
        rw [hs]
        simp only [renderLine]
        obtain ⟨r, hr⟩ := fmtTag_head t
        have hscan := scan_fmtTag_long t (by omega) (trimR text)
        rw [scan_trimR text w.noTag] at hscan
        have hne : (fmtTag t ++ trimR text).isEmpty = false := by rw [hr]; rfl
        simp only [emit, dartTrim_fmtTag, hne, hscan]
        simp only [Bool.false_eq_true, if_false, List.isEmpty_nil, if_true]
        exact ih e hw' (by omega)

theorem utf16Len_append (a b : List Char) : utf16Len (a ++ b) = utf16Len a + utf16Len b := by
  induction a with
  | nil => simp [utf16Len]
  | cons c a ih => simp only [List.cons_append, utf16Len, ih]; omega

theorem utf16Len_sublist {a b : List Char} (h : a.Sublist b) : utf16Len a ≤ utf16Len b := by
  induction h with
  | slnil => exact Nat.le_refl _
  | cons c _ ih => simp only [utf16Len]; omega
  | cons₂ c _ ih => simp only [utf16Len]; omega

theorem trimR_sublist (s : List Char) : (trimR s).Sublist s := by
  induction s with
  | nil => exact List.Sublist.slnil
  | cons c cs ih =>
    simp only [trimR]
    split
    · exact List.nil_sublist _
    · exact ih.cons₂ c

theorem dartTrim_sublist (s : List Char) : (dartTrim s).Sublist s :=
  (List.dropWhile_sublist _).trans (trimR_sublist s)

theorem keptCost_le (l : LyricLine) : keptCost l ≤ utf16Len (renderLine l) + 1 := by
  obtain ⟨text, time⟩ := l
  cases time with
  | none => simp [keptCost, survives]
  | some t =>
    unfold keptCost survives renderLine
    split
    · simp only [Option.some.injEq] at *
      rename_i k hk
      split at hk
      · simp only [Option.some.injEq] at hk
        subst hk
        have := utf16Len_sublist (dartTrim_sublist text)
        rw [utf16Len_append]; simp only; omega
      · simp at hk
    · omega

theorem keptCost_sum_le (ls : List LyricLine) : (ls.map keptCost).sum ≤ utf16Len (toLrc ls) := by
  induction ls with
  | nil => simp
  | cons l ls ih =>
    rw [toLrc_cons, List.map_cons, List.sum_cons, utf16Len_append]
    have := keptCost_le l
    simp only [utf16Len]
    have : ('\n' : Char).toNat < 0x10000 := by decide
    simp only [this, if_true]
    omega

/-- The document round trip: for lines `to_lrc` can write as lines of their
own, parsing what it wrote gives, sorted stably by time, exactly the timed
lines below 1000 minutes, with their text trimmed and their time rounded
down to the centisecond. -/
theorem parseLrc_toLrc (ls : List LyricLine) (hw : ∀ l ∈ ls, Writable l) :
    parseLrc (toLrc ls) = some (sortByTime (ls.filterMap survives)) := by
  have hsplit := splitLines_toLrc ls (fun l hl => (hw l hl).noBreak)
  have hoff : docOffset (ls.map renderLine ++ [[]]) = 0 := by
    apply docOffset_none
    intro raw hr
    simp only [List.mem_append, List.mem_map, List.mem_singleton] at hr
    rcases hr with ⟨⟨text, time⟩, hl, rfl⟩ | rfl
    · cases time with
      | none => exact (hw _ hl).noOffset rfl
      | some t =>
        simp only [renderLine, dartTrim_fmtTag]
        exact offsetOf_fmtTag t ((hw _ hl).nonneg t rfl) _
    · simp [dartTrim, trimL, trimR, offsetOf, offsetTag]
  unfold parseLrc
  simp only [hsplit, hoff]
  rw [emit_toLrc _ 0 ls hw]
  · rfl
  · have := keptCost_sum_le ls
    omega

/-! ## Consequences -/

/-- Lines in time order. -/
def InOrder (ls : List LyricLine) : Prop := ls.Pairwise fun a b => timeKey a ≤ timeKey b

theorem sortByTime_inOrder_id (ls : List LyricLine) (h : InOrder ls) : sortByTime ls = ls := by
  induction ls with
  | nil => rfl
  | cons x xs ih =>
    unfold InOrder at h
    rw [List.pairwise_cons] at h
    rw [sortByTime, ih h.2]
    cases xs with
    | nil => rfl
    | cons y ys => simp [insertByTime, h.1 y (by simp)]

theorem filterMap_eq_map {α β : Type} (f : α → Option β) (g : α → β) (l : List α)
    (h : ∀ x ∈ l, f x = some (g x)) : l.filterMap f = l.map g := by
  induction l with
  | nil => rfl
  | cons x xs ih =>
    rw [List.filterMap_cons, h x (by simp), ih (fun y hy => h y (by simp [hy]))]
    rfl

/-- Lines in time order come back in their own order, each trimmed and
rounded down to the centisecond. -/
theorem parseLrc_toLrc_inOrder (ls : List LyricLine) (hw : ∀ l ∈ ls, Writable l)
    (ht : ∀ l ∈ ls, ∃ t, l.time = some t ∧ t < 60000000) (hs : InOrder ls) :
    parseLrc (toLrc ls) =
      some (ls.map fun l => { text := dartTrim l.text, time := l.time.map (· / 10 * 10) }) := by
  rw [parseLrc_toLrc ls hw, filterMap_eq_map]
  · rw [sortByTime_inOrder_id]
    unfold InOrder at hs ⊢
    rw [List.pairwise_map]
    refine hs.imp_of_mem (fun ha hb hab => ?_)
    obtain ⟨ta, hta, -⟩ := ht _ ha
    obtain ⟨tb, htb, -⟩ := ht _ hb
    simp only [timeKey, hta, htb, Option.map_some, Option.getD_some] at hab ⊢
    omega
  · intro l hl
    obtain ⟨t, htl, hlt⟩ := ht l hl
    obtain ⟨text, time⟩ := l
    simp only at htl
    subst htl
    simp [survives, hlt]

/-- Lines already as `parse_lrc` gives them — trimmed, at whole
centiseconds below 1000 minutes, in time order — come back unchanged. -/
theorem parseLrc_toLrc_normal (ls : List LyricLine) (hw : ∀ l ∈ ls, Writable l)
    (hn : ∀ l ∈ ls, dartTrim l.text = l.text ∧ ∃ t, l.time = some t ∧ t < 60000000 ∧ t % 10 = 0)
    (hs : InOrder ls) : parseLrc (toLrc ls) = some ls := by
  rw [parseLrc_toLrc_inOrder ls hw (fun l hl => by
    obtain ⟨-, t, h1, h2, -⟩ := hn l hl; exact ⟨t, h1, h2⟩) hs]
  congr 1
  conv => rhs; rw [← List.map_id ls]
  apply List.map_congr_left
  intro l hl
  obtain ⟨htext, t, htl, -, h10⟩ := hn l hl
  obtain ⟨text, time⟩ := l
  simp only at htext htl
  subst htl
  simp only [htext, Option.map_some, id, LyricLine.mk.injEq, true_and, Option.some.injEq]
  omega

theorem dropWhile_dropWhile {α : Type} (p : α → Bool) (l : List α) :
    (l.dropWhile p).dropWhile p = l.dropWhile p := by
  induction l with
  | nil => rfl
  | cons x xs ih =>
    simp only [List.dropWhile]
    cases hp : p x
    · simp [List.dropWhile, hp]
    · exact ih

theorem trimR_dropWhile (y : List Char) : trimR (y.dropWhile isDartWs) = (trimR y).dropWhile isDartWs := by
  induction y with
  | nil => rfl
  | cons c cs ih =>
    cases hc : isDartWs c
    · have : (List.dropWhile isDartWs (c :: cs)) = c :: cs := by simp [List.dropWhile, hc]
      rw [this]
      simp [trimR, hc, List.dropWhile]
    · have : (List.dropWhile isDartWs (c :: cs)) = cs.dropWhile isDartWs := by
        simp [List.dropWhile, hc]
      rw [this, ih]
      simp only [trimR, hc, Bool.and_true]
      split
      · rename_i h
        rw [List.isEmpty_iff] at h
        simp [h]
      · simp [List.dropWhile, hc]

theorem dartTrim_dartTrim (s : List Char) : dartTrim (dartTrim s) = dartTrim s := by
  simp only [dartTrim, trimL]
  rw [trimR_dropWhile, trimR_trimR, dropWhile_dropWhile]

theorem mem_insertByTime (x y : LyricLine) (l : List LyricLine) :
    y ∈ insertByTime x l ↔ y = x ∨ y ∈ l := by
  induction l with
  | nil => simp [insertByTime]
  | cons z zs ih =>
    simp only [insertByTime]
    split
    · simp
    · simp only [List.mem_cons, ih]
      constructor
      · rintro (h | h | h) <;> simp [h]
      · rintro (h | h | h) <;> simp [h]

theorem mem_sortByTime (y : LyricLine) (l : List LyricLine) : y ∈ sortByTime l ↔ y ∈ l := by
  induction l with
  | nil => simp [sortByTime]
  | cons x xs ih => simp [sortByTime, mem_insertByTime, ih]

theorem inOrder_insertByTime (x : LyricLine) (l : List LyricLine) (h : InOrder l) :
    InOrder (insertByTime x l) := by
  induction l with
  | nil => simp [insertByTime, InOrder]
  | cons z zs ih =>
    unfold InOrder at h ⊢
    rw [List.pairwise_cons] at h
    simp only [insertByTime]
    split
    · rename_i hxz
      rw [List.pairwise_cons]
      refine ⟨fun b hb => ?_, List.pairwise_cons.mpr h⟩
      rcases List.mem_cons.mp hb with rfl | hb
      · exact hxz
      · exact Int.le_trans hxz (h.1 b hb)
    · rename_i hxz
      rw [List.pairwise_cons]
      refine ⟨fun b hb => ?_, ih h.2⟩
      rcases (mem_insertByTime x b zs).mp hb with rfl | hb
      · omega
      · exact h.1 b hb

theorem inOrder_sortByTime (l : List LyricLine) : InOrder (sortByTime l) := by
  induction l with
  | nil => simp [sortByTime, InOrder]
  | cons x xs ih => exact inOrder_insertByTime x _ ih

/-- Saving again changes nothing: what `parse_lrc` read back is written and
read back as itself. -/
theorem parseLrc_toLrc_idem (ls : List LyricLine) (hw : ∀ l ∈ ls, Writable l) :
    ∃ out, parseLrc (toLrc ls) = some out ∧ parseLrc (toLrc out) = some out := by
  refine ⟨_, parseLrc_toLrc ls hw, ?_⟩
  have hk : ∀ k ∈ sortByTime (ls.filterMap survives), ∃ l ∈ ls, ∃ t, l.time = some t ∧ 0 ≤ t ∧
      t < 60000000 ∧ k = { text := dartTrim l.text, time := some (t / 10 * 10) } := by
    intro k hk
    rw [mem_sortByTime, List.mem_filterMap] at hk
    obtain ⟨l, hl, hs⟩ := hk
    obtain ⟨text, time⟩ := l
    cases time with
    | none => simp [survives] at hs
    | some t =>
      by_cases ht : t < 60000000
      · simp only [survives, ht, if_true, Option.some.injEq] at hs
        exact ⟨_, hl, t, rfl, (hw _ hl).nonneg t rfl, ht, hs.symm⟩
      · simp [survives, ht] at hs
  apply parseLrc_toLrc_normal
  · intro k hkm
    obtain ⟨l, hl, t, -, h0, -, rfl⟩ := hk k hkm
    exact {
      noBreak := fun c hc => (hw l hl).noBreak c ((dartTrim_sublist _).subset hc)
      noTag := by simpa [dartTrim_dartTrim] using (hw l hl).noTag
      nonneg := by intro t' ht'; simp only [Option.some.injEq] at ht'; omega
      noOffset := by intro h; simp at h }
  · intro k hkm
    obtain ⟨l, hl, t, -, h0, h1, rfl⟩ := hk k hkm
    exact ⟨dartTrim_dartTrim _, t / 10 * 10, rfl, by omega, by omega⟩
  · exact inOrder_sortByTime _

end Vocara
