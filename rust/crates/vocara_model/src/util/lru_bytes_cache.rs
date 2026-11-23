//! A byte-budgeted LRU cache for analysis results (port of
//! `lib/utils/lru_bytes_cache.dart`).
//!
//! The per-path analysis caches used to grow without bound: every take,
//! lane aggregate, and EQ spectrogram ever opened in a session stayed
//! resident. Entries here count toward a budget via `byte_of`; inserting
//! past it evicts least-recently-USED entries (reads refresh recency, so the
//! currently displayed subject is always the most recent and never evicted
//! under it). The newest entry is kept even when it alone exceeds the budget
//! — an oversized result is better served once than recomputed forever — and
//! so is every entry its partner caches ([`LruBytesCache::partner_with`]) hold
//! under the same key: a clip's spectrogram and metrics are one analysis held
//! in two caches, and a pair too big for the budget pushed one half out to
//! make room for the other, so the view showing that half computed it again,
//! pushing out the other's.
//!
//! A cache either owns its budget ([`LruBytesCache::new`]) or shares one with
//! its siblings ([`LruBytesCache::shared`]). Sharing is what makes the
//! process's analysis footprint ONE number instead of the sum of several
//! independent ceilings, and it lets the screen the user left pay for the one
//! they are on: eviction always takes the globally least-recently-used entry,
//! whichever cache holds it.
//!
//! Rust shape: the budget owns every member's entries behind one mutex (so
//! an insert can evict from a sibling cache without lock ordering), values
//! are stored type-erased and handed out by clone (hold them in an `Arc`).
//! The `on_evict` hooks run after the lock is released, in eviction order.
//! Dropping a cache disposes it (Dart needs an explicit `dispose()`).

use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

type EvictHook = Arc<dyn Fn(&str) + Send + Sync>;

/// One entry plus the recency stamp its budget ranks it by.
struct Entry {
    value: Box<dyn Any + Send>,
    bytes: usize,
    tick: u64,
}

/// One member cache's entries. Iteration order by tick is the cache's own
/// recency order (Dart's LinkedHashMap re-insertion on read).
struct Member {
    entries: HashMap<String, Entry>,
    by_tick: BTreeMap<u64, String>,
    bytes: usize,
    /// The group of caches holding parts of one result with this one.
    partners: u64,
    on_evict: Option<EvictHook>,
    disposed: bool,
}

impl Member {
    /// This cache's oldest entry not under [keep] (one at most is, so it is
    /// the first or the second), or `None`.
    fn oldest_except(&self, keep: Option<&str>) -> Option<(u64, &str)> {
        self.by_tick
            .iter()
            .find(|(_, k)| Some(k.as_str()) != keep)
            .map(|(t, k)| (*t, k.as_str()))
    }

    fn take(&mut self, key: &str) -> Option<Entry> {
        let e = self.entries.remove(key)?;
        self.by_tick.remove(&e.tick);
        self.bytes -= e.bytes;
        Some(e)
    }
}

struct BudgetState {
    max_bytes: usize,
    bytes: usize,
    clock: u64,
    next_id: u64,
    /// Members in joining order (the order Dart's eviction scan walks).
    order: Vec<u64>,
    members: HashMap<u64, Member>,
}

impl BudgetState {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Evict globally-oldest entries until the total fits, never one under
    /// [keep] — the key just inserted — in a cache of the inserter's
    /// [partners] group, so an oversized result is still served whole.
    /// Returns the evicted keys with their caches' hooks.
    fn trim(&mut self, keep: &str, partners: u64) -> Vec<(Option<EvictHook>, String)> {
        let mut evicted = Vec::new();
        while self.bytes > self.max_bytes {
            let mut victim: Option<(u64, u64)> = None; // (member, tick)
            for id in &self.order {
                let m = &self.members[id];
                let kept_in = (m.partners == partners).then_some(keep);
                let Some((tick, _)) = m.oldest_except(kept_in) else {
                    continue;
                };
                if victim.is_none_or(|(_, oldest)| tick < oldest) {
                    victim = Some((*id, tick));
                }
            }
            // Nothing left to give: only the entries under [keep] remain.
            let Some((id, tick)) = victim else {
                break;
            };
            let Some(m) = self.members.get_mut(&id) else {
                break;
            };
            let Some(key) = m.by_tick.get(&tick).cloned() else {
                break;
            };
            if let Some(e) = m.take(&key) {
                self.bytes -= e.bytes;
            }
            evicted.push((m.on_evict.clone(), key));
        }
        evicted
    }
}

fn run_hooks(evicted: Vec<(Option<EvictHook>, String)>) {
    for (hook, key) in evicted {
        if let Some(hook) = hook {
            hook(&key);
        }
    }
}

/// A byte budget several [`LruBytesCache`]es charge against together.
///
/// It owns the ceiling, the running total, and the single recency clock that
/// makes "least recently used" comparable ACROSS its members — without one,
/// each cache could only rank its own entries and the budget would have to
/// pick a victim cache arbitrarily. Cloning shares the same budget.
#[derive(Clone)]
pub struct LruBytesBudget {
    state: Arc<Mutex<BudgetState>>,
}

impl LruBytesBudget {
    /// A budget of [max_bytes] (must be > 0).
    pub fn new(max_bytes: usize) -> LruBytesBudget {
        assert!(max_bytes > 0, "a budget must be > 0");
        LruBytesBudget {
            state: Arc::new(Mutex::new(BudgetState {
                max_bytes,
                bytes: 0,
                clock: 0,
                next_id: 0,
                order: Vec::new(),
                members: HashMap::new(),
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BudgetState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn max_bytes(&self) -> usize {
        self.lock().max_bytes
    }

    /// Current tracked payload bytes across every member (tests/diagnostics).
    pub fn bytes(&self) -> usize {
        self.lock().bytes
    }

    /// Members currently charging against this budget (tests/diagnostics).
    pub fn member_count(&self) -> usize {
        self.lock().order.len()
    }

    fn same(&self, other: &LruBytesBudget) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    fn join(&self) -> u64 {
        let mut s = self.lock();
        let id = s.next_id;
        s.next_id += 1;
        s.order.push(id);
        s.members.insert(
            id,
            Member {
                entries: HashMap::new(),
                by_tick: BTreeMap::new(),
                bytes: 0,
                // A fresh group of its own until partnered.
                partners: id,
                on_evict: None,
                disposed: false,
            },
        );
        id
    }
}

/// A byte-budgeted LRU cache keyed by string (see the module doc).
pub struct LruBytesCache<V> {
    budget: LruBytesBudget,
    id: u64,
    byte_of: Box<dyn Fn(&V) -> usize + Send + Sync>,
    _values: PhantomData<fn() -> V>,
}

impl<V: Clone + Send + 'static> LruBytesCache<V> {
    /// A cache with a budget of its own.
    pub fn new(max_bytes: usize, byte_of: impl Fn(&V) -> usize + Send + Sync + 'static) -> Self {
        LruBytesCache::shared(&LruBytesBudget::new(max_bytes), byte_of)
    }

    /// A cache charging against a budget it SHARES with its siblings. Drop it
    /// (or call [`LruBytesCache::dispose`]) when the owner goes away: a shared
    /// budget outlives any one screen, so a cache that never leaves keeps that
    /// screen's whole payload resident for the life of the process.
    pub fn shared(
        budget: &LruBytesBudget,
        byte_of: impl Fn(&V) -> usize + Send + Sync + 'static,
    ) -> Self {
        LruBytesCache {
            budget: budget.clone(),
            id: budget.join(),
            byte_of: Box::new(byte_of),
            _values: PhantomData,
        }
    }

    /// Called with the key of every entry this cache drops — eviction,
    /// [`LruBytesCache::remove`] and [`LruBytesCache::clear`] alike. Anything
    /// memoized FROM an entry is keyed off the same key and has no owner once
    /// the entry is gone, so this is where those derivations are dropped.
    pub fn set_on_evict(&self, hook: Option<impl Fn(&str) + Send + Sync + 'static>) {
        let hook = hook.map(|h| Arc::new(h) as EvictHook);
        if let Some(m) = self.budget.lock().members.get_mut(&self.id) {
            m.on_evict = hook;
        }
    }

    /// The budget this cache charges against, so an owner can report the
    /// shared total (tests/diagnostics).
    pub fn budget(&self) -> &LruBytesBudget {
        &self.budget
    }

    /// Holds parts of one result with [other], on the same budget: an insert
    /// under a key never evicts what a partner holds under that key. Only
    /// partners: another screen's entry for the same file is evicted in its
    /// turn like any other. Joins [other]'s group: partner every cache of a
    /// group with the same one.
    pub fn partner_with<W>(&self, other: &LruBytesCache<W>) {
        assert!(self.budget.same(&other.budget), "partners share a budget");
        let mut s = self.budget.lock();
        let group = s.members.get(&other.id).map(|m| m.partners);
        if let (Some(group), Some(m)) = (group, s.members.get_mut(&self.id)) {
            m.partners = group;
        }
    }

    fn with_member<T>(&self, f: impl FnOnce(&Member) -> T, absent: T) -> T {
        match self.budget.lock().members.get(&self.id) {
            Some(m) => f(m),
            None => absent,
        }
    }

    /// Current tracked payload bytes in THIS cache (for tests/diagnostics);
    /// see [`LruBytesCache::budget`] for the shared total.
    pub fn bytes(&self) -> usize {
        self.with_member(|m| m.bytes, 0)
    }

    pub fn len(&self) -> usize {
        self.with_member(|m| m.entries.len(), 0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.with_member(|m| m.entries.contains_key(key), false)
    }

    /// The keys held, least recently used first. Unlike a read, listing them
    /// does not refresh anyone's recency.
    pub fn keys(&self) -> Vec<String> {
        self.with_member(|m| m.by_tick.values().cloned().collect(), Vec::new())
    }

    /// Dart's `cache[key]`: the value, refreshing its recency.
    pub fn get(&self, key: &str) -> Option<V> {
        let mut s = self.budget.lock();
        if !s.members.get(&self.id)?.entries.contains_key(key) {
            return None;
        }
        let tick = s.tick();
        let m = s.members.get_mut(&self.id)?;
        let e = m.entries.get_mut(key)?;
        m.by_tick.remove(&e.tick);
        e.tick = tick;
        m.by_tick.insert(tick, key.to_string());
        e.value.downcast_ref::<V>().cloned()
    }

    /// Dart's `cache[key] = value`: replaces any entry under [key], then
    /// evicts globally-oldest entries until the budget fits (never this key,
    /// nor what this cache's partners hold under it).
    pub fn insert(&self, key: impl Into<String>, value: V) {
        let key = key.into();
        let bytes = (self.byte_of)(&value);
        let evicted = {
            let mut s = self.budget.lock();
            // A disposed cache has LEFT the shared budget, so it can no longer be
            // chosen as an eviction victim: charging it here would add bytes the
            // budget can never give back (an owner whose async analysis lands after
            // its screen was closed did exactly that, permanently shrinking what the
            // live screens are allowed to hold). Nothing can read the entry back
            // either, so dropping the write loses nothing.
            if s.members.get(&self.id).is_none_or(|m| m.disposed) {
                return;
            }
            let tick = s.tick();
            let Some(m) = s.members.get_mut(&self.id) else {
                return;
            };
            let old = m.take(&key).map_or(0, |e| e.bytes);
            m.entries.insert(
                key.clone(),
                Entry {
                    value: Box::new(value),
                    bytes,
                    tick,
                },
            );
            m.by_tick.insert(tick, key.clone());
            m.bytes += bytes;
            let partners = m.partners;
            s.bytes = s.bytes - old + bytes;
            s.trim(&key, partners)
        };
        run_hooks(evicted);
    }

    pub fn remove(&self, key: &str) -> Option<V> {
        let (value, hook) = {
            let mut s = self.budget.lock();
            let m = s.members.get_mut(&self.id)?;
            let e = m.take(key)?;
            let hook = m.on_evict.clone();
            s.bytes -= e.bytes;
            (e.value.downcast::<V>().ok().map(|v| *v), hook)
        };
        if let Some(hook) = hook {
            hook(key);
        }
        value
    }

    pub fn clear(&self) {
        let (keys, hook) = {
            let mut s = self.budget.lock();
            let Some(m) = s.members.get_mut(&self.id) else {
                return;
            };
            let keys: Vec<String> = m.by_tick.values().cloned().collect();
            let freed = m.bytes;
            m.entries.clear();
            m.by_tick.clear();
            m.bytes = 0;
            let hook = m.on_evict.clone();
            s.bytes -= freed;
            (keys, hook)
        };
        if let Some(hook) = hook {
            for k in &keys {
                hook(k);
            }
        }
    }

    /// Whether [`LruBytesCache::dispose`] has run. A disposed cache stays
    /// usable as an empty one — reads miss, writes are dropped — so an
    /// owner's in-flight work can land after teardown without corrupting the
    /// shared budget.
    pub fn is_disposed(&self) -> bool {
        self.with_member(|m| m.disposed, true)
    }

    /// Drops everything and leaves the shared budget. Idempotent.
    pub fn dispose(&self) {
        let mut s = self.budget.lock();
        // Leave FIRST: the budget discounts what this cache still holds, so the
        // total stays honest whether or not the map was already emptied.
        let before = s.order.len();
        s.order.retain(|id| *id != self.id);
        let left = s.order.len() != before;
        let Some(m) = s.members.get_mut(&self.id) else {
            return;
        };
        let held = m.bytes;
        m.entries.clear();
        m.by_tick.clear();
        m.bytes = 0;
        m.disposed = true;
        if left {
            s.bytes -= held;
        }
    }
}

impl<V> Drop for LruBytesCache<V> {
    fn drop(&mut self) {
        let mut s = self.budget.lock();
        let before = s.order.len();
        s.order.retain(|id| *id != self.id);
        let left = s.order.len() != before;
        if let Some(m) = s.members.remove(&self.id) {
            if left {
                s.bytes -= m.bytes;
            }
        }
    }
}

/// The app-wide analysis budget every heavy result cache charges against —
/// the practice screen's per-clip spectrograms and metrics, its lane
/// aggregates and EQ display spectrograms, and the technique screen's
/// reference analyses.
///
/// ONE number for what analysis can hold, rather than a per-cache ceiling
/// each: the old independent budgets summed to more than half a gigabyte that
/// no single screen could ever use, while nothing stopped two screens' worth
/// of results from being resident at once. 384 MB comfortably holds a working
/// practice (a lane aggregate plus its clips, ~10 min at 214 bins is ~44 MB
/// of spectrogram each) and lets whatever the user left be the natural
/// eviction victim for what they are on.
pub fn analysis_cache_budget() -> &'static LruBytesBudget {
    static BUDGET: OnceLock<LruBytesBudget> = OnceLock::new();
    BUDGET.get_or_init(|| LruBytesBudget::new(384 * 1000 * 1000))
}

#[cfg(test)]
mod tests {
    //! Port of the LruBytesCache group of `client/test/analysis_gate_test.dart`.
    use std::sync::Mutex;

    use super::*;

    fn cache(max: usize) -> LruBytesCache<Vec<u8>> {
        LruBytesCache::new(max, |v: &Vec<u8>| v.len())
    }

    fn shared(budget: &LruBytesBudget) -> LruBytesCache<Vec<u8>> {
        LruBytesCache::shared(budget, |v: &Vec<u8>| v.len())
    }

    #[test]
    fn evicts_least_recently_used_past_the_byte_budget() {
        let cache = cache(10);
        cache.insert("a", vec![0; 4]);
        cache.insert("b", vec![0; 4]);
        // Touch 'a' so 'b' is the eviction victim.
        assert!(cache.get("a").is_some());
        cache.insert("c", vec![0; 4]); // 12 > 10 -> evict LRU ('b')
        assert!(cache.contains_key("a"));
        assert!(!cache.contains_key("b"));
        assert!(cache.contains_key("c"));
        assert_eq!(cache.bytes(), 8);
        assert_eq!(cache.keys(), ["a", "c"]);
    }

    #[test]
    fn replacing_a_key_retallies_bytes_instead_of_double_counting() {
        let cache = cache(10);
        cache.insert("a", vec![0; 6]);
        cache.insert("a", vec![0; 2]);
        assert_eq!(cache.bytes(), 2);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn keeps_the_newest_entry_even_when_it_alone_exceeds_the_budget() {
        let cache = cache(10);
        cache.insert("big", vec![0; 50]);
        // Better served once than recomputed forever.
        assert!(cache.contains_key("big"));
        cache.insert("next", vec![0; 4]);
        assert!(!cache.contains_key("big"), "evicted when superseded");
        assert!(cache.contains_key("next"));
    }

    #[test]
    fn an_insert_never_pushes_out_what_its_partners_hold_under_the_same_key_and_only_its_partners()
    {
        // A clip's spectrogram and metrics are one analysis held in two caches.
        // With the pair over the budget, each half's insert dropped the other,
        // and the view showing it computed it again, dropping the first.
        let budget = LruBytesBudget::new(10);
        let spectrograms = shared(&budget);
        let metrics = shared(&budget);
        metrics.partner_with(&spectrograms);
        let eq = shared(&budget);
        eq.insert("take", vec![0; 2]); // the take's EQ display, unread since
        metrics.insert("other", vec![0; 2]);
        spectrograms.insert("take", vec![0; 8]);
        metrics.insert("take", vec![0; 4]);
        assert!(spectrograms.contains_key("take"));
        assert!(metrics.contains_key("take"));
        assert!(!eq.contains_key("take"), "no partner: oldest");
        assert!(!metrics.contains_key("other"), "room made");
        assert_eq!(budget.bytes(), 12, "the pair alone is over the budget");

        // Superseded, its halves go like any other entries, oldest first.
        spectrograms.insert("next", vec![0; 4]);
        assert!(!spectrograms.contains_key("take"));
        assert!(metrics.contains_key("take"));
        assert_eq!(budget.bytes(), 8);
    }

    #[test]
    fn remove_and_clear_keep_the_byte_tally_honest() {
        let cache = cache(100);
        cache.insert("a", vec![0; 6]);
        cache.insert("b", vec![0; 4]);
        cache.remove("a");
        assert_eq!(cache.bytes(), 4);
        cache.clear();
        assert_eq!(cache.bytes(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn a_write_after_dispose_cannot_charge_the_shared_budget() {
        // The owner of a shared cache disposes it when its screen goes away,
        // but its in-flight analysis can land afterwards. A disposed cache is
        // no longer a member of the budget, so anything charged then could
        // never be evicted — it would permanently shrink what the live screens
        // may hold, one abandoned analysis at a time.
        let budget = LruBytesBudget::new(100);
        let live = shared(&budget);
        let leaving = shared(&budget);
        live.insert("kept", vec![0; 10]);
        leaving.insert("a", vec![0; 20]);
        assert_eq!(budget.bytes(), 30);

        leaving.dispose();
        assert_eq!(budget.bytes(), 10, "the disposed cache gave its bytes back");

        leaving.insert("late-analysis", vec![0; 40]);
        assert_eq!(budget.bytes(), 10, "phantom bytes nothing could evict");
        assert!(leaving.is_disposed());
        assert!(leaving.get("late-analysis").is_none());
        // The live cache still owns the whole budget it started with.
        live.insert("more", vec![0; 80]);
        assert!(live.contains_key("more"));
        assert!(live.contains_key("kept"));
        assert_eq!(budget.bytes(), 90);
    }

    #[test]
    fn eviction_remove_and_clear_report_their_keys_and_drop_leaves_the_budget() {
        let budget = LruBytesBudget::new(10);
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let cache = shared(&budget);
        let log = Arc::clone(&seen);
        cache.set_on_evict(Some(move |k: &str| log.lock().unwrap().push(k.to_string())));
        cache.insert("a", vec![0; 6]);
        cache.insert("b", vec![0; 6]); // evicts a
        cache.insert("c", vec![0; 1]);
        cache.remove("c");
        cache.insert("d", vec![0; 1]);
        cache.clear();
        assert_eq!(*seen.lock().unwrap(), ["a", "c", "b", "d"]);
        cache.insert("e", vec![0; 3]);
        assert_eq!(budget.member_count(), 1);
        drop(cache);
        assert_eq!(budget.member_count(), 0);
        assert_eq!(budget.bytes(), 0);
    }
}
