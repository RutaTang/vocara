import 'dart:async';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/app_settings_service.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/domain/models/audio_device.dart';

/// In-memory storage fake: serves one preloaded document and records writes.
class _FakeStorage implements LocalStorageService {
  Map<String, dynamic>? doc;
  Map<String, dynamic>? lastWrite;
  int writes = 0;

  /// When true every write fails, like a full/read-only disk.
  bool failWrites = false;

  /// When set, `read` parks on this until the test completes it — the window
  /// in which a user choice can race the fire-and-forget load().
  Completer<void>? readGate;

  @override
  Future<Map<String, dynamic>?> read(String collection, String id) async {
    final gate = readGate;
    if (gate != null) await gate.future;
    return doc;
  }

  @override
  Future<void> write(String collection, String id, Map<String, dynamic> json) {
    if (failWrites) {
      return Future.error(const FileSystemException('disk full'));
    }
    lastWrite = json;
    writes++;
    return Future.value();
  }

  @override
  Future<Directory> assetDir(String collection) =>
      throw UnimplementedError('not used by AppSettingsService');

  @override
  Future<String> assetPath(String collection) =>
      throw UnimplementedError('not used by AppSettingsService');
}

/// Engine fake that records the device UIDs pushed to it.
class _FakeEngine extends Fake implements GraphAudioPlaybackService {
  final outputCalls = <String>[];
  final inputCalls = <String>[];

  @override
  double get sampleRate => 48000.0;

  @override
  bool setOutputDevice(String uid) {
    outputCalls.add(uid);
    return true;
  }

  @override
  bool setInputDevice(String uid) {
    inputCalls.add(uid);
    return true;
  }

  @override
  void setMonitorGain(double gain) {}

  @override
  void setInputGain(double gain) {}

  @override
  void setMonitorReverb(double room, double damping, double wet) {}
}

const _builtInOut = AudioDevice(
  uid: 'built-in-out',
  name: 'MacBook Pro Speakers',
  rate: 48000,
  isDefault: true,
);
const _headphones = AudioDevice(
  uid: 'usb-hp',
  name: 'USB Headphones',
  rate: 48000,
);
const _builtInMic = AudioDevice(
  uid: 'built-in-mic',
  name: 'MacBook Pro Microphone',
  rate: 48000,
  isDefault: true,
  isInput: true,
);

void main() {
  // Mutable live device lists the injected enumerators read from, so tests can
  // simulate unplug/replug between refreshDevices() calls.
  late List<AudioDevice> outs;
  late List<AudioDevice> ins;
  late _FakeStorage storage;
  late AppSettingsService service;

  AppSettingsService build() => AppSettingsService(
    storage: storage,
    enumerateOutputs: () => List.of(outs),
    enumerateInputs: () => List.of(ins),
  );

  setUp(() {
    outs = [_builtInOut];
    ins = [_builtInMic];
    storage = _FakeStorage();
    service = build();
  });

  group('unplugged persisted device', () {
    test(
      'is reported missing but the UID and last-known name are kept',
      () async {
        storage.doc = {
          'outputDeviceUid': _headphones.uid,
          'outputDeviceName': _headphones.name,
        };
        await service.load();

        expect(service.outputDeviceMissing, isTrue);
        expect(service.outputDeviceUid, _headphones.uid); // choice preserved
        expect(service.outputDeviceName, _headphones.name);
        expect(service.inputDeviceMissing, isFalse); // no explicit input pick
      },
    );

    test(
      'monitorAllowed evaluates the effective device: missing pick is off',
      () async {
        storage.doc = {'outputDeviceUid': _headphones.uid};
        await service.load();
        // The persisted output is unplugged, so there is no known effective
        // device and the hear-yourself monitor stays at the safe default (off).
        expect(service.monitorAllowed, isFalse);

        outs = [_builtInOut, _headphones];
        service.refreshDevices();
        // Back on the (headphone-named) pick, the monitor is allowed again.
        expect(service.monitorAllowed, isTrue);
      },
    );

    test('selectedInputDevice falls back to the last-known name', () async {
      storage.doc = {
        'inputDeviceUid': 'usb-mic',
        'inputDeviceName': 'USB Microphone',
      };
      await service.load();
      final selected = service.selectedInputDevice;
      expect(selected?.uid, 'usb-mic');
      expect(selected?.name, 'USB Microphone');
    });
  });

  group('device returning after an unplug', () {
    test('switches the engine back live and keeps the persisted UID', () async {
      storage.doc = {
        'outputDeviceUid': _headphones.uid,
        'outputDeviceName': _headphones.name,
      };
      await service.load();
      final engine = _FakeEngine();
      service.attachEngine(engine);
      await Future<void>.delayed(Duration.zero); // deferred attach refresh
      engine.outputCalls.clear(); // drop the attach-time apply

      outs = [_builtInOut, _headphones];
      service.refreshDevices();

      expect(engine.outputCalls, [_headphones.uid]); // switched back live
      expect(service.outputDeviceUid, _headphones.uid); // still persisted
      expect(service.outputDeviceMissing, isFalse);
    });

    test('an unrelated hot-plug does not re-push a connected device', () async {
      storage.doc = {'inputDeviceUid': _builtInMic.uid};
      await service.load();
      final engine = _FakeEngine();
      service.attachEngine(engine);
      await Future<void>.delayed(Duration.zero);
      engine.inputCalls.clear();
      engine.outputCalls.clear();

      outs = [_builtInOut, _headphones]; // new output appears
      service.refreshDevices();

      expect(engine.inputCalls, isEmpty); // mic never left, no re-apply
      expect(engine.outputCalls, isEmpty); // no output pick to restore
    });

    test(
      'unplugging does not push anything (engine falls back natively)',
      () async {
        storage.doc = {'outputDeviceUid': _headphones.uid};
        outs = [_builtInOut, _headphones];
        await service.load();
        final engine = _FakeEngine();
        service.attachEngine(engine);
        await Future<void>.delayed(Duration.zero);
        engine.outputCalls.clear();

        outs = [_builtInOut];
        service.refreshDevices();

        expect(engine.outputCalls, isEmpty);
        expect(service.outputDeviceMissing, isTrue);
      },
    );
  });

  group('last-known device names', () {
    test(
      'are captured on selection and persisted alongside the UIDs',
      () async {
        outs = [_builtInOut, _headphones];
        await service.load();

        await service.setOutputDevice(_headphones.uid);
        expect(service.outputDeviceName, _headphones.name);
        expect(storage.lastWrite?['outputDeviceUid'], _headphones.uid);
        expect(storage.lastWrite?['outputDeviceName'], _headphones.name);

        // Back to System Default: the name is cleared too.
        await service.setOutputDevice(null);
        expect(service.outputDeviceName, isNull);
        expect(storage.lastWrite?['outputDeviceName'], isNull);
      },
    );

    test('follow a rename of a connected persisted device', () async {
      storage.doc = {
        'outputDeviceUid': _headphones.uid,
        'outputDeviceName': 'Old Name',
      };
      outs = [_builtInOut, _headphones];
      await service.load();

      expect(service.outputDeviceName, _headphones.name);
      // The refreshed name was persisted (fire-and-forget).
      await Future<void>.delayed(Duration.zero);
      expect(storage.lastWrite?['outputDeviceName'], _headphones.name);
    });

    test('a failing fire-and-forget name persist is swallowed, not rethrown '
        'into the root zone', () async {
      storage.doc = {
        'outputDeviceUid': _headphones.uid,
        'outputDeviceName': 'Old Name',
      };
      storage.failWrites = true;
      outs = [_builtInOut, _headphones];
      await service.load();

      // Let the discarded persist future settle. Before the error handler
      // was attached, its I/O failure surfaced as an unhandled async error
      // in the test zone and this test would fail.
      await Future<void>.delayed(Duration.zero);
      // The in-memory rename still took effect; only persistence degraded.
      expect(service.outputDeviceName, _headphones.name);
    });
  });

  group('load racing a live user choice', () {
    test('a choice made while load() is in flight survives it', () async {
      // load() is started and its future dropped by the provider graph, so it
      // can resolve after the singer has already picked a device or moved a
      // fader. Adopting the document wholesale reverted the choice on screen,
      // re-pushed the stale value to the engine, and then persisted it back.
      storage.doc = {
        'inputDeviceUid': 'stale-mic',
        'inputDeviceName': 'Stale Mic',
        'monitorGain': 0.9,
        'recordGainDb': 3.0,
      };
      final gate = Completer<void>();
      storage.readGate = gate;
      final engine = _FakeEngine();
      service.attachEngine(engine);

      final loading = service.load(); // parked inside read()
      await service.setInputDevice(_builtInMic.uid);
      service.setMonitorGainLive(0.25);
      gate.complete();
      await loading;

      expect(service.inputDeviceUid, _builtInMic.uid);
      expect(service.inputDeviceName, _builtInMic.name);
      expect(service.monitorGain, 0.25);
      // Untouched fields still adopt the document.
      expect(service.recordGainDb, 3.0);
      // And the user's value is what gets persisted next, not the document's.
      await service.commitAudioGains();
      expect(storage.lastWrite!['monitorGain'], 0.25);
      expect(storage.lastWrite!['inputDeviceUid'], _builtInMic.uid);
    });

    test('one wrong-typed field costs only that field, not the engine '
        'apply', () async {
      // The reads used to sit outside the try, so a single bad entry threw a
      // TypeError past the fire-and-forget future: the device list stayed
      // empty and monitoring was permanently unavailable with nothing on
      // screen to say why.
      storage.doc = {
        'outputDeviceUid': 42, // wrong type
        'inputDeviceUid': _builtInMic.uid,
        'monitorGain': 'loud', // wrong type
        'recordGainDb': 7.0,
      };
      final engine = _FakeEngine();
      service.attachEngine(engine);
      engine.inputCalls.clear();

      await service.load();

      expect(service.outputDeviceUid, isNull, reason: 'bad field -> default');
      expect(service.monitorGain, 0.0, reason: 'bad field -> default');
      expect(service.inputDeviceUid, _builtInMic.uid, reason: 'good field');
      expect(service.recordGainDb, 7.0, reason: 'good field');
      // The apply still ran: the engine received the loaded input device.
      expect(engine.inputCalls, contains(_builtInMic.uid));
      expect(service.inputDevices, isNotEmpty, reason: 'refreshDevices ran');
    });

    test('a non-finite stored gain falls back instead of clamping to the '
        'maximum', () async {
      // num.clamp resolves NaN/Infinity through compareTo and returns the
      // range MAXIMUM, so a corrupt value would arrive as full-scale gain.
      storage.doc = {
        'recordGainDb': double.infinity,
        'monitorGain': double.nan,
      };
      await service.load();

      expect(service.recordGainDb, 12.0);
      expect(service.monitorGain, 0.0);
    });
  });
}
