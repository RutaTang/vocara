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

  @override
  Future<Map<String, dynamic>?> read(String collection, String id) async => doc;

  @override
  Future<void> write(String collection, String id, Map<String, dynamic> json) {
    lastWrite = json;
    writes++;
    return Future.value();
  }

  @override
  Future<Directory> assetDir(String collection) =>
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
  });
}
