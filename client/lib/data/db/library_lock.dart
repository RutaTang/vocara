import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart' show visibleForTesting;
import 'package:path/path.dart' as p;

import '../services/diagnostics_log.dart';

/// What the library's single-instance guard lets this process do: only an
/// EXCLUSIVE holder may reclaim anything (boot recovery, the boot sweeps,
/// Repair library's GC). See [LibraryLock].
abstract interface class LibraryGuard {
  /// A guard that answers [tryExclusive] with [exclusive], always, and holds
  /// no lock: for stores no other process can share (in memory) and for
  /// tests that stand in for another process.
  factory LibraryGuard.fixed({required bool exclusive}) = _FixedGuard;

  /// Takes the library exclusively, or returns false when another Vocara
  /// process holds it (shared or exclusive). Never waits. Every true must be
  /// paired with a [releaseExclusive].
  Future<bool> tryExclusive();

  /// Ends one [tryExclusive] that returned true. The last one in this
  /// process hands the library back to shared, so it stays guarded.
  Future<void> releaseExclusive();
}

class _FixedGuard implements LibraryGuard {
  _FixedGuard({required this.exclusive});

  final bool exclusive;

  @override
  Future<bool> tryExclusive() async => exclusive;

  @override
  Future<void> releaseExclusive() async {}
}

/// The single-instance guard for one library directory: an advisory lock
/// on `<dir>/vocara.lock`.
///
/// Boot recovery and every destructive sweep assume that nothing else is
/// writing the library: a staged WAV with no row is a crashed take, a pool
/// file with no row is a crash leftover, the staging trees hold nothing in
/// flight. A second Vocara process on the same library (`open -n`, or two
/// builds that share a bundle id) breaks all of that while the other one is
/// recording or saving: a boot adopts its half-written take as
/// "(recovered)" and deletes the file, and the staging sweep trashes what it
/// has not saved yet. The migrator makes the same assumption.
///
/// So EVERY running instance holds the lock SHARED for its whole life, and
/// reclaiming needs it EXCLUSIVE, which no process can get while any other
/// one holds it at all:
///
///  * at boot, [forLibrary] takes it shared and at once tries to upgrade it,
///    both while holding a separate LAUNCH GATE (byte 1 of the file, held
///    only for those two non-blocking calls): so no other launch can take
///    shared between this one's shared and its upgrade. Without the gate two
///    launches racing for a library nobody held — at the same moment, or
///    both waiting on an owner's boot that died — each refused the other's
///    upgrade, leaving the library unowned and a pending upgrade refused by
///    both. A launch that upgrades is the boot's owner ([tryExclusive] hands
///    it that claim): it runs recovery and the sweeps, then hands the lock
///    back to shared. One that can't take shared at all (an owner is mid
///    boot) waits — however long that boot takes, telling the app once the
///    wait runs long, but NEVER going on without a hold: an instance with no
///    hold is one the owner's sweeps cannot see, and they trashed the take it
///    was recording;
///  * Repair library tries the same upgrade and reclaims nothing without it.
///
/// An instance that started second keeps its shared hold, so it still guards
/// the library after the first one quits: a third launch then cannot own it
/// either. (Holding the lock only in the first process left every later
/// instance unguarded the moment the first one exited.)
///
/// The lock is a POSIX record lock ([RandomAccessFile.lock]) on byte 0 of the
/// file, the launch gate one on byte 1 (locks on different bytes never
/// conflict): a conversion between shared and exclusive on the same
/// descriptor is atomic, and a refused upgrade leaves the shared hold in
/// place. Record locks belong to
/// the PROCESS: two requests from this process never conflict, and closing
/// ANY descriptor this process holds on the file drops them all. So the file
/// is opened once per library (read and write, which a shared and an
/// exclusive lock each need), the guard is kept in [_byPath] for the life of
/// the isolate and never closed, a later open of the same library in this
/// process (tests open several stores over one directory) reuses it, and
/// exclusive holds inside this process are counted. The kernel releases
/// everything when the process exits, however it exits.
class LibraryLock implements LibraryGuard {
  LibraryLock._(this.path, this._file);

  /// The lock file's name inside the library directory.
  static const String fileName = 'vocara.lock';

  /// How long a launch waits on an owner's boot before [forLibrary] tells
  /// the app (which then shows a waiting window; the wait goes on).
  static const Duration defaultWait = Duration(seconds: 2);

  /// How often a wait for the shared hold retries.
  static const Duration defaultPoll = Duration(milliseconds: 100);

  /// The byte the library lock covers, and the launch gate's (see the class
  /// doc).
  static const int _libraryByte = 0;
  static const int _gateByte = 1;

  /// This process's guards by canonical lock-file path, registered before
  /// the first await so two opens racing on one library share one guard.
  /// Never closed (see the class doc).
  static final Map<String, Future<LibraryLock>> _byPath = {};

  /// Errno values a non-blocking lock attempt fails with when another
  /// process holds a conflicting lock: EACCES, and EAGAIN (11 on Linux, 35
  /// on macOS). Plus EINTR, belt and braces: the Dart VM retries an
  /// interrupted fcntl itself, but should one ever surface it is a retry, not
  /// a filesystem without locks (which turns the guard off).
  static const Set<int> _heldElsewhere = {13, 11, 35, 4};

  final String path;

  /// The open lock file, or null when it could not be opened or locked at
  /// all (see [_unsupported]).
  final RandomAccessFile? _file;

  /// Whether this process holds the lock at least shared right now.
  bool _shared = false;

  /// [tryExclusive] calls in this process that returned true and are not
  /// released yet. The lock is exclusive while this is above zero.
  int _exclusiveHolds = 0;

  /// The exclusive hold [forLibrary] took from nothing, not yet handed to a
  /// [tryExclusive] (the boot's): it counts in [_exclusiveHolds] already.
  bool _bootClaim = false;

  /// Set when the platform refused the lock for a reason that says nothing
  /// about another process (no lock file, a filesystem without record
  /// locks). The guard then stands down entirely: every [tryExclusive] is
  /// granted, which is how the app behaved before the guard existed, rather
  /// than never running crash recovery on such a volume again.
  bool _unsupported = false;

  /// Serializes every lock call on [_file]: two in flight could convert each
  /// other's lock (a shared retry landing on an exclusive hold downgrades
  /// it).
  Future<void> _serial = Future.value();

  Future<T> _locked<T>(Future<T> Function() op) {
    final run = _serial.then((_) => op());
    _serial = run.then((_) {}, onError: (_) {});
    return run;
  }

  /// Whether this process holds the lock (shared or exclusive) right now.
  @visibleForTesting
  bool get holdsLock => _shared || _exclusiveHolds > 0;

  /// This process's guard for [dir]: holding the lock exclusive when this
  /// launch won it from nothing (the boot's claim, see the class doc), else
  /// shared — taken only once an owner's boot hands it back. [onStillWaiting]
  /// is called once if that takes longer than [wait]; the wait goes on.
  static Future<LibraryLock> forLibrary(
    Directory dir, {
    Duration wait = defaultWait,
    Duration poll = defaultPoll,
    void Function()? onStillWaiting,
  }) async {
    final path = p.join(p.canonicalize(dir.path), fileName);
    final known = _byPath[path];
    if (known != null) {
      final lock = await known;
      // A guard for a lock file that has since been deleted (a test tearing
      // its directory down and making a new one at the same path) guards
      // nothing: open the new file.
      if (lock._file == null || await File(path).exists()) return lock;
    }
    final created = _open(path, wait, poll, onStillWaiting);
    _byPath[path] = created;
    return created;
  }

  static Future<LibraryLock> _open(
    String path,
    Duration wait,
    Duration poll,
    void Function()? onStillWaiting,
  ) async {
    RandomAccessFile? file;
    try {
      // FileMode.append opens for reading AND writing (creating the file),
      // which the shared and the exclusive lock each require.
      file = await File(path).open(mode: FileMode.append);
    } on FileSystemException catch (e) {
      _logUnsupported(e);
    }
    final lock = LibraryLock._(path, file);
    if (file == null) {
      lock._unsupported = true;
      return lock;
    }
    final slow = DateTime.now().add(wait);
    var told = false;
    while (true) {
      // Shared, then the upgrade, both inside the launch gate (see the class
      // doc). Refused shared means an owner is mid boot: let the gate go and
      // wait for it.
      if (await lock._locked(lock._tryGate)) {
        try {
          if (await lock._locked(lock._tryShared)) {
            await lock._locked(lock._tryBootClaim);
            return lock;
          }
        } finally {
          await lock._locked(lock._releaseGate);
        }
      }
      if (lock._unsupported) return lock;
      if (!told && !DateTime.now().isBefore(slow)) {
        told = true;
        DiagnosticsLog.instance.log(
          'persistence',
          'library lock still held exclusively by another process after '
              '${wait.inMilliseconds} ms; still waiting for it',
        );
        onStillWaiting?.call();
      }
      await Future<void>.delayed(poll);
    }
  }

  /// The upgrade of a fresh shared hold that makes this launch the boot's
  /// owner, never waiting: refused while any other process holds the lock at
  /// all. Call only through [_locked], inside the launch gate.
  Future<bool> _tryBootClaim() async {
    if (!await _lock(FileLock.exclusive)) return false;
    _exclusiveHolds = 1;
    _bootClaim = true;
    return true;
  }

  /// The launch gate, never waiting. Call only through [_locked].
  Future<bool> _tryGate() async {
    if (_unsupported) return false;
    return _lock(FileLock.exclusive, byte: _gateByte);
  }

  /// Lets the launch gate go. Call only through [_locked].
  Future<void> _releaseGate() async {
    try {
      await _file!.unlock(_gateByte, _gateByte + 1);
    } on FileSystemException catch (_) {
      // Nothing to let go of; the kernel drops it with the process anyway.
    }
  }

  /// One non-blocking attempt at the shared hold. Call only through
  /// [_locked].
  Future<bool> _tryShared() async {
    if (_unsupported) return false;
    if (_shared || _exclusiveHolds > 0) return true;
    if (await _lock(FileLock.shared)) _shared = true;
    return _shared;
  }

  @override
  Future<bool> tryExclusive() => _locked(() async {
    if (_unsupported) return true;
    if (_bootClaim) {
      // The hold [forLibrary] took from nothing: this is the boot's.
      _bootClaim = false;
      return true;
    }
    if (_exclusiveHolds > 0) {
      _exclusiveHolds++;
      return true;
    }
    // Converts this process's shared hold in place when there is one; never
    // waits. Refused while any other process holds the lock, and the shared
    // hold survives the refusal.
    if (!await _lock(FileLock.exclusive)) return _unsupported;
    _exclusiveHolds = 1;
    return true;
  });

  @override
  Future<void> releaseExclusive() => _locked(() async {
    if (_unsupported || _exclusiveHolds == 0) return;
    if (--_exclusiveHolds > 0) return;
    // Back to shared, never to nothing: the library stays guarded. A
    // downgrade cannot conflict with anything.
    _shared = await _lock(FileLock.shared);
  });

  /// One non-blocking lock call on [byte] (the library's unless told). Returns
  /// false when another process holds a conflicting lock; any other refusal
  /// marks the guard [_unsupported].
  Future<bool> _lock(FileLock mode, {int byte = _libraryByte}) async {
    try {
      await _file!.lock(mode, byte, byte + 1);
      return true;
    } on FileSystemException catch (e) {
      final code = e.osError?.errorCode;
      if (code != null && _heldElsewhere.contains(code)) return false;
      _logUnsupported(e);
      _unsupported = true;
      return false;
    }
  }

  static void _logUnsupported(FileSystemException e) {
    DiagnosticsLog.instance.log(
      'persistence',
      'library lock unavailable (${e.osError?.errorCode}); '
          'proceeding without the single-instance guard',
    );
  }
}
