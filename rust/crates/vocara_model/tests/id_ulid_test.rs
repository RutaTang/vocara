//! Port of `client/test/id_ulid_test.dart`: the ULIDs `new_id` mints are
//! Crockford base32, unique, and sort by creation. (The minter's rules on a
//! clock that stalls or goes back are `util::id`'s own tests.)

use std::collections::HashSet;
use std::thread;
use std::time::Duration;

use vocara_model::util::id::new_id;

/// `^[0-9A-HJKMNP-TV-Z]{26}$`: 26 characters, no I, L, O or U.
fn is_crockford_ulid(id: &str) -> bool {
    id.len() == 26
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_uppercase() && !b"ILOU".contains(&b)))
}

#[test]
fn is_26_chars_of_crockford_base32_with_no_i_l_o_or_u() {
    for _ in 0..200 {
        let id = new_id();
        assert_eq!(id.len(), 26, "{id}");
        assert!(is_crockford_ulid(&id), "{id}");
    }
}

#[test]
fn is_unique_across_many_rapid_calls() {
    let mut seen = HashSet::new();
    for _ in 0..5000 {
        assert!(seen.insert(new_id()), "duplicate id");
    }
}

#[test]
fn is_strictly_monotonic_lexicographic_within_a_burst() {
    // Many ids minted in the same/adjacent ms must still sort by creation.
    let mut prev = new_id();
    for _ in 0..5000 {
        let next = new_id();
        assert!(next > prev, "not increasing: {prev} -> {next}");
        prev = next;
    }
}

#[test]
fn lexicographic_order_matches_time_order_across_millis() {
    let a = new_id();
    thread::sleep(Duration::from_millis(3));
    let b = new_id();
    assert!(b > a);
    // The 10-char timestamp prefix is non-decreasing with wall time.
    assert!(b[..10] >= a[..10]);
}
