/-!
# The byte-budgeted LRU trim

Mirrors `BudgetState::trim` (`vocara_model::util::lru_bytes_cache`, ported
from `lib/utils/lru_bytes_cache.dart`): after an insert, entries are evicted
globally-oldest first until the total fits the budget, never one under the
key just inserted (in the inserter's partner caches), so an oversized result
is still served whole.

Model: the entries in recency order (oldest first) as `(key, bytes)` pairs, a
`keep` predicate (the inserted key), and the trim as repeated removal of the
first entry not kept. Proved:

* `trim_fits_or_only_kept`: afterwards the total fits the budget, or only
  kept entries are left;
* `trim_keeps_kept`: a kept entry is never evicted (the result just inserted
  is always served);
* `trim_sublist`: the trim only removes, keeping the survivors' order;
* `trim_evicts_oldest`: the unkept survivors are a SUFFIX of the unkept
  entries — eviction takes the least recently used first, never a newer
  entry ahead of an older one;
* `trim_noop_when_fits`: nothing is evicted while the total already fits.
-/

namespace Vocara

abbrev CacheEntry := Nat × Nat

def total (l : List CacheEntry) : Nat := (l.map Prod.snd).sum

/-- Removes the first entry not kept; `none` when every entry is kept. -/
def dropFirst (keep : CacheEntry → Bool) : List CacheEntry → Option (List CacheEntry)
  | [] => none
  | e :: es => if keep e then (dropFirst keep es).map (e :: ·) else some es

/-- Evict (at most [n] times) until the total fits [max]. -/
def trimN (keep : CacheEntry → Bool) (max : Nat) : Nat → List CacheEntry → List CacheEntry
  | 0, l => l
  | n + 1, l =>
    if total l ≤ max then l
    else match dropFirst keep l with
      | none => l
      | some l' => trimN keep max n l'

/-- The trim: as many evictions as there are entries is always enough. -/
def trim (keep : CacheEntry → Bool) (max : Nat) (l : List CacheEntry) : List CacheEntry :=
  trimN keep max l.length l

theorem dropFirst_none (keep : CacheEntry → Bool) :
    ∀ l, dropFirst keep l = none → ∀ e ∈ l, keep e = true
  | [], _ => by simp
  | e :: es, h => by
    simp only [dropFirst] at h
    split at h
    · rename_i hk
      have hes : dropFirst keep es = none := by
        cases hd : dropFirst keep es with
        | none => rfl
        | some _ => rw [hd] at h; simp at h
      intro x hx
      simp only [List.mem_cons] at hx
      rcases hx with rfl | hx
      · exact hk
      · exact dropFirst_none keep es hes x hx
    · simp at h

theorem dropFirst_sublist (keep : CacheEntry → Bool) :
    ∀ l l', dropFirst keep l = some l' → l'.Sublist l
  | [], _, h => by simp [dropFirst] at h
  | e :: es, l', h => by
    simp only [dropFirst] at h
    split at h
    · cases hd : dropFirst keep es with
      | none => rw [hd] at h; simp at h
      | some m =>
        rw [hd] at h
        simp only [Option.map_some, Option.some.injEq] at h
        subst h
        exact (dropFirst_sublist keep es m hd).cons₂ e
    · simp only [Option.some.injEq] at h
      subst h
      exact List.sublist_cons_self e es

theorem dropFirst_keeps (keep : CacheEntry → Bool) :
    ∀ l l', dropFirst keep l = some l' → ∀ x ∈ l, keep x = true → x ∈ l'
  | [], _, h => by simp [dropFirst] at h
  | e :: es, l', h => by
    intro x hx hkx
    simp only [dropFirst] at h
    split at h
    · cases hd : dropFirst keep es with
      | none => rw [hd] at h; simp at h
      | some m =>
        rw [hd] at h
        simp only [Option.map_some, Option.some.injEq] at h
        subst h
        simp only [List.mem_cons] at hx ⊢
        rcases hx with rfl | hx
        · exact Or.inl rfl
        · exact Or.inr (dropFirst_keeps keep es m hd x hx hkx)
    · rename_i hk
      simp only [Option.some.injEq] at h
      subst h
      simp only [List.mem_cons] at hx
      rcases hx with rfl | hx
      · simp_all
      · exact hx

/-- Dropping the first unkept entry drops the head of the unkept entries. -/
theorem dropFirst_filter (keep : CacheEntry → Bool) :
    ∀ l l', dropFirst keep l = some l' →
      l'.filter (fun e => !keep e) = (l.filter (fun e => !keep e)).tail
  | [], _, h => by simp [dropFirst] at h
  | e :: es, l', h => by
    simp only [dropFirst] at h
    split at h
    · rename_i hk
      cases hd : dropFirst keep es with
      | none => rw [hd] at h; simp at h
      | some m =>
        rw [hd] at h
        simp only [Option.map_some, Option.some.injEq] at h
        subst h
        simp [List.filter_cons, hk, dropFirst_filter keep es m hd]
    · rename_i hk
      simp only [Option.some.injEq] at h
      subst h
      simp [List.filter_cons, hk]

theorem dropFirst_filter_length (keep : CacheEntry → Bool) (l l' : List CacheEntry)
    (h : dropFirst keep l = some l') :
    (l'.filter (fun e => !keep e)).length + 1 = (l.filter (fun e => !keep e)).length := by
  rw [dropFirst_filter keep l l' h]
  cases hf : l.filter (fun e => !keep e) with
  | nil =>
    -- dropFirst found an unkept entry, so the unkept list is not empty.
    exfalso
    have hall : ∀ e ∈ l, keep e = true := by
      intro e he
      have : e ∉ l.filter (fun e => !keep e) := by rw [hf]; simp
      simp [List.mem_filter] at this
      exact this he
    clear hf
    induction l generalizing l' with
    | nil => simp [dropFirst] at h
    | cons x xs ih =>
      simp only [dropFirst, hall x (by simp), if_true] at h
      cases hd : dropFirst keep xs with
      | none => rw [hd] at h; simp at h
      | some m => exact ih m hd (fun e he => hall e (by simp [he]))
  | cons _ _ => simp

theorem trimN_fits (keep : CacheEntry → Bool) (max : Nat) :
    ∀ n l, (l.filter (fun e => !keep e)).length ≤ n →
      total (trimN keep max n l) ≤ max ∨ ∀ e ∈ trimN keep max n l, keep e = true
  | 0, l, h => by
    right
    intro e he
    simp only [trimN] at he
    have hnil : (l.filter (fun e => !keep e)) = [] := List.eq_nil_of_length_eq_zero (by omega)
    cases hk : keep e
    · have hin : e ∈ l.filter (fun e => !keep e) := by simp [List.mem_filter, he, hk]
      rw [hnil] at hin
      simp at hin
    · rfl
  | n + 1, l, h => by
    simp only [trimN]
    split
    · exact Or.inl (by assumption)
    · cases hd : dropFirst keep l with
      | none => exact Or.inr (dropFirst_none keep l hd)
      | some l' =>
        simp only
        apply trimN_fits keep max n l'
        have := dropFirst_filter_length keep l l' hd
        omega

theorem trim_fits_or_only_kept (keep : CacheEntry → Bool) (max : Nat) (l : List CacheEntry) :
    total (trim keep max l) ≤ max ∨ ∀ e ∈ trim keep max l, keep e = true :=
  trimN_fits keep max l.length l (List.length_filter_le _ _)

theorem trimN_keeps (keep : CacheEntry → Bool) (max : Nat) :
    ∀ n l, ∀ x ∈ l, keep x = true → x ∈ trimN keep max n l
  | 0, _, x, hx, _ => by simpa [trimN] using hx
  | n + 1, l, x, hx, hk => by
    simp only [trimN]
    split
    · exact hx
    · cases hd : dropFirst keep l with
      | none => simpa using hx
      | some l' => exact trimN_keeps keep max n l' x (dropFirst_keeps keep l l' hd x hx hk) hk

theorem trim_keeps_kept (keep : CacheEntry → Bool) (max : Nat) (l : List CacheEntry) :
    ∀ x ∈ l, keep x = true → x ∈ trim keep max l :=
  trimN_keeps keep max l.length l

theorem trimN_sublist (keep : CacheEntry → Bool) (max : Nat) :
    ∀ n l, (trimN keep max n l).Sublist l
  | 0, _ => by simp [trimN]
  | n + 1, l => by
    simp only [trimN]
    split
    · exact List.Sublist.refl l
    · cases hd : dropFirst keep l with
      | none => exact List.Sublist.refl l
      | some l' => exact (trimN_sublist keep max n l').trans (dropFirst_sublist keep l l' hd)

theorem trim_sublist (keep : CacheEntry → Bool) (max : Nat) (l : List CacheEntry) :
    (trim keep max l).Sublist l :=
  trimN_sublist keep max l.length l

theorem trimN_suffix (keep : CacheEntry → Bool) (max : Nat) :
    ∀ n l, ((trimN keep max n l).filter (fun e => !keep e)) <:+
      (l.filter (fun e => !keep e))
  | 0, _ => by simp [trimN]
  | n + 1, l => by
    simp only [trimN]
    split
    · exact List.suffix_refl _
    · cases hd : dropFirst keep l with
      | none => exact List.suffix_refl _
      | some l' =>
        simp only
        refine (trimN_suffix keep max n l').trans ?_
        rw [dropFirst_filter keep l l' hd]
        exact List.tail_suffix _

theorem trim_evicts_oldest (keep : CacheEntry → Bool) (max : Nat) (l : List CacheEntry) :
    ((trim keep max l).filter (fun e => !keep e)) <:+ (l.filter (fun e => !keep e)) :=
  trimN_suffix keep max l.length l

theorem trim_noop_when_fits (keep : CacheEntry → Bool) (max : Nat) (l : List CacheEntry)
    (h : total l ≤ max) : trim keep max l = l := by
  unfold trim
  cases hl : l.length with
  | zero => simp [trimN]
  | succ n => simp [trimN, h]

end Vocara
