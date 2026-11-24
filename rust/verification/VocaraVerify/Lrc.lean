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
`%`). The vectors (`lrc_*.json`) replay it through the Rust.
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

end Vocara
