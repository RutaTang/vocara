import 'dart:math';

/// Crockford base32 alphabet (excludes I, L, O, U to avoid ambiguity). The
/// characters are in ascending byte order, so a ULID's lexicographic string
/// order matches its numeric order — i.e. ULIDs sort by creation time.
const String _crockford = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

final Random _secureRandom = Random.secure();

/// Highest millisecond timestamp handed out so far, so [newId] never moves
/// backwards even if the wall clock does, and stays monotonic within a ms.
int _lastTimestampMs = -1;

/// The 80-bit randomness of the last id minted at [_lastTimestampMs]; bumped by
/// one for each additional id in the same millisecond (monotonic factory).
BigInt _lastRandomness = BigInt.zero;

final BigInt _randomCeiling = BigInt.one << 80;

/// Generates a ULID: a 26-character Crockford base32 string encoding a 48-bit
/// millisecond timestamp followed by 80 bits of randomness. It is globally
/// unique, lexicographically sortable by creation time, and monotonic within a
/// millisecond — so it doubles as the stable cross-device primary key and the
/// wire record key (an id never needs re-minting on import or restore).
///
/// The signature is unchanged from the previous local-only id, so every call
/// site keeps working.
String newId() {
  final now = DateTime.now().millisecondsSinceEpoch;
  if (now <= _lastTimestampMs) {
    // Same (or an earlier) millisecond: increment the previous randomness to
    // keep ids strictly increasing.
    _lastRandomness += BigInt.one;
    if (_lastRandomness >= _randomCeiling) {
      // 80-bit overflow (astronomically unlikely): spill into the next ms.
      _lastTimestampMs += 1;
      _lastRandomness = _random80();
    }
  } else {
    _lastTimestampMs = now;
    _lastRandomness = _random80();
  }
  final value = (BigInt.from(_lastTimestampMs) << 80) | _lastRandomness;
  return _encode(value);
}

/// 80 bits of cryptographic randomness as a [BigInt].
BigInt _random80() {
  var r = BigInt.zero;
  for (var i = 0; i < 10; i++) {
    r = (r << 8) | BigInt.from(_secureRandom.nextInt(256));
  }
  return r;
}

/// Encodes a 128-bit value as 26 Crockford base32 characters (5 bits each; the
/// leading character carries the top 2 padding bits, so it is always 0–7).
String _encode(BigInt value) {
  final mask = BigInt.from(31);
  final chars = List<String>.filled(26, '0');
  var v = value;
  for (var i = 25; i >= 0; i--) {
    chars[i] = _crockford[(v & mask).toInt()];
    v = v >> 5;
  }
  return chars.join();
}

/// The shape an id must have to be usable as a FILESYSTEM PATH COMPONENT:
/// exactly 26 uppercase alphanumerics, which every [newId] ULID satisfies.
///
/// Deliberately the whole uppercase-alphanumeric set rather than the Crockford
/// alphabet [newId] mints from: this is a PATH-SAFETY rule (no separator, no
/// `..`, no extension, fixed width), not a check that an id came from this
/// app's minter, and being stricter than the property it guards would reject
/// ids the filesystem handles perfectly well.
///
/// The sync wire otherwise admits a superset (see `SyncRow.checkIdShapes`) so
/// legacy derived ids like `<trackId>-c0` keep syncing, but the media services
/// build `audio/<practiceId>/` and `technique_media/<techniqueId>/` from these
/// and recursively delete them — so for those columns the two rules must
/// agree, or a row can sync successfully and then be un-operable locally
/// (every media call for it throws). One definition, read by the wire gate and
/// by the services that do the building.
bool isPathComponentId(String id) => _pathComponentShape.hasMatch(id);

final RegExp _pathComponentShape = RegExp(r'^[0-9A-Z]{26}$');
