//! Wave 3 — the final gate of P0 sync: seeded randomized convergence
//! simulation (the Automerge/Ditto-style gold standard for sync engines).
//! Port of `client/test/sync_simulation_test.dart`; the harness is
//! `harness.rs` (port of `test/support/sync_sim_harness.dart`).
//!
//! Each episode drives 2..4 full device stacks over one fake cloud with a
//! seeded mix of repository-level edits through HELD EDITORS (aggregate +
//! base hashes kept across other devices' syncs), explicit deletes, and
//! transport faults (partitions, page-size churn, held pushes, injected push
//! conflicts, verbatim page re-delivery, killed page commits). At quiescence
//! it asserts full-database convergence (HLC + tombstone + content hash for
//! every synced row), per-device integrity invariants, drained oplogs, empty
//! quarantines, change tokens at the cloud head, and the tombstone/revive
//! oracle.
//!
//! Every episode is fully deterministic per (seed, opCount): all randomness
//! flows from one `Random(seed)` — Dart's VM generator, reimplemented bit
//! for bit (`dart_random.rs`), so a seed draws the decision sequence of its
//! Dart episode; HLC time is a logical episode clock with per-device skews
//! (no wall clock in any control path). On failure the harness panics with
//! the seed, the first diverging row with cloud forensics, and the complete
//! replayable op log.
//!
//! The suite found (and now guards against) four real sync bugs in Dart;
//! see `tests/sync/sim_regressions.rs` for the unit-level pins:
//!  * seed 0x5eed0001 — cascade deletes uploaded child-first, so a page
//!    boundary made the FK sweep drop rows forever (fixed by parent-first
//!    push ordering in SyncEngine/SyncRowBuilder).
//!  * seed 0x5eed0009 / 0xabd782 — the single-slot resolution was a local,
//!    un-journaled write: divergent liveness under identical HLCs (fixed by
//!    derived-stamp + journaled resolution in SyncApplier).
//!  * seed 0xc1c2b7 — integrity repair ran BETWEEN pull pages and "healed"
//!    half-delivered aggregates (fixed by end-of-run repair in SyncEngine).
//!  * seed 0xab1a57 — repairs stamped fresh clock ticks, letting bookkeeping
//!    out-rank a genuinely later delete and resurrect it (fixed by derived
//!    repair stamps in IntegrityChecker).
//!
//! The seeds keep their meaning in Rust: with the IntegrityChecker's repair
//! stamp switched back to a fresh tick, the 0xab1a57 probe below fails with
//! the oracle's resurrection report (a deleted practice live everywhere),
//! the bug it was pinned for.
//!
//! Runtime: everything below runs by default, as in Dart (about 5 s for the
//! whole file in a debug build, the 12-episode deep sweep the longest).
//! Wider probing runs — the kind that found the pinned probe seeds — are
//! the ignored `probe_sweep_from_env`, e.g.
//! `VOCARA_SIM_BASE=0x1000 VOCARA_SIM_EPISODES=500 VOCARA_SIM_OPS=150 cargo
//! test -p vocara_store --test sync_simulation -- --ignored`.

mod dart_random;
mod harness;

use harness::run_sync_sim_episode;

// --- (a) fixed battery: hand-picked seeds as permanent regression tests ---
//
// Ten seeds at the standard episode size. opCount is part of the episode
// fingerprint: changing it changes the whole episode, so these stay pinned
// at 120 forever. 0x5eed0001 and 0x5eed0009 are the seeds that first
// exposed the child-first-upload row loss and the slot-resolution
// divergence + heal resurrection.

macro_rules! battery {
    ($($name:ident = $seed:expr),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                run_sync_sim_episode($seed, 120, None);
            }
        )*
    };
}

battery! {
    battery_seed_0x5eed0001_converges_with_invariants = 0x5EED_0001,
    battery_seed_0x5eed0002_converges_with_invariants = 0x5EED_0002,
    battery_seed_0x5eed0003_converges_with_invariants = 0x5EED_0003,
    battery_seed_0x5eed0004_converges_with_invariants = 0x5EED_0004,
    battery_seed_0x5eed0005_converges_with_invariants = 0x5EED_0005,
    battery_seed_0x5eed0006_converges_with_invariants = 0x5EED_0006,
    battery_seed_0x5eed0007_converges_with_invariants = 0x5EED_0007,
    battery_seed_0x5eed0008_converges_with_invariants = 0x5EED_0008,
    battery_seed_0x5eed0009_converges_with_invariants = 0x5EED_0009,
    battery_seed_0x5eed000a_converges_with_invariants = 0x5EED_000A,
}

// Bug-finder episodes from wider probing runs, pinned verbatim (seed AND
// opCount) as permanent regressions.

#[test]
fn probe_seed_0xab1a57_repair_resurrection_episode_converges() {
    run_sync_sim_episode(0xAB_1A57, 150, None);
}

#[test]
fn probe_seed_0xabd782_slot_divergence_episode_converges() {
    run_sync_sim_episode(0xAB_D782, 150, None);
}

#[test]
fn sweep_seed_0xc1c2b7_mid_pull_heal_episode_converges() {
    // Sweep index 5 (0xC0FFEE + 5 * 9973), named for direct replay.
    run_sync_sim_episode(0xC1_C2B7, 60, None);
}

// --- (b) sweep: many smaller episodes from a FIXED base constant ----------

#[test]
fn seed_sweep_30_episodes_converge_base_0xc0ffee_ci_deterministic() {
    for i in 0..30 {
        let seed = 0xC0_FFEE + i * 9973;
        run_sync_sim_episode(seed, 60, None);
    }
}

// --- (c) deep sweep: fewer but longer episodes (more editor churn, more
// delete/revive cycles per practice). Runtime headroom makes this cheap.

#[test]
fn deep_sweep_12_long_episodes_converge_base_0xf00d00() {
    for i in 0..12 {
        let seed = 0xF0_0D00 + i * 271;
        run_sync_sim_episode(seed, 200, None);
    }
}

// --- (d) probing: a wider sweep, sized from the environment ---------------

/// An integer from the environment, decimal or `0x` hex.
fn env_int(name: &str, default: i64) -> i64 {
    match std::env::var(name) {
        Err(_) => default,
        Ok(v) => match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
            Some(hex) => i64::from_str_radix(hex, 16),
            None => v.parse(),
        }
        .unwrap_or_else(|e| panic!("{name}={v}: {e}")),
    }
}

#[test]
#[ignore = "probing run sized by VOCARA_SIM_{BASE,EPISODES,STRIDE,OPS,DEVICES}"]
fn probe_sweep_from_env() {
    let base = env_int("VOCARA_SIM_BASE", 0xC0_FFEE);
    let episodes = env_int("VOCARA_SIM_EPISODES", 100);
    let stride = env_int("VOCARA_SIM_STRIDE", 9973);
    let ops = usize::try_from(env_int("VOCARA_SIM_OPS", 120)).expect("ops");
    let devices = usize::try_from(env_int("VOCARA_SIM_DEVICES", 0)).expect("devices");
    for i in 0..episodes {
        run_sync_sim_episode(base + i * stride, ops, (devices > 0).then_some(devices));
    }
}
