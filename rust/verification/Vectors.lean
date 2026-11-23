import VocaraVerify

/-!
Writes the test vectors the Rust tests replay (`vectors/*.json`), computed by
the very definitions the theorems are about. `lake exe vectors <dir>`.
-/

open Vocara

/-- A deterministic pseudo-random stream (64-bit LCG). -/
def lcg (s : Nat) : Nat := (s * 6364136223846793005 + 1442695040888963407) % 2 ^ 64

def nums (seed count : Nat) : List Nat :=
  (List.range count).foldl (fun (acc : List Nat × Nat) _ =>
    let s := lcg acc.2; (acc.1 ++ [s], s)) ([], seed) |>.1

def jstr (s : String) : String := "\"" ++ s ++ "\""
def jarr (xs : List String) : String := "[" ++ ", ".intercalate xs ++ "]"
def jobj (kvs : List (String × String)) : String :=
  "{" ++ ", ".intercalate (kvs.map fun (k, v) => jstr k ++ ": " ++ v) ++ "}"

def codesToString (l : List Nat) : String := String.mk (l.map Char.ofNat)
def nodeOf (s : String) : List Nat := s.toList.map Char.toNat

/-! ## ULID -/

def ulidCases : List Nat :=
  [0, 1, 31, 32, 2 ^ 80 - 1, 2 ^ 80, 2 ^ 128 - 1, 2 ^ 127, 1700000000000 * 2 ^ 80 + 12345] ++
  (nums 1 40).map (fun x => (x * x * x) % 2 ^ 128)

def ulidJson : String :=
  jarr (ulidCases.map fun v =>
    jobj [("value", jstr (toString v)), ("encoded", jstr (codesToString (encodeUlid v)))])

/-! ## HLC -/

def hlcJson (h : Hlc) : String :=
  jobj [("wall", toString h.wall), ("ctr", toString h.ctr), ("node", jstr (codesToString h.node))]

def ordStr : Ordering → String
  | .lt => "-1" | .eq => "0" | .gt => "1"

def nodes : List String := ["A", "B", "N1", "01HZY0000000000000000000AB", "zz", "A!B"]

def hlcSample (x : Nat) : Hlc :=
  let wall := [0, 1, 999, 1700000000000, 1700000000001, 9999999999999].getD (x % 6) 0
  let ctr := [0, 1, 7, 999998, 999999].getD ((x / 7) % 5) 0
  ⟨wall, ctr, nodeOf (nodes.getD ((x / 37) % 6) "A")⟩

def hlcPackedJson : String :=
  jarr ((nums 2 60).map fun x =>
    let h := hlcSample x
    jobj [("hlc", hlcJson h), ("packed", jstr (codesToString (packed h)))])

def hlcOrderJson : String :=
  jarr ((nums 3 80).map fun x =>
    let a := hlcSample x
    let b := hlcSample (lcg x)
    jobj [("a", hlcJson a), ("b", hlcJson b), ("cmp", ordStr (hlcCmp a b))])

def nowSample (x : Nat) : Nat :=
  [0, 998, 999, 1000, 1700000000000, 1700000000002, 9999999999999].getD (x % 7) 0

def hlcTickJson : String :=
  jarr ((nums 4 60).map fun x =>
    let last := hlcSample x
    let node := nodeOf (nodes.getD ((x / 11) % 6) "A")
    let now := nowSample (x / 3)
    jobj [("last", hlcJson last), ("node", jstr (codesToString node)), ("now", toString now),
      ("result", hlcJson (tick last node now))])

def hlcAdvanceJson : String :=
  jarr ((nums 5 80).map fun x =>
    let last := hlcSample x
    let remote := hlcSample (lcg x)
    let node := nodeOf (nodes.getD ((x / 13) % 6) "A")
    let now := nowSample (x / 5)
    jobj [("last", hlcJson last), ("remote", hlcJson remote), ("node", jstr (codesToString node)),
      ("now", toString now), ("result", hlcJson (advance last remote node now))])

/-! ## LWW -/

def lwwJson : String :=
  jarr ((nums 6 40).map fun x =>
    let mk (y : Nat) : Version String := ⟨hlcSample y, "w" ++ toString (y % 1000)⟩
    let held := mk x
    let ds := (nums (x % 1000) ((x % 6) + 1)).map mk
    let vj (v : Version String) := jobj [("stamp", hlcJson v.stamp), ("payload", jstr v.payload)]
    jobj [("held", vj held), ("deliveries", jarr (ds.map vj)), ("result", vj (applyAll held ds))])

/-! ## Version comparison -/

def verStr (l : List Nat) : String := ".".intercalate (l.map toString)

def versions : List (List Nat) :=
  [[0], [0, 1], [1], [1, 0], [1, 0, 0], [1, 0, 1], [1, 2], [1, 2, 0], [1, 10], [1, 9, 9],
   [2], [2, 0, 0, 0], [10, 0], [0, 0, 1], [1, 1, 0]]

def versionJson : String :=
  let cases := versions.flatMap fun a => versions.flatMap fun b =>
    [false, true].map fun pre =>
      let cur := verStr b ++ (if pre then "-rc.1" else "")
      jobj [("candidate", jstr (verStr a)), ("current", jstr cur),
        ("newer", if isNewerStr (verStr a).toList cur.toList then "true" else "false")]
  -- Other spellings, run through the modelled string parser.
  let extra := [("v1.2.0", "1.1"), ("V2", "1.9.9"), ("1.2.0+42", "1.1.9"), ("1.3", "1.2.0+7"),
    ("1.2.0-rc.1", "1.0"), ("1.x", "1.0"), ("", "1.0"), ("1..2", "1.0"), (" 2", "1.0"),
    ("0x2", "1.0"), ("2 ", "1.0"), ("v", "1.0"), ("1.0.1", "v1.0.0-rc.1+3"), ("1.0.0", "1.0.0+build-7"),
    ("1.0.0", "1.0.0-rc.1"), ("0.9.9", "1.0.0-rc.1"), ("1.0.0-rc.2", "1.0.0-rc.1"), ("1.0.1", "garbage"),
    ("1. 2", "1.0"), ("+5", "1.0"), ("1.0.10", "1.0.9")]
  let extraJson := extra.map fun (c, cur) =>
    jobj [("candidate", jstr c), ("current", jstr cur),
      ("newer", if isNewerStr c.toList cur.toList then "true" else "false")]
  jarr (cases ++ extraJson)

/-! ## Ring -/

/-- Runs pushes (`some v`) and pops (`none`) on the model. -/
def ringRun (cap : Nat) (ops : List (Option Nat)) : List String :=
  let init : Ring Nat := ⟨cap, 0, 0, fun _ => 0⟩
  (ops.foldl (fun (acc : Ring Nat × List String) op =>
    let (r, out) := acc
    match op with
    | some v =>
      match push r v with
      | some r' => (r', out ++ [jobj [("push", toString v), ("ok", "true")]])
      | none => (r, out ++ [jobj [("push", toString v), ("ok", "false")]])
    | none =>
      match pop r with
      | some (v, r') => (r', out ++ [jobj [("pop", toString v)]])
      | none => (r, out ++ [jobj [("pop", "null")]])) (init, [])).2

def ringJson : String :=
  jarr ([2, 4, 8].map fun cap =>
    let ops := (nums cap 200).map fun x => if x % 5 < 3 then some (x % 1000) else none
    jobj [("capacity", toString cap), ("ops", jarr (ringRun cap ops))])

/-! ## Canonical JSON -/

/-- Key atoms as UTF-16 code units (what Dart's `String.compareTo` orders by):
ASCII, Latin-1, a BMP character above the surrogates (U+FF61) and two astral
characters as surrogate pairs (U+1F600, U+10000), where code-unit order and
Unicode scalar order disagree. -/
def keyAtoms : List (List Nat) :=
  [[0x61], [0x62], [0x41], [0x7A], [0x30], [0x5F], [0x20], [0xE9], [0xFF61],
   [0xD83D, 0xDE00], [0xD800, 0xDC00]]

def keyOf (x : Nat) : List Nat :=
  (nums x (x % 3 + 1)).flatMap fun y => keyAtoms.getD (y % keyAtoms.length) []

/-- Distinct keys, as a Dart map has (the last of equal keys kept). -/
def dedupKeys : List (List Nat) → List (List Nat)
  | [] => []
  | k :: ks => if ks.contains k then dedupKeys ks else k :: dedupKeys ks

def entriesJson (es : List (Entry Nat)) : String :=
  jarr (es.map fun (k, v) => jarr [jarr (k.map toString), toString v])

def canonicalJson : String :=
  jarr ((nums 9 60).map fun x =>
    let keys := dedupKeys ((nums x (x % 12 + 1)).map keyOf)
    let es : List (Entry Nat) := (List.range keys.length).zip keys |>.map fun (i, k) => (k, i)
    jobj [("input", entriesJson es), ("canon", entriesJson (canon es))])

/-! ## Reconcile -/

def optJson : Option Nat → String
  | none => "null"
  | some n => toString n

def opName : Option Op → String
  | some .insert => jstr "insert"
  | some .update => jstr "update"
  | some .skip => jstr "skip"
  | some .unchanged => jstr "unchanged"
  | some .tombstone => jstr "tombstone"
  | some .keep => jstr "keep"
  | none => "null"

/-- A side drawn from a small hash domain so equal hashes are common (from
the generator's high bits: an LCG's low bits cycle with a short period). -/
def sideOf (x : Nat) : Option Nat :=
  let h := x / 2 ^ 40
  if h % 4 = 0 then none else some (h / 4 % 3)

def reconcileJson : String :=
  jarr ((nums 11 80).map fun x =>
    let rows := (nums x (x % 9 + 1)).map fun y =>
      let v := nums y 3
      ({ mine := sideOf (v.getD 0 0), db := sideOf (v.getD 1 0), base := sideOf (v.getD 2 0) } : Row)
    jarr ((List.range rows.length).zip rows |>.map fun (i, r) =>
      jobj [("id", jstr ("R" ++ toString i)), ("mine", optJson r.mine), ("db", optJson r.db),
        ("base", optJson r.base), ("op", opName (classify r)), ("after", optJson (after r))]))

/-! ## LRU trim -/

/-- `cache[k] = bytes`: replace any entry under k, append it as the newest,
then trim keeping k. -/
def lruPut (max k b : Nat) (l : List CacheEntry) : List CacheEntry :=
  trim (fun e => e.1 == k) max (l.filter (fun e => e.1 != k) ++ [(k, b)])

/-- `cache[k]`: a read refreshes the entry's recency. -/
def lruGet (k : Nat) (l : List CacheEntry) : List CacheEntry :=
  match l.find? (fun e => e.1 == k) with
  | some e => l.filter (fun e => e.1 != k) ++ [e]
  | none => l

def lruJson : String :=
  jarr ((nums 13 40).map fun x =>
    let max := x / 2 ^ 40 % 60 + 1
    let ops := (nums x 30).map fun y =>
      let h := y / 2 ^ 33
      (h % 3 == 0, h / 3 % 6, h / 18 % 26)
    let step := fun (acc : List CacheEntry × List String) (op : Bool × Nat × Nat) =>
      let (isGet, k, b) := op
      let l := if isGet then lruGet k acc.1 else lruPut max k b acc.1
      let opJson := if isGet then jobj [("get", toString k)]
        else jobj [("put", toString k), ("bytes", toString b)]
      (l, acc.2 ++ [jobj [("op", opJson), ("keys", jarr (l.map fun e => toString e.1))]])
    let (_, steps) := ops.foldl step ([], [])
    jobj [("max", toString max), ("steps", jarr steps)])

/-! ## Oplog asset gate -/

/-- One ledger entry of a multi-row ledger: its sequence, its row, and the
row's entry. -/
structure LEntry where
  seq : Nat
  key : Nat
  e : OpEntry

/-- The carry over a whole ledger: each row's entries through [carry]'s step
(rows never interact). [st] is each row's resolved dependency so far. -/
def carryLedger (st : List (Nat × Option Nat)) : List LEntry → List LEntry
  | [] => []
  | x :: xs =>
    let d := (st.lookup x.key).getD none
    let d' := opStep d x.e
    { x with e := { x.e with dep := if x.e.tomb then x.e.dep else d' } } ::
      carryLedger ((x.key, d') :: st.filter (fun p => p.1 != x.key)) xs

/-- The entries no later entry of the same row follows, each with what its
row resolves to through it (`read_pending`'s collapse: seq-ascending). -/
def collapseLedger (w : List LEntry) : List (LEntry × Option Nat) :=
  (List.range w.length).filterMap fun i =>
    match w[i]? with
    | none => none
    | some x =>
      if (w.drop (i + 1)).any (fun y => y.key == x.key) then none
      else some (x, resolve none (((w.take (i + 1)).filter (fun y => y.key == x.key)).map (·.e)))

def ackLedger (l : List LEntry) (w : Nat) : List LEntry :=
  (carryLedger [] l).filter (fun x => x.seq > w)

def compactLedger (l : List LEntry) : List LEntry :=
  (collapseLedger (carryLedger [] l)).map (·.1)

def lentryJson (x : LEntry) (dep : Option Nat) : String :=
  jobj [("seq", toString x.seq), ("row", toString x.key),
    ("tomb", if x.e.tomb then "true" else "false"), ("dep", optJson dep)]

def ledgerJson (l : List LEntry) : String := jarr (l.map fun x => lentryJson x x.e.dep)

def oplogJson : String :=
  jarr ((nums 17 60).map fun x =>
    let ops := nums x 40
    let step := fun (acc : List LEntry × Nat × List String) (y : Nat) =>
      let (l, next, out) := acc
      let h := y / 2 ^ 33
      let kind := h % 20
      if kind < 11 then
        let ent : OpEntry := ⟨h / 20 % 4 == 0, if h / 80 % 3 == 0 then some (h / 240 % 4 + 1) else none⟩
        let x : LEntry := ⟨next, h / 960 % 3, ent⟩
        (l ++ [x], next + 1, out ++ [jobj [("append", lentryJson x ent.dep)]])
      else if kind < 15 then
        let m := h / 20 % 8 + 1
        let read := collapseLedger (l.take m)
        (l, next, out ++ [jobj [("read", toString m),
          ("expect", jarr (read.map fun p => lentryJson p.1 p.2))]])
      else if kind < 19 then
        let w := match l with
          | [] => next - 1
          | f :: _ => if h / 20 % 5 == 0 then f.seq - 1 else
              (l[h / 100 % l.length]?.map (·.seq)).getD (next - 1)
        let l' := ackLedger l w
        (l', next, out ++ [jobj [("ack", toString w), ("ledger", ledgerJson l')]])
      else
        let l' := compactLedger l
        (l', next, out ++ [jobj [("compact", "true"), ("ledger", ledgerJson l')]])
    let (_, _, steps) := ops.foldl step ([], 1, [])
    jarr steps)

def main (args : List String) : IO Unit := do
  let dir := args.headD "vectors"
  IO.FS.createDirAll dir
  let files := [("ulid.json", ulidJson), ("hlc_packed.json", hlcPackedJson),
    ("hlc_order.json", hlcOrderJson), ("hlc_tick.json", hlcTickJson),
    ("hlc_advance.json", hlcAdvanceJson), ("lww.json", lwwJson),
    ("version.json", versionJson), ("ring.json", ringJson),
    ("canonical.json", canonicalJson), ("reconcile.json", reconcileJson),
    ("lru.json", lruJson), ("oplog.json", oplogJson)]
  for (name, body) in files do
    IO.FS.writeFile (dir ++ "/" ++ name) (body ++ "\n")
    IO.println s!"wrote {dir}/{name}"
