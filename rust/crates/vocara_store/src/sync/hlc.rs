//! Hybrid Logical Clock stamps and the device clock (port of
//! `lib/data/sync/hlc.dart`).

use std::cmp::Ordering;
use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use crate::dart;
use crate::error::{Error, Result};

/// A Hybrid Logical Clock timestamp: a physical wall-clock value (ms) plus a
/// logical [`Hlc::counter`] that orders events within the same millisecond,
/// plus a stable per-install [`Hlc::node`] id as the final tiebreak.
///
/// HLCs are TOTALLY ordered, which is what makes entity-level
/// Last-Writer-Wins deterministic: two devices that have seen the same set of
/// writes resolve any row to the identical winner, so they always converge —
/// no split brain.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hlc {
    /// The HLC's physical component (epoch milliseconds).
    pub wall_ms: i64,
    /// Logical counter, bumped when two events share a [`Hlc::wall_ms`].
    pub counter: i64,
    /// Stable id of the install that produced this stamp (a ULID minted
    /// once).
    pub node: String,
}

impl Hlc {
    /// A stamp from its three components.
    pub fn new(wall_ms: i64, counter: i64, node: impl Into<String>) -> Self {
        Hlc {
            wall_ms,
            counter,
            node: node.into(),
        }
    }

    /// A fixed-width, lexicographically comparable encoding whose string
    /// order is exactly the tuple order — stored next to each row as the
    /// hot-path LWW key so a conflict is a single `packed < packed`
    /// comparison. The counter field is fixed at 6 digits: [`HlcClock`]
    /// never emits a counter above [`HlcClock::MAX_COUNTER`] (it spills into
    /// the next millisecond instead), so the width — and therefore the
    /// lexicographic ordering — can never break.
    ///
    /// Byte-identical to Dart's `'${wallMs.toString().padLeft(13, '0')}-'
    /// '${counter.toString().padLeft(6, '0')}-$node'`, a negative number's
    /// sign included.
    pub fn packed(&self) -> String {
        format!(
            "{}-{}-{}",
            dart::pad_left_zeros(&self.wall_ms.to_string(), 13),
            dart::pad_left_zeros(&self.counter.to_string(), 6),
            self.node
        )
    }

    /// Parses a [`Hlc::packed`] encoding back into an [`Hlc`]. The node is a
    /// ULID (no dashes), so only the first two dashes are separators. The two
    /// numbers parse as Dart's `int.parse` does.
    pub fn parse(packed: &str) -> Result<Hlc> {
        let not_packed = || Error::Format(format!("Not a packed HLC: \"{packed}\""));
        let i1 = packed.find('-').ok_or_else(not_packed)?;
        let i2 = packed[i1 + 1..]
            .find('-')
            .map(|i| i + i1 + 1)
            .ok_or_else(not_packed)?;
        let number = |s: &str| {
            dart::parse_int(s)
                .ok_or_else(|| Error::Format(format!("Invalid radix-10 number: \"{s}\"")))
        };
        Ok(Hlc {
            wall_ms: number(&packed[..i1])?,
            counter: number(&packed[i1 + 1..i2])?,
            node: packed[i2 + 1..].to_string(),
        })
    }
}

impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> Ordering {
        self.wall_ms
            .cmp(&other.wall_ms)
            .then(self.counter.cmp(&other.counter))
            .then_with(|| dart::compare_strings(&self.node, &other.node))
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Hlc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hlc({})", self.packed())
    }
}

/// An inbound (remote) HLC implausibly far in the future, beyond the clock's
/// drift bound. The offending record is quarantined rather than merged, so a
/// single device with a bad clock cannot ratchet the whole fleet's clocks
/// forward (which would let ancient future-stamped data win LWW for years).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlcDriftError {
    /// The refused stamp.
    pub remote: Hlc,
    /// The reference "now" it was judged against.
    pub now_ms: i64,
    /// The drift bound it exceeded.
    pub max_drift_ms: i64,
}

impl fmt::Display for HlcDriftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "HlcDriftException(remote={} is {}ms ahead of now, > maxDrift {}ms)",
            self.remote.packed(),
            self.remote.wall_ms.wrapping_sub(self.now_ms),
            self.max_drift_ms
        )
    }
}

impl std::error::Error for HlcDriftError {}

/// The wall clock a [`HlcClock`] reads (epoch ms); injectable for tests.
pub type NowFn = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The clock's mutable state, behind one mutex so every read-modify-write is
/// atomic across threads (Dart relied on the single isolate for that).
#[derive(Debug, Clone)]
struct ClockState {
    last: Hlc,
    max_wall_ever_seen: i64,
}

/// A device's Hybrid Logical Clock. The SINGLE producer of HLC stamps for
/// local writes ([`HlcClock::tick`]) and the only place that advances when a
/// remote stamp is seen ([`HlcClock::receive`], or
/// [`HlcClock::merge_verified`] for stamps already gated by
/// [`HlcClock::assert_plausible`]). Inject `now` in tests for determinism.
///
/// Shared by reference (`Arc<HlcClock>`): every method takes `&self`.
pub struct HlcClock {
    node: String,
    max_drift_ms: i64,
    now: NowFn,
    state: Mutex<ClockState>,
}

impl fmt::Debug for HlcClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HlcClock")
            .field("node", &self.node)
            .field("max_drift_ms", &self.max_drift_ms)
            .field("state", &*self.state.lock())
            .finish()
    }
}

fn system_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The node alphabet [`HlcClock::derived`] advances in, in code-unit order.
const NODE_ALPHABET: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

impl HlcClock {
    /// 5 minutes. Generous enough to absorb normal cross-device clock skew,
    /// tight enough that a grossly-wrong (e.g. far-future) clock is rejected.
    /// A long OFFLINE gap is unaffected — the wall clock advances normally
    /// while offline, so a clock that was correct months ago arrives with a
    /// PAST wall, never a future one.
    pub const DEFAULT_MAX_DRIFT_MS: i64 = 5 * 60 * 1000;

    /// The highest counter the fixed 6-digit [`Hlc::packed`] field can
    /// carry. [`HlcClock::tick`] and [`HlcClock::receive`] spill into the
    /// next millisecond instead of exceeding it — a 7-digit counter would
    /// sort lexicographically BELOW 6-digit ones, making the string MAX pick
    /// the wrong winner under LWW.
    pub const MAX_COUNTER: i64 = 999_999;

    /// One node segment: a device id is a 26-character ULID, and every
    /// derivation appends exactly one more.
    pub const NODE_SEGMENT_LENGTH: usize = 26;

    /// Longest derivation CHAIN a node may carry: the minting device plus 13
    /// derived writes. Far beyond any real chain — the integrity fixpoint
    /// caps at 8 passes per run — and bounded so a hostile stamp cannot bloat
    /// every packed key it touches.
    pub const MAX_NODE_SEGMENTS: usize = 14;

    /// Longest node the wire accepts (see `SyncRow::check_hlc_consistency`)
    /// and the ceiling [`HlcClock::derived`] extends under:
    /// [`HlcClock::MAX_NODE_SEGMENTS`] segments joined by '!'.
    pub const MAX_NODE_LENGTH: usize =
        Self::NODE_SEGMENT_LENGTH * Self::MAX_NODE_SEGMENTS + (Self::MAX_NODE_SEGMENTS - 1);

    /// A clock for [node] reading the system wall clock, starting at
    /// `Hlc(0, 0, node)` with the default drift bound.
    pub fn new(node: impl Into<String>) -> Self {
        let node = node.into();
        let last = Hlc::new(0, 0, node.clone());
        HlcClock {
            max_drift_ms: Self::DEFAULT_MAX_DRIFT_MS,
            now: Arc::new(system_now_ms),
            state: Mutex::new(ClockState {
                max_wall_ever_seen: last.wall_ms,
                last,
            }),
            node,
        }
    }

    /// The same clock reading [now] instead of the system wall clock.
    pub fn with_now(mut self, now: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.now = Arc::new(now);
        self
    }

    /// The same clock with a different maximum tolerated forward drift.
    pub fn with_max_drift_ms(mut self, max_drift_ms: i64) -> Self {
        self.max_drift_ms = max_drift_ms;
        self
    }

    /// The same clock resuming from [initial] (a persisted stamp): its last
    /// stamp, and the highest wall it has seen.
    pub fn with_initial(self, initial: Hlc) -> Self {
        {
            let mut s = self.state.lock();
            s.max_wall_ever_seen = initial.wall_ms;
            s.last = initial;
        }
        self
    }

    /// Stable per-install node id (final LWW tiebreak).
    pub fn node(&self) -> &str {
        &self.node
    }

    /// Maximum tolerated forward drift of a remote (or the local) clock.
    pub fn max_drift_ms(&self) -> i64 {
        self.max_drift_ms
    }

    /// The most recent stamp this clock produced or merged.
    pub fn last(&self) -> Hlc {
        self.state.lock().last.clone()
    }

    /// The highest wall ever observed (kept for forensics / drift
    /// diagnostics).
    pub fn max_wall_ever_seen(&self) -> i64 {
        self.state.lock().max_wall_ever_seen
    }

    /// The stamp a DERIVED write (integrity repair, slot-resolution
    /// tombstone, FK-sweep collision restore) mints from the row state [base]
    /// it derives from: the SAME (wall, counter), with the node extended by
    /// `'!' + device_node`. String order makes a prefix extension sort
    /// strictly AFTER the prefix, so the derived stamp out-ranks [base] (it
    /// replicates over every peer still holding base) while sorting strictly
    /// BELOW every stamp that out-ranks base — including a held-wall author
    /// edit at `(base.wall, base.counter + 1)` AND a node-space tie winner at
    /// `(base.wall, base.counter, biggerNode)`.
    ///
    /// Deriving in NODE space, never counter space, is the whole point — it
    /// closes both halves of the derived-stamp lost update:
    ///
    ///  * counter escalation: two chained repairs used to stamp base+1 then
    ///    base+2, and base+2-derived beat a base+1 REAL edit, silently
    ///    reverting it fleet-wide. Here a repair-of-a-repair just extends the
    ///    node again — `(w, c, A!D!D)` — and still loses to every real edit
    ///    above the ORIGINAL base. Bookkeeping can never out-rank user intent,
    ///    no matter how many times it re-derives.
    ///  * the restore collision: a parked update that won its slot by NODE
    ///    only — `(w, c, B)` over prior `(w, c, A)` — used to be beaten by the
    ///    restore's `(w, c + 1, …)` tombstone, so its quarantine replay lost
    ///    LWW forever. `(w, c, A!D)` sorts between A and B: the tombstone
    ///    replicates over the prior state AND the replay still lands.
    ///
    /// The device suffix keeps derived writes from DIFFERENT devices distinct
    /// (two devices deriving different content from the same base must not
    /// mint one stamp), and `'!'` (below the Crockford node alphabet) makes
    /// derived stamps recognizable and reserved on the wire.
    ///
    /// At the chain's ceiling ([`HlcClock::MAX_NODE_SEGMENTS`] segments) the
    /// derivation stays in NODE space rather than bumping the counter. A
    /// counter bump leaves node space entirely, so the derived stamp
    /// outranks any same-millisecond honest write — exactly the tie the
    /// extension scheme exists to avoid — and the wire's node grammar makes
    /// the ceiling unreachable without a chain of real derivations anyway.
    /// Advancing the node keeps the stamp strictly above its base and
    /// strictly below the next real edit, which is the whole contract.
    ///
    /// The one exception: the wire grammar also admits a ceiling node whose
    /// characters sit at the TOP of the alphabet (minted ids are Crockford,
    /// so only a hostile stamp reaches it) — at the absolute top of node
    /// space no stamp exists between the base and (wall, counter + 1), so
    /// the derivation must leave node space after all. It does so MINIMALLY,
    /// one counter up (spilling to the next millisecond at
    /// [`HlcClock::MAX_COUNTER`], like [`HlcClock::tick`]) under
    /// [device_node]. That stamp can win a node tiebreak against a real edit
    /// at the very same (wall, counter + 1) — accepted: the input requires a
    /// hostile ceiling-length stamp, and refusing to derive would instead
    /// permanently fail every caller (integrity repair, slot resolution) for
    /// the row carrying it. Total by design: never fails.
    pub fn derived(base: &Hlc, device_node: &str) -> Hlc {
        let extended = format!("{}!{}", base.node, device_node);
        if dart::string_length(&extended) <= Self::MAX_NODE_LENGTH {
            return Hlc::new(base.wall_ms, base.counter, extended);
        }
        if let Some(advanced) = advance_node(&base.node) {
            return Hlc::new(base.wall_ms, base.counter, advanced);
        }
        // Node space above the base is empty: the minimal strictly-greater
        // stamp lives one counter up. Same spill rule as tick/advance at the
        // cap.
        let mut wall = base.wall_ms;
        let mut counter = base.counter.wrapping_add(1);
        if counter > Self::MAX_COUNTER {
            wall = wall.wrapping_add(1);
            counter = 0;
        }
        Hlc::new(wall, counter, device_node)
    }

    /// Stamps a LOCAL write. Standard, strictly-monotonic HLC advance: the
    /// wall never moves backward, so every stamp is greater than the previous
    /// one.
    ///
    /// We deliberately do NOT reset the wall back to real time on forward
    /// drift. That reset emitted an `Hlc(now, 0)` that compared SMALLER than
    /// the device's own earlier (skewed-forward) stamps, so — under
    /// entity-level LWW — the newest local edit silently lost to an older
    /// one, and a just-undeleted row could stay tombstoned. Monotonicity is
    /// the stronger invariant. If a corrected clock leaves us ahead of real
    /// time, the wall simply holds (counter keeps incrementing) until real
    /// time catches up: local edits still win over our own prior ones, and
    /// any stamp too far ahead of a peer is quarantined by that peer's
    /// [`HlcClock::receive`] drift gate — a visible, self-healing degradation
    /// rather than silent local data loss.
    pub fn tick(&self) -> Hlc {
        let now = (self.now)();
        let mut s = self.state.lock();
        let mut wall = if now > s.last.wall_ms {
            now
        } else {
            s.last.wall_ms
        };
        let mut counter = if wall == s.last.wall_ms {
            s.last.counter.wrapping_add(1)
        } else {
            0
        };
        if counter > Self::MAX_COUNTER {
            // The 6-digit packed counter is exhausted within this
            // millisecond: spill into the next one. Same hold-the-wall
            // degradation as a forward clock excursion — still strictly
            // monotonic, and real time catches up.
            wall = wall.wrapping_add(1);
            counter = 0;
        }
        s.last = Hlc::new(wall, counter, self.node.clone());
        track(&mut s, wall);
        s.last.clone()
    }

    /// Judges [remote] against the drift rule WITHOUT advancing the clock:
    /// fails with [`HlcDriftError`] exactly when [`HlcClock::receive`]
    /// would, and mutates nothing. This exists so the sync applier can GATE a
    /// record before its savepoint while deferring the state-changing
    /// receive until the record has settled AND its batch's outer
    /// transaction has committed — before the split, a record that failed
    /// (or was quarantined) inside its savepoint had already advanced the
    /// in-memory clock, so a rolled-back record still left a side effect
    /// behind; the same holds for a whole batch whose outer commit fails.
    /// Both methods share one drift check, so the two rules can never
    /// diverge.
    pub fn assert_plausible(&self, remote: &Hlc) -> std::result::Result<(), HlcDriftError> {
        let now = (self.now)();
        let s = self.state.lock();
        self.check_drift(&s, remote, now)
    }

    /// Merges a [remote] stamp seen on an inbound record, advancing the
    /// local clock per the canonical HLC receive rule so subsequent local
    /// stamps stay causally after anything we have observed. Returns the new
    /// local stamp.
    ///
    /// Refuses a remote whose wall is more than the drift bound in the
    /// future ([`HlcDriftError`]) so one device's bad clock can't contaminate
    /// ours. A remote in the past is fine (it simply doesn't advance us). The
    /// check-only form of the same rule is [`HlcClock::assert_plausible`];
    /// the advance WITHOUT the drift check is [`HlcClock::merge_verified`].
    pub fn receive(&self, remote: &Hlc) -> std::result::Result<Hlc, HlcDriftError> {
        let now = (self.now)();
        let mut s = self.state.lock();
        self.check_drift(&s, remote, now)?;
        Ok(self.advance(&mut s, remote, now))
    }

    /// Merges a [remote] stamp that ALREADY passed
    /// [`HlcClock::assert_plausible`] earlier in the same batch: the exact
    /// advance [`HlcClock::receive`] performs, with NO drift re-check. Never
    /// call this on a stamp that has not been gated.
    ///
    /// This exists for the sync applier's deferred post-commit advance. Each
    /// record is gated with assert_plausible before its savepoint, but the
    /// state-changing merge runs only after the batch's outer transaction
    /// has committed — and between the two, the wall clock can roll
    /// BACKWARD. assert_plausible never raises max_wall_ever_seen (it is
    /// check-only by contract), so the drift reference
    /// `max(now, max_wall_ever_seen)` can SHRINK across that window, and
    /// re-running the check in receive would then fail for a stamp whose
    /// data and change token are already durable: the run reports failure
    /// for a committed page. Clamping the remote stamp instead was
    /// considered and rejected — it would corrupt causal order. Both methods
    /// share one advance, so the two merges can never diverge.
    pub fn merge_verified(&self, remote: &Hlc) -> Hlc {
        let now = (self.now)();
        let mut s = self.state.lock();
        self.advance(&mut s, remote, now)
    }

    /// [`HlcClock::merge_verified`] of [seen] only when it is at least this
    /// clock's last stamp, as one atomic step (Dart's `if (seen >=
    /// clock.last) clock.mergeVerified(seen)` ran on a single isolate).
    /// Returns whether it merged.
    pub fn merge_verified_if_not_behind(&self, seen: &Hlc) -> bool {
        let now = (self.now)();
        let mut s = self.state.lock();
        if *seen >= s.last {
            self.advance(&mut s, seen, now);
            true
        } else {
            false
        }
    }

    /// The shared advance rule behind receive and merge_verified: adopt the
    /// max wall of (now, ours, remote) and pick the counter that keeps the
    /// new stamp strictly after both our last stamp and the remote one.
    fn advance(&self, s: &mut ClockState, remote: &Hlc, now: i64) -> Hlc {
        let mut wall = now.max(s.last.wall_ms).max(remote.wall_ms);
        let mut counter = if wall == s.last.wall_ms && wall == remote.wall_ms {
            s.last.counter.max(remote.counter).wrapping_add(1)
        } else if wall == s.last.wall_ms {
            s.last.counter.wrapping_add(1)
        } else if wall == remote.wall_ms {
            remote.counter.wrapping_add(1)
        } else {
            0
        };
        if counter > Self::MAX_COUNTER {
            // Keep the packed encoding's fixed 6-digit counter: spill to the
            // next millisecond (still strictly after both last and remote).
            wall = wall.wrapping_add(1);
            counter = 0;
        }
        s.last = Hlc::new(wall, counter, self.node.clone());
        track(s, wall);
        s.last.clone()
    }

    /// The shared drift rule behind receive and assert_plausible. Judge
    /// "implausibly far in the future" against the best estimate of true
    /// time we have: the later of our local clock and the highest wall we
    /// have ever seen from an honest peer. Gating on the local clock alone
    /// meant that if OUR clock ran slow by more than the drift bound, every
    /// correctly-stamped remote looked far-future and ALL inbound sync was
    /// quarantined. Using max(now, seen) keeps the gate working through a
    /// slow local clock while still rejecting a genuinely far-future remote
    /// clock.
    fn check_drift(
        &self,
        s: &ClockState,
        remote: &Hlc,
        now: i64,
    ) -> std::result::Result<(), HlcDriftError> {
        let reference = if now > s.max_wall_ever_seen {
            now
        } else {
            s.max_wall_ever_seen
        };
        if remote.wall_ms.wrapping_sub(reference) > self.max_drift_ms {
            return Err(HlcDriftError {
                remote: remote.clone(),
                now_ms: reference,
                max_drift_ms: self.max_drift_ms,
            });
        }
        Ok(())
    }
}

fn track(s: &mut ClockState, wall: i64) {
    if wall > s.max_wall_ever_seen {
        s.max_wall_ever_seen = wall;
    }
}

/// [node] replaced by its successor among nodes of the same segment shape,
/// or `None` when every character is already at the top of the alphabet.
/// The whole chain is one odometer: a carry out of the final segment
/// advances the previous one ('!' separators stay fixed in place). The node
/// alphabet is listed in code-unit order AND the wire grammar admits nothing
/// outside it, so alphabet order is string order here; within the final
/// segment no legal node sits between a node and its successor, and once a
/// carry crosses a segment boundary only a strictly shorter chain that
/// prefixes the successor can — an ordering the hostile-only ceiling
/// accepts.
fn advance_node(node: &str) -> Option<String> {
    let alphabet: Vec<char> = NODE_ALPHABET.chars().collect();
    let mut chars: Vec<char> = node.chars().collect();
    for i in (0..chars.len()).rev() {
        if chars[i] == '!' {
            continue; // segment separator: fixed in place
        }
        // Outside the alphabet (never stored past the wire gate, but
        // derived() is total): un-advanceable in place, keep carrying left.
        let Some(pos) = alphabet.iter().position(|c| *c == chars[i]) else {
            continue;
        };
        let next = pos + 1;
        if next < alphabet.len() {
            chars[i] = alphabet[next];
            return Some(chars.into_iter().collect());
        }
        chars[i] = alphabet[0]; // carry one character left
    }
    None // every position exhausted: node space above is empty
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering as AtomicOrdering};

    fn h(wall: i64, counter: i64, node: &str) -> Hlc {
        Hlc::new(wall, counter, node)
    }

    /// A settable wall clock, like the Dart tests' `var now`.
    fn settable(start: i64) -> (Arc<AtomicI64>, impl Fn() -> i64 + Send + Sync + 'static) {
        let now = Arc::new(AtomicI64::new(start));
        let read = now.clone();
        (now, move || read.load(AtomicOrdering::SeqCst))
    }

    fn fixed(now: i64) -> impl Fn() -> i64 + Send + Sync + 'static {
        move || now
    }

    // --- Hlc packing + ordering ---

    #[test]
    fn packed_is_lexicographically_ordered_like_compare_to() {
        let a = h(1000, 0, "AAAA");
        let b = h(1000, 1, "AAAA");
        let c = h(1001, 0, "AAAA");
        let d = h(1000, 0, "BBBB");
        assert!(a < b);
        assert!(b < c);
        assert!(a < d); // node breaks the tie
        assert!(a.packed() < b.packed());
        assert!(b.packed() < c.packed());
        assert!(a.packed() < d.packed());
    }

    #[test]
    fn packed_round_trips_through_parse() {
        let hlc = h(1717000000000, 42, "01J9Z3KQ8V5N7P2R4T6W8XABCD");
        let back = Hlc::parse(&hlc.packed()).unwrap();
        assert_eq!(back, hlc);
        assert_eq!(back.wall_ms, hlc.wall_ms);
        assert_eq!(back.counter, 42);
        assert_eq!(back.node, hlc.node);
    }

    #[test]
    fn packed_is_fixed_width_so_string_sort_never_breaks_on_magnitude() {
        let small = h(9, 0, "N").packed();
        let big = h(1000000000000, 0, "N").packed();
        assert!(small < big);
    }

    #[test]
    fn packed_encoding_is_byte_identical_to_dart() {
        // Literal values from the Dart suites (quarantine_store_test,
        // app_database_test): the cross-language compatibility check.
        assert_eq!(h(1000, 0, "N").packed(), "0000000001000-000000-N");
        assert_eq!(h(2000, 0, "N").packed(), "0000000002000-000000-N");
        assert_eq!(h(500, 0, "N").packed(), "0000000000500-000000-N");
        assert_eq!(
            h(1000, 0, "NODE0000000000000000000000").packed(),
            "0000000001000-000000-NODE0000000000000000000000"
        );
        assert_eq!(h(1717000000000, 42, "X").packed(), "1717000000000-000042-X");
        // Dart pads the text of a negative number, sign included.
        assert_eq!(h(-5, -1, "N").packed(), "00000000000-5-0000-1-N");
    }

    #[test]
    fn parse_refuses_what_is_not_packed() {
        assert!(matches!(Hlc::parse("REMOTE-PACKED"), Err(Error::Format(_))));
        assert!(matches!(Hlc::parse("x"), Err(Error::Format(_))));
        assert!(matches!(Hlc::parse("abc-000000-N"), Err(Error::Format(_))));
        assert!(matches!(Hlc::parse("-1-2"), Err(Error::Format(_))));
        // Only the first two dashes separate: the node keeps the rest.
        assert_eq!(Hlc::parse("1-2-A-B").unwrap(), h(1, 2, "A-B"));
    }

    // --- HlcClock.derived (node-space derivation) ---

    const DEVICE: &str = "DEVICE00000000000000000000";

    fn base() -> Hlc {
        h(1000, 5, &"A".repeat(26))
    }

    #[test]
    fn derived_out_ranks_its_base_yet_sorts_below_every_stamp_that_out_ranks_it() {
        let base = base();
        let d = HlcClock::derived(&base, DEVICE);
        assert!(d > base, "must replicate over the base state");
        assert_eq!(d.wall_ms, base.wall_ms);
        assert_eq!(d.counter, base.counter, "never escalates the counter");

        // A held-wall author edit one counter above the base.
        let held_wall = h(1000, 6, &"0".repeat(26));
        assert!(d < held_wall);
        // A node-space tie winner barely above the base — the parked-update
        // shape whose replay must still beat the derived tombstone.
        let node_winner = h(1000, 5, &format!("{}B", "A".repeat(25)));
        assert!(node_winner > base, "fixture sanity");
        assert!(d < node_winner);
        // Packed order agrees everywhere the tuple order does.
        assert!(d.packed() > base.packed());
        assert!(d.packed() < held_wall.packed());
        assert!(d.packed() < node_winner.packed());
    }

    #[test]
    fn a_chain_of_derivations_still_loses_to_every_real_edit_above_the_original_base() {
        let base = base();
        let d1 = HlcClock::derived(&base, DEVICE);
        let d2 = HlcClock::derived(&d1, DEVICE);
        let d3 = HlcClock::derived(&d2, DEVICE);
        assert!(d1 < d2);
        assert!(d2 < d3, "each re-derivation replicates");
        let held_wall = h(1000, 6, &"0".repeat(26));
        for d in [&d1, &d2, &d3] {
            assert!(
                *d < held_wall,
                "chained bookkeeping must never beat user intent"
            );
        }
    }

    #[test]
    fn derived_keeps_devices_distinct() {
        let a = HlcClock::derived(&base(), "DEVICEA0000000000000000000");
        let b = HlcClock::derived(&base(), "DEVICEB0000000000000000000");
        assert_ne!(a, b);
        assert_ne!(a.cmp(&b), Ordering::Equal);
    }

    #[test]
    fn derived_round_trips_through_packed_parse() {
        let d = HlcClock::derived(
            &HlcClock::derived(&base(), DEVICE),
            "01J9Z3KQ8V5N7P2R4T6W8XABCD",
        );
        assert_eq!(Hlc::parse(&d.packed()).unwrap(), d);
    }

    fn derive_to_ceiling(base: &Hlc) -> Hlc {
        let mut d = base.clone();
        while d.node.len() + DEVICE.len() < HlcClock::MAX_NODE_LENGTH {
            d = HlcClock::derived(&d, DEVICE);
            assert_eq!(d.counter, base.counter);
        }
        d
    }

    #[test]
    fn at_the_chain_ceiling_it_stays_in_node_space() {
        let base = base();
        let d = derive_to_ceiling(&base);
        let over = HlcClock::derived(&d, DEVICE);

        // A counter bump would leave node space entirely and outrank every
        // same-millisecond honest write.
        assert_eq!(over.counter, base.counter, "no counter bump");
        assert_eq!(over.wall_ms, base.wall_ms);
        assert!(over.node.len() <= HlcClock::MAX_NODE_LENGTH);
        // Still strictly above its base…
        assert_eq!(over.cmp(&d), Ordering::Greater);
        // …and still strictly below a real next edit at the same wall clock.
        let real_edit = h(base.wall_ms, base.counter + 1, DEVICE);
        assert_eq!(over.cmp(&real_edit), Ordering::Less);
    }

    #[test]
    fn a_further_derivation_at_the_ceiling_keeps_advancing() {
        let base = base();
        let d = derive_to_ceiling(&base);
        let first = HlcClock::derived(&d, DEVICE);
        let second = HlcClock::derived(&first, DEVICE);
        assert_eq!(second.cmp(&first), Ordering::Greater);
        assert_eq!(second.counter, base.counter);
    }

    // --- hostile ceiling nodes (un-advanceable final segment) ---

    const TOP: &str = "z"; // last character of the node alphabet

    fn seg(c: &str) -> String {
        c.repeat(HlcClock::NODE_SEGMENT_LENGTH)
    }

    fn chain(head: &str, tail: &str, tail_segments: usize) -> String {
        let mut parts = vec![seg(head)];
        for _ in 0..tail_segments {
            parts.push(seg(tail));
        }
        parts.join("!")
    }

    #[test]
    fn an_exhausted_final_segment_carries_into_the_previous_segment() {
        let node = chain("A", TOP, HlcClock::MAX_NODE_SEGMENTS - 1);
        assert_eq!(node.len(), HlcClock::MAX_NODE_LENGTH, "fixture sanity");
        let base = h(1000, 5, &node);

        let d = HlcClock::derived(&base, DEVICE);
        assert!(d > base, "must replicate over the base state");
        assert_eq!(d.wall_ms, base.wall_ms);
        assert_eq!(d.counter, base.counter, "never escalates the counter");
        assert_eq!(d.node.len(), HlcClock::MAX_NODE_LENGTH);
        // The carry treats the whole chain as one odometer.
        assert_eq!(
            d.node,
            chain("A", "0", 13).replacen(&seg("A"), &format!("{}B", "A".repeat(25)), 1)
        );
        let real_edit = h(base.wall_ms, base.counter + 1, DEVICE);
        assert!(d < real_edit);
        assert_eq!(
            Hlc::parse(&d.packed()).unwrap(),
            d,
            "round-trips like any stamp"
        );
    }

    #[test]
    fn a_chain_from_an_exhausted_final_segment_keeps_advancing_below_the_next_real_edit() {
        let base = h(1000, 5, &chain("A", TOP, HlcClock::MAX_NODE_SEGMENTS - 1));
        let first = HlcClock::derived(&base, DEVICE);
        let second = HlcClock::derived(&first, DEVICE);
        assert!(first > base);
        assert!(second > first);
        let real_edit = h(1000, 6, DEVICE);
        assert!(second < real_edit);
    }

    #[test]
    fn the_absolute_top_of_node_space_leaves_node_space_minimally() {
        let base = h(1000, 5, &chain(TOP, TOP, HlcClock::MAX_NODE_SEGMENTS - 1));
        let d = HlcClock::derived(&base, DEVICE);
        assert!(d > base, "must replicate over the base state");
        assert_eq!(d.wall_ms, base.wall_ms);
        assert_eq!(d.counter, base.counter + 1, "the minimal escalation");
        assert_eq!(d.node, DEVICE, "distinct per deriving device again");
        let later_edit = h(1000, 7, &"0".repeat(26));
        assert!(d < later_edit);
        assert_eq!(Hlc::parse(&d.packed()).unwrap(), d);
    }

    #[test]
    fn the_top_of_node_space_spill_respects_the_counter_cap() {
        let base = h(
            1000,
            HlcClock::MAX_COUNTER,
            &chain(TOP, TOP, HlcClock::MAX_NODE_SEGMENTS - 1),
        );
        let d = HlcClock::derived(&base, DEVICE);
        assert!(d > base);
        assert_eq!(d.wall_ms, base.wall_ms + 1);
        assert_eq!(d.counter, 0);
        assert!(
            base.packed() < d.packed(),
            "packed ordering stays monotonic across the spill"
        );
    }

    #[test]
    fn a_derivation_from_the_spilled_stamp_goes_back_to_plain_extension() {
        let base = h(1000, 5, &chain(TOP, TOP, HlcClock::MAX_NODE_SEGMENTS - 1));
        let spilled = HlcClock::derived(&base, DEVICE);
        let next = HlcClock::derived(&spilled, DEVICE);
        assert!(next > spilled);
        assert_eq!(next.counter, spilled.counter, "extension, not a bump");
        assert_eq!(next.node, format!("{}!{DEVICE}", spilled.node));
    }

    // --- HlcClock.tick (local writes) ---

    #[test]
    fn counter_increments_within_a_millisecond_and_resets_when_wall_advances() {
        let (now, read) = settable(1000);
        let clock = HlcClock::new("N").with_now(read);
        assert_eq!(clock.tick(), h(1000, 0, "N"));
        assert_eq!(clock.tick(), h(1000, 1, "N"));
        assert_eq!(clock.tick(), h(1000, 2, "N"));
        now.store(1001, AtomicOrdering::SeqCst);
        assert_eq!(clock.tick(), h(1001, 0, "N"));
    }

    #[test]
    fn stays_monotonic_when_the_wall_clock_goes_backwards() {
        let (now, read) = settable(5000);
        let clock = HlcClock::new("N").with_now(read);
        let a = clock.tick();
        now.store(4000, AtomicOrdering::SeqCst); // clock jumped back
        let b = clock.tick();
        assert!(b > a);
        assert_eq!(b.wall_ms, 5000); // held at the last seen wall
        assert_eq!(b.counter, 1);
    }

    #[test]
    fn a_once_future_clock_stays_strictly_monotonic_on_tick() {
        let (now, read) = settable(1000);
        let clock = HlcClock::new("N").with_now(read).with_max_drift_ms(60000);
        // Force the internal clock into the future via a remote merge (30s
        // ahead).
        clock.receive(&h(1000 + 30000, 0, "M")).unwrap();
        assert_eq!(clock.last().wall_ms, 31000);
        // Wall is still ~1000; tick holds at the last seen wall.
        now.store(1001, AtomicOrdering::SeqCst);
        assert_eq!(clock.tick().wall_ms, 31000);

        // Even when the last stamp is far ahead of wall, tick must NOT emit a
        // stamp that compares LESS than its own previous one.
        let skewed = h(1002 + 60000 + 5, 9, "N");
        let clock2 = HlcClock::new("N")
            .with_now(fixed(1002))
            .with_max_drift_ms(60000)
            .with_initial(skewed.clone());
        let t = clock2.tick();
        assert!(t > skewed, "strictly monotonic, never backward");
        assert_eq!(t.wall_ms, skewed.wall_ms);
        assert_eq!(t.counter, 10);
    }

    #[test]
    fn a_slow_local_clock_still_accepts_correctly_stamped_remotes() {
        let true_now = 10 * 60 * 1000;
        let clock = HlcClock::new("N")
            .with_now(fixed(1000)) // local clock 10 min behind real time
            .with_initial(h(true_now, 0, "N"));
        let merged = clock.receive(&h(true_now + 1000, 0, "M")).unwrap();
        assert_eq!(merged.wall_ms, true_now + 1000);
    }

    // --- HlcClock.receive (remote merge) ---

    #[test]
    fn adopts_a_remote_ahead_within_drift_and_advances_past_it() {
        let clock = HlcClock::new("N").with_now(fixed(1000));
        clock.tick(); // local at 1000/0
        let merged = clock.receive(&h(1005, 3, "M")).unwrap();
        assert_eq!(merged.wall_ms, 1005);
        assert_eq!(merged.counter, 4); // remote.counter + 1
        assert_eq!(merged.node, "N"); // the local clock keeps its own node
    }

    #[test]
    fn a_remote_in_the_past_does_not_move_us_backwards() {
        let clock = HlcClock::new("N").with_now(fixed(2000));
        let local = clock.tick(); // 2000/0
        let merged = clock.receive(&h(500, 9, "M")).unwrap();
        assert_eq!(merged.wall_ms, 2000);
        assert!(merged >= local);
    }

    #[test]
    fn ties_between_equal_local_and_remote_walls_take_max_counter_plus_one() {
        let clock = HlcClock::new("N")
            .with_now(fixed(1000))
            .with_initial(h(1000, 2, "N"));
        let merged = clock.receive(&h(1000, 7, "M")).unwrap();
        assert_eq!(merged.wall_ms, 1000);
        assert_eq!(merged.counter, 8); // max(2,7)+1
    }

    #[test]
    fn refuses_a_remote_implausibly_far_in_the_future() {
        let clock = HlcClock::new("N")
            .with_now(fixed(1000))
            .with_max_drift_ms(60000);
        let err = clock.receive(&h(1000 + 60001, 0, "BAD")).unwrap_err();
        assert_eq!(err.max_drift_ms, 60000);
        // The bad clock did not contaminate us.
        assert!(clock.last().wall_ms <= 1000);
        assert_eq!(
            err.to_string(),
            "HlcDriftException(remote=0000000061001-000000-BAD is 60001ms ahead \
             of now, > maxDrift 60000ms)"
        );
    }

    // --- HlcClock.assert_plausible (check-only drift gate) ---

    #[test]
    fn assert_plausible_judges_the_same_boundary_as_receive_without_mutating() {
        let max_drift = 60000;
        let remote = |wall: i64| h(wall, 0, "M");
        let probe = HlcClock::new("N")
            .with_now(fixed(1000))
            .with_max_drift_ms(max_drift);
        let mutate = HlcClock::new("N")
            .with_now(fixed(1000))
            .with_max_drift_ms(max_drift);

        // At the boundary: both accept.
        let at_edge = remote(1000 + max_drift);
        assert!(probe.assert_plausible(&at_edge).is_ok());
        assert!(mutate.receive(&at_edge).is_ok());
        // One past the boundary: both refuse.
        assert!(probe
            .assert_plausible(&remote(1000 + max_drift + 1))
            .is_err());
        assert!(mutate
            .receive(&remote(1000 + max_drift + max_drift + 1))
            .is_err());

        // The check-only clock is bit-for-bit where it started.
        assert_eq!(probe.last(), h(0, 0, "N"));
        assert_eq!(probe.max_wall_ever_seen(), 0);
        assert_eq!(probe.tick(), h(1000, 0, "N"));
    }

    #[test]
    fn assert_plausible_judges_against_max_of_now_and_max_wall_ever_seen() {
        let true_now = 10 * 60 * 1000;
        let clock = HlcClock::new("N")
            .with_now(fixed(1000))
            .with_initial(h(true_now, 0, "N"));
        let honest = h(true_now + 1000, 0, "M");
        assert!(clock.assert_plausible(&honest).is_ok());
        assert_eq!(clock.last().wall_ms, true_now);
        assert_eq!(clock.max_wall_ever_seen(), true_now);
        assert!(clock.receive(&honest).is_ok());
    }

    // --- HlcClock.merge_verified (post-commit advance, no drift re-check) ---

    #[test]
    fn merge_verified_advances_exactly_like_receive_for_a_plausible_stamp() {
        let via_merge = HlcClock::new("N").with_now(fixed(1000));
        let via_receive = HlcClock::new("N").with_now(fixed(1000));
        via_merge.tick();
        via_receive.tick();
        let remote = h(1005, 3, "M");
        assert_eq!(
            via_merge.merge_verified(&remote),
            via_receive.receive(&remote).unwrap()
        );
        assert_eq!(via_merge.last(), via_receive.last());
        assert_eq!(
            via_merge.max_wall_ever_seen(),
            via_receive.max_wall_ever_seen()
        );
        assert_eq!(via_merge.tick(), via_receive.tick());
        let again = h(1005, 9, "M");
        assert_eq!(
            via_merge.merge_verified(&again),
            via_receive.receive(&again).unwrap()
        );
    }

    #[test]
    fn merge_verified_accepts_a_stamp_receive_would_reject_and_stays_monotonic() {
        let (now, read) = settable(1000);
        let clock = HlcClock::new("N").with_now(read).with_max_drift_ms(60000);
        let before = clock.tick(); // 1000/0
        let vetted = h(1000 + 60000 + 5000, 2, "M");
        assert!(clock.receive(&vetted).is_err());
        assert_eq!(clock.last(), before);
        let merged = clock.merge_verified(&vetted);
        assert!(merged > before);
        assert!(merged > vetted, "strictly after the remote");
        assert_eq!(merged.wall_ms, vetted.wall_ms);
        assert_eq!(merged.counter, vetted.counter + 1);
        assert_eq!(clock.max_wall_ever_seen(), vetted.wall_ms);
        let t1 = clock.tick();
        assert!(t1 > merged);
        assert_eq!(t1.packed().len(), merged.packed().len());
        now.store(2000, AtomicOrdering::SeqCst);
        let t2 = clock.tick();
        assert!(t2 > t1);
    }

    #[test]
    fn a_vetted_stamp_still_merges_after_the_wall_clock_rolls_back() {
        let (now, read) = settable(1000);
        let clock = HlcClock::new("N").with_now(read).with_max_drift_ms(60000);
        let stamp = h(1000 + 60000, 0, "M");
        assert!(clock.assert_plausible(&stamp).is_ok());
        now.store(500, AtomicOrdering::SeqCst);
        assert!(clock.receive(&stamp).is_err());
        let merged = clock.merge_verified(&stamp);
        assert!(merged > stamp);
        assert_eq!(clock.last(), merged);
    }

    // --- counter cap (fixed 6-digit packed field) ---

    #[test]
    fn tick_spills_into_the_next_millisecond_at_the_counter_cap() {
        let clock = HlcClock::new("N").with_now(fixed(1000)).with_initial(h(
            5000,
            HlcClock::MAX_COUNTER - 1,
            "N",
        ));
        let at_cap = clock.tick();
        assert_eq!(at_cap, h(5000, HlcClock::MAX_COUNTER, "N"));
        let spilled = clock.tick();
        assert_eq!(spilled, h(5001, 0, "N"));
        assert!(spilled > at_cap);
        assert_eq!(spilled.packed().len(), at_cap.packed().len());
        assert!(at_cap.packed() < spilled.packed());
        let next = clock.tick();
        assert_eq!(next, h(5001, 1, "N"));
        assert!(spilled.packed() < next.packed());
    }

    #[test]
    fn receive_spills_instead_of_overflowing_the_counter() {
        let local = h(1000, HlcClock::MAX_COUNTER, "N");
        let clock = HlcClock::new("N")
            .with_now(fixed(1000))
            .with_initial(local.clone());
        let merged = clock.receive(&h(1000, HlcClock::MAX_COUNTER, "M")).unwrap();
        assert_eq!(merged, h(1001, 0, "N"));
        assert!(merged > local);
        assert!(local.packed() < merged.packed());
    }

    #[test]
    fn two_clocks_converge_to_a_deterministic_total_order() {
        let a = HlcClock::new("AAA").with_now(fixed(100));
        let b = HlcClock::new("BBB").with_now(fixed(100));
        let s1 = a.tick();
        let s2 = b.receive(&s1).unwrap();
        let s3 = a.receive(&s2).unwrap();
        assert!(s1 < s2);
        assert!(s2 < s3);
        assert!(s1 < s3);
    }

    #[test]
    fn merge_if_not_behind_skips_an_older_stamp() {
        let clock = HlcClock::new("N")
            .with_now(fixed(1000))
            .with_initial(h(2000, 3, "N"));
        assert!(!clock.merge_verified_if_not_behind(&h(1500, 0, "M")));
        assert_eq!(clock.last(), h(2000, 3, "N"));
        assert!(clock.merge_verified_if_not_behind(&h(2000, 3, "N")));
        assert_eq!(clock.last(), h(2000, 4, "N"));
    }
}
