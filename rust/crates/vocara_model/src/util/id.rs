//! ULID ids: the stable cross-device primary key and wire record key
//! (port of `lib/utils/id.dart`).

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngCore;

/// Crockford base32 alphabet (excludes I, L, O, U to avoid ambiguity). The
/// characters are in ascending byte order, so a ULID's lexicographic string
/// order matches its numeric order — i.e. ULIDs sort by creation time.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The randomness ceiling: 80 bits.
const RANDOM_CEILING: u128 = 1 << 80;

/// The minter's state: the highest millisecond timestamp handed out so far,
/// so [`new_id`] never moves backwards even if the wall clock does, and the
/// 80-bit randomness of the last id minted at it, bumped by one for each
/// additional id in the same millisecond (monotonic factory).
struct Minter {
    last_timestamp_ms: i64,
    last_randomness: u128,
}

static MINTER: Mutex<Minter> = Mutex::new(Minter {
    last_timestamp_ms: -1,
    last_randomness: 0,
});

/// Generates a ULID: a 26-character Crockford base32 string encoding a 48-bit
/// millisecond timestamp followed by 80 bits of randomness. It is globally
/// unique, lexicographically sortable by creation time, and monotonic within
/// a millisecond — so it doubles as the stable cross-device primary key and
/// the wire record key (an id never needs re-minting on import or restore).
pub fn new_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    new_id_at(now)
}

/// [`new_id`] with the wall clock given, for tests of the monotonic rules.
pub fn new_id_at(now_ms: i64) -> String {
    let mut m = MINTER.lock().unwrap_or_else(|e| e.into_inner());
    if now_ms <= m.last_timestamp_ms {
        // Same (or an earlier) millisecond: increment the previous randomness
        // to keep ids strictly increasing.
        m.last_randomness += 1;
        if m.last_randomness >= RANDOM_CEILING {
            // 80-bit overflow (astronomically unlikely): spill into the next ms.
            m.last_timestamp_ms += 1;
            m.last_randomness = random80();
        }
    } else {
        m.last_timestamp_ms = now_ms;
        m.last_randomness = random80();
    }
    let value = ((m.last_timestamp_ms as u128) << 80) | m.last_randomness;
    encode(value)
}

/// 80 bits of cryptographic randomness.
fn random80() -> u128 {
    let mut bytes = [0u8; 10];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().fold(0u128, |r, b| (r << 8) | u128::from(*b))
}

/// Encodes a 128-bit value as 26 Crockford base32 characters (5 bits each; the
/// leading character carries the top 2 padding bits, so it is always 0–7).
pub fn encode(value: u128) -> String {
    let mut chars = [b'0'; 26];
    let mut v = value;
    for c in chars.iter_mut().rev() {
        *c = CROCKFORD[(v & 31) as usize];
        v >>= 5;
    }
    // Every byte comes from the ASCII alphabet above.
    String::from_utf8(chars.to_vec()).unwrap_or_default()
}

/// The shape an id must have to be usable as a FILESYSTEM PATH COMPONENT:
/// exactly 26 uppercase alphanumerics, which every [`new_id`] ULID satisfies.
///
/// Deliberately the whole uppercase-alphanumeric set rather than the Crockford
/// alphabet [`new_id`] mints from: this is a PATH-SAFETY rule (no separator,
/// no `..`, no extension, fixed width), not a check that an id came from this
/// app's minter, and being stricter than the property it guards would reject
/// ids the filesystem handles perfectly well.
///
/// The sync wire otherwise admits a superset (see `SyncRow::check_id_shapes`)
/// so legacy derived ids like `<trackId>-c0` keep syncing, but the media
/// services build `audio/<practiceId>/` and `technique_media/<techniqueId>/`
/// from these and recursively delete them — so for those columns the two
/// rules must agree, or a row can sync successfully and then be un-operable
/// locally (every media call for it throws). One definition, read by the wire
/// gate and by the services that do the building.
pub fn is_path_component_id(id: &str) -> bool {
    id.len() == 26
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_26_crockford_chars_and_path_safe() {
        let id = new_id();
        assert_eq!(id.len(), 26);
        assert!(id.bytes().all(|b| CROCKFORD.contains(&b)));
        assert!(is_path_component_id(&id));
        assert!(id.as_bytes()[0] <= b'7', "top 2 bits are padding");
    }

    #[test]
    fn ids_increase_within_a_millisecond_and_when_the_clock_goes_back() {
        // A far-future base, so the shared minter state of other tests (which
        // use the real clock) cannot interleave below it.
        let base = 40_000_000_000_000;
        let a = new_id_at(base);
        let b = new_id_at(base);
        let c = new_id_at(base - 5_000); // the clock moved backwards
        let d = new_id_at(base + 1);
        assert!(a < b && b < c && c < d, "{a} {b} {c} {d}");
    }

    #[test]
    fn the_encoding_orders_like_the_value() {
        let values = [0u128, 1, 31, 32, (1 << 80) - 1, 1 << 80, u128::MAX >> 2];
        for w in values.windows(2) {
            assert!(encode(w[0]) < encode(w[1]));
        }
        assert_eq!(encode(0), "00000000000000000000000000");
        assert_eq!(encode(31), "0000000000000000000000000Z");
    }

    #[test]
    fn path_component_ids_are_exactly_26_uppercase_alphanumerics() {
        assert!(is_path_component_id("01HZZZZZZZZZZZZZZZZZZZZZZZ"));
        assert!(is_path_component_id("ABCDEFGHIJKLMNOPQRSTUVWXYZ")); // I L O U allowed
        assert!(!is_path_component_id("01hzzzzzzzzzzzzzzzzzzzzzzz"));
        assert!(!is_path_component_id("01HZZZZZZZZZZZZZZZZZZZZZZ"));
        assert!(!is_path_component_id("01HZZZZZZZZZZZZZZZZZZZZZZZ-c0"));
        assert!(!is_path_component_id("../ZZZZZZZZZZZZZZZZZZZZZZZZ"));
        assert!(!is_path_component_id("ÄBCDEFGHIJKLMNOPQRSTUVWXY"));
    }
}
