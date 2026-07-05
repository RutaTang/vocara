import 'package:drift/drift.dart' show driftRuntimeOptions;
import 'package:flutter_test/flutter_test.dart';

import 'support/sync_sim_harness.dart';

/// Wave 3 — the final gate of P0 sync: seeded randomized convergence
/// simulation (the Automerge/Ditto-style gold standard for sync engines).
///
/// Each episode drives 2..4 full device stacks over one fake cloud with a
/// seeded mix of repository-level edits through HELD EDITORS (aggregate +
/// base hashes kept across other devices' syncs), explicit deletes, and
/// transport faults (partitions, page-size churn, held pushes, injected push
/// conflicts, verbatim page re-delivery, killed page commits). At quiescence
/// it asserts full-database convergence (HLC + tombstone + content hash for
/// every synced row), per-device integrity invariants, drained oplogs, empty
/// quarantines, change tokens at the cloud head, and the tombstone/revive
/// oracle.
///
/// Every episode is fully deterministic per (seed, opCount): all randomness
/// flows from one Random(seed); HLC time is a logical episode clock with
/// per-device skews (no wall clock in any control path). On failure the
/// harness prints the seed, the first diverging row with cloud forensics,
/// and the complete replayable op log.
///
/// The suite found (and now guards against) four real sync bugs; see
/// test/sync_sim_regressions_test.dart for the unit-level pins:
///  * seed 0x5eed0001 — cascade deletes uploaded child-first, so a page
///    boundary made the FK sweep drop rows forever (fixed by parent-first
///    push ordering in SyncEngine/SyncRowBuilder).
///  * seed 0x5eed0009 / 0xabd782 — the single-slot resolution was a local,
///    un-journaled write: divergent liveness under identical HLCs (fixed by
///    derived-stamp + journaled resolution in SyncApplier).
///  * seed 0xc1c2b7 — integrity repair ran BETWEEN pull pages and "healed"
///    half-delivered aggregates (fixed by end-of-run repair in SyncEngine).
///  * seed 0xab1a57 — repairs stamped fresh clock ticks, letting bookkeeping
///    out-rank a genuinely later delete and resurrect it (fixed by derived
///    repair stamps in IntegrityChecker).
void main() {
  setUpAll(() {
    // Multiple live AppDatabase instances (one per simulated device) are the
    // whole point of this suite; each has its own in-memory executor.
    driftRuntimeOptions.dontWarnAboutMultipleDatabases = true;
  });

  // --- (a) fixed battery: hand-picked seeds as permanent regression tests ---

  // Ten seeds at the standard episode size. opCount is part of the episode
  // fingerprint: changing it changes the whole episode, so these stay pinned
  // at 120 forever. 0x5eed0001 and 0x5eed0009 are the seeds that first
  // exposed the child-first-upload row loss and the slot-resolution
  // divergence + heal resurrection.
  const battery = <int>[
    0x5EED0001,
    0x5EED0002,
    0x5EED0003,
    0x5EED0004,
    0x5EED0005,
    0x5EED0006,
    0x5EED0007,
    0x5EED0008,
    0x5EED0009,
    0x5EED000A,
  ];

  for (final seed in battery) {
    test(
      'battery seed 0x${seed.toRadixString(16)} converges with invariants',
      () => runSyncSimEpisode(seed: seed, opCount: 120),
      timeout: const Timeout(Duration(minutes: 2)),
    );
  }

  // Bug-finder episodes from wider probing runs, pinned verbatim (seed AND
  // opCount) as permanent regressions.
  test(
    'probe seed 0xab1a57 (repair-resurrection episode) converges',
    () => runSyncSimEpisode(seed: 0xAB1A57, opCount: 150),
    timeout: const Timeout(Duration(minutes: 2)),
  );
  test(
    'probe seed 0xabd782 (slot-divergence episode) converges',
    () => runSyncSimEpisode(seed: 0xABD782, opCount: 150),
    timeout: const Timeout(Duration(minutes: 2)),
  );
  test(
    'sweep seed 0xc1c2b7 (mid-pull heal episode) converges',
    // Sweep index 5 (0xC0FFEE + 5 * 9973), named for direct replay.
    () => runSyncSimEpisode(seed: 0xC1C2B7, opCount: 60),
    timeout: const Timeout(Duration(minutes: 2)),
  );

  // --- (b) sweep: many smaller episodes from a FIXED base constant ----------

  test(
    'seed sweep: 30 episodes converge (base 0xC0FFEE, CI-deterministic)',
    () async {
      for (var i = 0; i < 30; i++) {
        final seed = 0xC0FFEE + i * 9973;
        await runSyncSimEpisode(seed: seed, opCount: 60);
      }
    },
    timeout: const Timeout(Duration(minutes: 5)),
  );

  // --- (c) deep sweep: fewer but longer episodes (more editor churn, more
  // delete/revive cycles per practice). Runtime headroom makes this cheap.

  test(
    'deep sweep: 12 long episodes converge (base 0xF00D00)',
    () async {
      for (var i = 0; i < 12; i++) {
        final seed = 0xF00D00 + i * 271;
        await runSyncSimEpisode(seed: seed, opCount: 200);
      }
    },
    timeout: const Timeout(Duration(minutes: 5)),
  );
}
