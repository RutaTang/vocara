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
  final monitorGains = <double>[];

  /// What the engine reports it is RENDERING to — which is not the selection while the duplex
  /// mic unit owns the output. '' = nothing open / unknown.
  String actualOutput = '';

  @override
  double get sampleRate => 48000.0;

  @override
  String actualOutputDevice() => actualOutput;

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
  void setMonitorGain(double gain) => monitorGains.add(gain);

  /// Every input gain pushed (linear), in order.
  final inputGainCalls = <double>[];

  @override
  void setInputGain(double gain) => inputGainCalls.add(gain);

  @override
  void setMonitorReverb(double room, double damping, double wet) {}

  /// Every input-channel mode pushed, in order.
  final inputChannelCalls = <int>[];

  @override
  void setInputChannel(int mode) => inputChannelCalls.add(mode);
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
const _interface = AudioDevice(
  uid: 'usb-2in',
  name: 'USB Audio Interface',
  rate: 48000,
  isInput: true,
);
const _otherInterface = AudioDevice(
  uid: 'usb-other',
  name: 'Another Interface',
  rate: 48000,
  isInput: true,
);

void main() {
  // Mutable live device lists the injected enumerators read from, so tests can
  // simulate unplug/replug between refreshDevices() calls.
  late List<AudioDevice> outs;
  late List<AudioDevice> ins;
  late _FakeStorage storage;
  late AppSettingsService service;

  // Input channel counts by device uid ('' = the system default input).
  late Map<String, int> channels;

  AppSettingsService build() => AppSettingsService(
    storage: storage,
    enumerateOutputs: () => List.of(outs),
    enumerateInputs: () => List.of(ins),
    inputChannelCount: (uid) => channels[uid] ?? 0,
  );

  setUp(() {
    outs = [_builtInOut];
    ins = [_builtInMic];
    channels = {'': 1, _builtInMic.uid: 1, _interface.uid: 2};
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

    test('monitorAllowed follows what the engine RENDERS to, not the pick', () async {
      // The scenario the selection-based test cannot see: headphones are selected and present,
      // but opening the mic hands the output to the duplex unit, which renders through the
      // microphone's endpoint or the system default — here, the built-in speakers. Judging the
      // selection would leave the monitor up and howling into them.
      outs = [_builtInOut, _headphones];
      storage.doc = {'outputDeviceUid': _headphones.uid};
      await service.load();
      final engine = _FakeEngine();
      service.attachEngine(engine);
      expect(service.monitorAllowed, isTrue, reason: 'on the chosen device');

      engine.actualOutput = _builtInOut.uid;
      engine.monitorGains.clear();
      service.reconcileMonitorSafety();
      expect(service.monitorAllowed, isFalse);
      expect(engine.monitorGains, [0.0]);

      // Back on the chosen device, the fader is honoured again.
      engine.actualOutput = _headphones.uid;
      service.reconcileMonitorSafety();
      expect(service.monitorAllowed, isTrue);
    });

    test('a Bluetooth speaker never allows the monitor', () async {
      // macOS reports only the transport: a Bluetooth speaker taken for
      // headphones fed the monitor straight back into the mic.
      await service.load();
      Future<bool> allowedOn(String name, {bool bluetooth = true}) async {
        final device = AudioDevice(
          uid: 'bt-$name',
          name: name,
          rate: 48000,
          isBluetooth: bluetooth,
        );
        outs = [_builtInOut, device];
        service.refreshDevices();
        await service.setOutputDevice(device.uid);
        return service.monitorAllowed;
      }

      for (final speaker in [
        'UE BOOM 3',
        'JBL Flip 6',
        'Living Room Speaker',
        'Sonos Roam',
        'Bose SoundLink Flex',
      ]) {
        expect(await allowedOn(speaker), isFalse, reason: speaker);
      }
      for (final headphones in ['AirPods Pro', 'WH-1000XM5', 'Bose QC45']) {
        expect(await allowedOn(headphones), isTrue, reason: headphones);
      }
      expect(
        await allowedOn('MacBook Pro Speakers', bluetooth: false),
        isFalse,
        reason: 'built-in speakers stay off',
      );
    });

    test(
      'an unenumerable rendering device falls back to judging the selection',
      () async {
        // Aggregate/virtual devices and uid-format drift can name something the panel never
        // enumerates. Refusing to judge at all would mute a legitimate headphone monitor with
        // nothing on screen to explain it, so this degrades to the pre-existing behaviour.
        outs = [_builtInOut, _headphones];
        storage.doc = {'outputDeviceUid': _headphones.uid};
        await service.load();
        final engine = _FakeEngine()..actualOutput = 'aggregate-42';
        service.attachEngine(engine);
        service.reconcileMonitorSafety();
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

  group('a write the disk refuses', () {
    // Every setter is wired into the panel as a `void` callback, so the Future
    // it returns is discarded by the UI. The failure therefore has to live in
    // observable state, not in that Future: otherwise it lands in the root
    // zone as a spurious crash report while the panel shows a value the disk
    // never took.
    test(
      'an awaited setter neither throws nor leaks, and reports the failure',
      () async {
        storage.failWrites = true;
        var notified = 0;
        service.addListener(() => notified++);

        // Called exactly as the panel calls it: through a void-typed callback,
        // so the Future it returns is dropped on the floor.
        final void Function(String?) callback = service.setOutputDevice;
        callback(_headphones.uid);
        await Future<void>.delayed(Duration.zero);

        expect(service.persistFailed, isTrue);
        expect(notified, greaterThan(0)); // the panel gets to render the banner
        // The choice is live regardless — only its durability degraded.
        expect(service.outputDeviceUid, _headphones.uid);
      },
    );

    test('the failure flag clears once a write lands again', () async {
      storage.failWrites = true;
      await service.setInputDevice(_builtInMic.uid);
      expect(service.persistFailed, isTrue);

      storage.failWrites = false;
      await service.commitAudioGains();
      expect(service.persistFailed, isFalse);
      expect(storage.lastWrite?['inputDeviceUid'], _builtInMic.uid);
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

    test('a choice persisted while load() is in flight does not clobber the '
        'untouched fields on disk', () async {
      // _persist serializes all seven fields from memory. Run while load()
      // was parked in read(), it wrote the constructor defaults for every
      // field the user had not touched, and load() never wrote again — the
      // next launch lost the saved output device and gains.
      outs = [_builtInOut, _headphones];
      storage.doc = {
        'outputDeviceUid': _headphones.uid,
        'outputDeviceName': _headphones.name,
        'recordGainDb': 3.0,
        'monitorGain': 0.5,
      };
      final gate = Completer<void>();
      storage.readGate = gate;

      final loading = service.load(); // parked inside read()
      await service.setInputDevice(_builtInMic.uid);
      // Nothing may race the read: on a real disk it could read back the
      // very defaults the racing write put there.
      expect(storage.writes, 0);
      gate.complete();
      await loading;

      expect(storage.lastWrite!['inputDeviceUid'], _builtInMic.uid);
      expect(storage.lastWrite!['outputDeviceUid'], _headphones.uid);
      expect(storage.lastWrite!['outputDeviceName'], _headphones.name);
      expect(storage.lastWrite!['recordGainDb'], 3.0);
      expect(storage.lastWrite!['monitorGain'], 0.5);
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

  group('input channel', () {
    test('is chosen per input device and persisted with it', () async {
      ins = [_builtInMic, _interface];
      await service.load();
      expect(service.inputChannel, GraphAudioPlaybackService.inputChannelAuto);

      await service.setInputDevice(_interface.uid);
      await service.setInputChannel(GraphAudioPlaybackService.inputChannel2);
      expect(service.inputChannel, GraphAudioPlaybackService.inputChannel2);

      // Another device has its own choice, Auto until made.
      await service.setInputDevice(_builtInMic.uid);
      expect(service.inputChannel, GraphAudioPlaybackService.inputChannelAuto);
      await service.setInputChannel(GraphAudioPlaybackService.inputChannel1);

      await service.setInputDevice(_interface.uid);
      expect(service.inputChannel, GraphAudioPlaybackService.inputChannel2);
      expect(storage.lastWrite?['inputChannelByUid'], {
        _interface.uid: 2,
        _builtInMic.uid: 1,
      });

      // A fresh start reads both back.
      storage.doc = storage.lastWrite;
      final restarted = build();
      await restarted.load();
      expect(restarted.inputChannel, GraphAudioPlaybackService.inputChannel2);
      await restarted.setInputDevice(_builtInMic.uid);
      expect(restarted.inputChannel, GraphAudioPlaybackService.inputChannel1);
    });

    test('reaches the engine on attach, on a change, and with the input '
        'device', () async {
      ins = [_builtInMic, _interface];
      storage.doc = {
        'inputDeviceUid': _interface.uid,
        'inputChannelByUid': {_interface.uid: 2},
      };
      await service.load();

      final engine = _FakeEngine();
      service.attachEngine(engine);
      expect(engine.inputChannelCalls, [2], reason: 'applied on attach');

      await service.setInputChannel(GraphAudioPlaybackService.inputChannel1);
      expect(engine.inputChannelCalls.last, 1, reason: 'applied on change');

      // Switching the input device applies THAT device's choice.
      await service.setInputDevice(_builtInMic.uid);
      expect(engine.inputChannelCalls.last, 0);
      expect(engine.inputCalls.last, _builtInMic.uid);
    });

    test('the channel count follows the current input', () async {
      ins = [_builtInMic, _interface];
      await service.load();
      expect(service.inputChannelCount, 1, reason: 'the system default mic');
      await service.setInputDevice(_interface.uid);
      expect(service.inputChannelCount, 2);
      // A device that changes its format is seen on the next device poll.
      channels[_interface.uid] = 1;
      service.refreshDevices();
      expect(service.inputChannelCount, 1);
    });

    test('a corrupt stored choice is ignored, not applied', () async {
      ins = [_builtInMic, _interface, _otherInterface];
      storage.doc = {
        'inputDeviceUid': _interface.uid,
        'inputChannelByUid': {
          _interface.uid: 7,
          _builtInMic.uid: 'two',
          _otherInterface.uid: 1,
        },
      };
      await service.load();
      expect(service.inputChannel, GraphAudioPlaybackService.inputChannelAuto);
      final engine = _FakeEngine();
      service.attachEngine(engine);
      expect(engine.inputChannelCalls, [0], reason: 'the bad value never ran');
      // One bad entry costs only that device's choice.
      await service.setInputDevice(_otherInterface.uid);
      expect(service.inputChannel, GraphAudioPlaybackService.inputChannel1);
    });

    test("the technique page's own engine gets the input's device and "
        'channel', () async {
      // It is never attached here; a new engine starts on Auto, and its takes
      // blended in the input the singer had picked to keep out.
      ins = [_builtInMic, _interface];
      storage.doc = {
        'inputDeviceUid': _interface.uid,
        'inputChannelByUid': {_interface.uid: 1},
      };
      await service.load();
      final engine = _FakeEngine();
      service.applyInputTo(engine);
      expect(engine.inputCalls, [_interface.uid]);
      expect(engine.inputChannelCalls, [1]);
      expect(
        engine.inputGainCalls,
        hasLength(1),
        reason: 'and the record gain',
      );
    });

    test('the technique engine is keyed on an interface plugged in since the '
        'last poll', () async {
      // Nothing polls the devices outside Settings: read off the list from
      // launch, the plugged-in interface was missing and its takes ran on the
      // built-in mic's choice.
      ins = [_builtInMic];
      storage.doc = {
        'inputDeviceUid': _interface.uid,
        'inputChannelByUid': {_interface.uid: 1},
        'recordGainDb': 0.0,
      };
      await service.load();
      ins = [_builtInMic, _interface]; // plugged in; no poll since
      final engine = _FakeEngine();
      service.applyInputTo(engine);
      expect(engine.inputChannelCalls, [1]);
      expect(engine.inputGainCalls.single, closeTo(1.0, 1e-9));
    });

    test(
      'a take start re-keys the channel without switching devices',
      () async {
        // Doing the full device refresh there also switched a returned device
        // back synchronously — re-opening the mic, or blocking on the output
        // host — right as the take armed. That stays the device poll's.
        ins = [_builtInMic];
        storage.doc = {
          'inputDeviceUid': _interface.uid,
          'inputChannelByUid': {_interface.uid: 2},
        };
        await service.load();
        final engine = _FakeEngine();
        service.attachEngine(engine);
        engine.inputCalls.clear();
        ins = [_builtInMic, _interface]; // plugged back in

        service.syncInputChannel();
        expect(engine.inputChannelCalls.last, 2, reason: "the interface's");
        expect(engine.inputCalls, isEmpty, reason: 'no device switch');

        service.refreshDevices(); // the poll still switches it back
        expect(engine.inputCalls, [_interface.uid]);
      },
    );

    test(
      'System Default keeps each default device on its own choice',
      () async {
        // Keyed by the selection, a choice made while one interface was the
        // default followed the default onto the next.
        const builtInNotDefault = AudioDevice(
          uid: 'built-in-mic',
          name: 'MacBook Pro Microphone',
          rate: 48000,
          isInput: true,
        );
        const interfaceDefault = AudioDevice(
          uid: 'usb-2in',
          name: 'USB Audio Interface',
          rate: 48000,
          isDefault: true,
          isInput: true,
        );
        ins = [_builtInMic, _interface];
        await service.load();
        final engine = _FakeEngine();
        service.attachEngine(engine);
        channels[_builtInMic.uid] = 2;
        await service.setInputChannel(GraphAudioPlaybackService.inputChannel2);

        ins = [builtInNotDefault, interfaceDefault];
        service.refreshDevices();
        expect(
          service.inputChannel,
          GraphAudioPlaybackService.inputChannelAuto,
        );
        expect(engine.inputChannelCalls.last, 0, reason: 'the new default');
        expect(service.inputChannelCount, 2, reason: "the new default's");

        ins = [_builtInMic, _interface];
        service.refreshDevices();
        expect(service.inputChannel, GraphAudioPlaybackService.inputChannel2);
        expect(engine.inputChannelCalls.last, 2);
      },
    );

    test('an unplugged choice gives way to the device the engine falls back '
        'to', () async {
      ins = [_builtInMic, _interface];
      storage.doc = {
        'inputDeviceUid': _interface.uid,
        'inputChannelByUid': {_interface.uid: 2},
      };
      await service.load();
      final engine = _FakeEngine();
      service.attachEngine(engine);
      expect(engine.inputChannelCalls.last, 2);

      ins = [_builtInMic];
      service.refreshDevices();
      expect(service.inputChannel, GraphAudioPlaybackService.inputChannelAuto);
      expect(engine.inputChannelCalls.last, 0, reason: 'the fallback mic');
      expect(service.inputChannelCount, 1, reason: 'the fallback mic');

      ins = [_builtInMic, _interface];
      service.refreshDevices();
      expect(service.inputChannel, GraphAudioPlaybackService.inputChannel2);
      expect(engine.inputChannelCalls.last, 2, reason: 'it came back');
    });
  });
}
