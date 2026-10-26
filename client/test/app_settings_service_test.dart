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

  /// Every output the settings named echo-free ('' = none), in call order.
  final List<String> echoFree = [];

  @override
  void setEchoFree(String uid) => echoFree.add(uid);

  /// Every input gain pushed (linear), in order.
  final inputGainCalls = <double>[];

  @override
  void setInputGain(double gain) => inputGainCalls.add(gain);

  @override
  void setMonitorReverb(double room, double damping, double wet) {}

  /// The input stream state word and the device it is bound to (see the engine's).
  int streamState = 0;
  String boundInput = '';

  @override
  int inputStreamState() => streamState;

  @override
  String actualInputDevice() => boundInput;

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

    test(
      'the echo canceller is told only the device the engine RENDERS to',
      () async {
        // On earbuds the mic hears none of the backing the canceller is handed: it adapted into
        // the in-key voice. Judged on the device actually rendering, never the selection.
        const airpods = AudioDevice(
          uid: 'airpods',
          name: 'AirPods Pro',
          rate: 48000,
          isBluetooth: true,
        );
        outs = [_builtInOut, airpods];
        storage.doc = {'outputDeviceUid': airpods.uid};
        await service.load();
        final engine = _FakeEngine();
        service.attachEngine(engine);
        expect(
          engine.echoFree.last,
          '',
          reason:
              'the engine reports nothing yet: the selection is not evidence',
        );

        engine.actualOutput = airpods.uid;
        service.reconcileMonitorSafety();
        expect(engine.echoFree.last, airpods.uid);

        engine.actualOutput = _builtInOut.uid;
        service.reconcileMonitorSafety();
        expect(engine.echoFree.last, '', reason: 'the speakers carry an echo');
      },
    );

    test('only a product named as earbuds is echo-free', () async {
      // A speaker taken for headphones would leave its whole echo in every take: not a device
      // merely on Bluetooth, and not a headphone PORT — "External Headphones" is the jack of an
      // Apple-silicon Mac whatever is plugged into it, desk speakers included.
      await service.load();
      final engine = _FakeEngine();
      service.attachEngine(engine);
      Future<String> echoFreeOn(String name, {bool bluetooth = true}) async {
        final device = AudioDevice(
          uid: 'out-$name',
          name: name,
          rate: 48000,
          isBluetooth: bluetooth,
        );
        outs = [_builtInOut, device];
        service.refreshDevices();
        engine.actualOutput = device.uid;
        await service.setOutputDevice(device.uid);
        return engine.echoFree.last;
      }

      for (final name in [
        'AirPods Pro',
        'AirPods Max',
        'Galaxy Buds2',
        'Beats Studio Buds',
      ]) {
        expect(await echoFreeOn(name), 'out-$name', reason: name);
      }
      expect(
        await echoFreeOn('EarPods (USB-C)', bluetooth: false),
        'out-EarPods (USB-C)',
      );
      for (final name in ['WH-1000XM5', 'UE BOOM 3', 'Speaker Buds Box']) {
        expect(await echoFreeOn(name), '', reason: name);
      }
      for (final port in [
        'External Headphones',
        'USB-C to 3.5mm Headphone Jack Adapter',
        'MacBook Pro Speakers',
      ]) {
        expect(await echoFreeOn(port, bluetooth: false), '', reason: port);
      }
    });

    test('a speaker the default moves to is judged as itself, not as the '
        'selection', () async {
      // The page enumerated with earbuds as the default; a Bluetooth speaker turned on since
      // became the default and the engine follows it. Judged against the stale list, it fell
      // back to the selection — the earbuds — and the monitor and the canceller both took it
      // for headphones.
      const airpods = AudioDevice(
        uid: 'airpods',
        name: 'AirPods Pro',
        rate: 48000,
        isDefault: true,
        isBluetooth: true,
      );
      const speakers = AudioDevice(
        uid: 'built-in-out',
        name: 'MacBook Pro Speakers',
        rate: 48000,
      );
      outs = [speakers, airpods];
      await service.load();
      final engine = _FakeEngine()..actualOutput = airpods.uid;
      service.attachEngine(engine);
      service.setMonitorGainLive(0.5);
      expect(service.monitorAllowed, isTrue);
      expect(engine.echoFree.last, airpods.uid);

      const boom = AudioDevice(
        uid: 'boom',
        name: 'UE BOOM 3',
        rate: 48000,
        isDefault: true,
        isBluetooth: true,
      );
      outs = [
        speakers,
        const AudioDevice(
          uid: 'airpods',
          name: 'AirPods Pro',
          rate: 48000,
          isBluetooth: true,
        ),
        boom,
      ];
      engine.actualOutput = boom.uid;
      engine.monitorGains.clear();
      service.reconcileMonitorSafety();
      expect(service.monitorAllowed, isFalse);
      expect(engine.monitorGains.last, 0.0);
      expect(engine.echoFree.last, '');
    });

    test('the 1 s monitor tick never switches the mic', () async {
      // The chosen USB mic was unplugged when the page opened, so the engine records from the
      // built-in one; it is plugged back in, unseen (Settings is closed). Mid-take the default
      // output moves to earbuds the lists do not know either. Judging them by re-reading the
      // lists ran refreshDevices' switches on the tick: the mic was reopened on the USB device in
      // the middle of the take.
      const usbMic = AudioDevice(
        uid: 'usb-mic',
        name: 'USB Microphone',
        rate: 48000,
        isInput: true,
      );
      storage.doc = {'inputDeviceUid': usbMic.uid};
      await service.load();
      final engine = _FakeEngine()..actualOutput = _builtInOut.uid;
      service.attachEngine(engine);
      await Future<void>.delayed(Duration.zero);
      ins = [_builtInMic, usbMic];
      engine.inputCalls.clear();
      engine.inputChannelCalls.clear();
      outs = [
        const AudioDevice(
          uid: 'built-in-out',
          name: 'MacBook Pro Speakers',
          rate: 48000,
        ),
        const AudioDevice(
          uid: 'airpods',
          name: 'AirPods Pro',
          rate: 48000,
          isDefault: true,
          isBluetooth: true,
        ),
      ];
      engine.actualOutput = 'airpods';
      service.reconcileMonitorSafety();
      expect(engine.inputCalls, isEmpty);
      expect(engine.inputChannelCalls, isEmpty);
      // The earbuds are still judged as themselves.
      expect(engine.echoFree.last, 'airpods');
      expect(service.monitorAllowed, isTrue);
    });

    test(
      'the route is named to the engine on a change, not every tick',
      () async {
        await service.load();
        final engine = _FakeEngine()..actualOutput = _builtInOut.uid;
        service.attachEngine(engine);
        final named = engine.echoFree.length;
        for (var i = 0; i < 5; i++) {
          service.reconcileMonitorSafety();
        }
        expect(engine.echoFree.length, named);
        // A new engine is always told.
        final next = _FakeEngine()..actualOutput = _builtInOut.uid;
        service.attachEngine(next);
        expect(next.echoFree, ['']);
      },
    );

    test('a tick while the mic opens keeps the earbuds named', () async {
      // Opening the mic hands the output to the duplex unit, and the engine reports nothing
      // rendering until it binds. Taken as "not headphones", that withdrew the earbuds: once the
      // unit bound, the canceller adapted on them until the next tick named them again.
      const airpods = AudioDevice(
        uid: 'airpods',
        name: 'AirPods Pro',
        rate: 48000,
        isDefault: true,
        isBluetooth: true,
      );
      outs = [
        const AudioDevice(
          uid: 'built-in-out',
          name: 'MacBook Pro Speakers',
          rate: 48000,
        ),
        airpods,
      ];
      await service.load();
      final engine = _FakeEngine()..actualOutput = airpods.uid;
      service.attachEngine(engine);
      expect(engine.echoFree, [airpods.uid]);
      engine.actualOutput = '';
      service.reconcileMonitorSafety();
      engine.actualOutput = airpods.uid;
      service.reconcileMonitorSafety();
      expect(engine.echoFree, [airpods.uid]);
      // A new engine reporting nothing yet is still told something.
      final next = _FakeEngine();
      service.attachEngine(next);
      expect(next.echoFree, ['']);
    });

    test(
      'the MON fader hears of a rendering device resolved on a later tick',
      () async {
        // The default moved to a speaker turned on since the lists were read, and the first fresh
        // read did not list it yet: the selection (the earbuds) was judged meanwhile. When the
        // next read found the speaker, the engine's gain went to 0 but the fader was never told.
        const airpods = AudioDevice(
          uid: 'airpods',
          name: 'AirPods Pro',
          rate: 48000,
          isDefault: true,
          isBluetooth: true,
        );
        outs = [airpods];
        await service.load();
        final engine = _FakeEngine()..actualOutput = airpods.uid;
        service.attachEngine(engine);
        service.setMonitorGainLive(0.5);
        engine.actualOutput = 'boom';
        service.reconcileMonitorSafety();
        expect(service.monitorAllowed, isTrue);
        outs = [
          airpods,
          const AudioDevice(
            uid: 'boom',
            name: 'UE BOOM 3',
            rate: 48000,
            isBluetooth: true,
          ),
        ];
        var notified = 0;
        service.addListener(() => notified++);
        service.reconcileMonitorSafety();
        expect(service.monitorAllowed, isFalse);
        expect(engine.monitorGains.last, 0.0);
        expect(notified, 1);
        // Nothing new on the next tick: no notify.
        service.reconcileMonitorSafety();
        expect(notified, 1);
      },
    );

    test('a device list that cannot be read judges the monitor unsafe', () async {
      // System Default on the earbuds; the default moves to a speaker the lists never saw, and
      // the fresh read fails (an empty list). The engine's mute covers a pick it is not
      // rendering, not a default that moves, so judging the selection here fed the monitor into
      // the speaker for as long as the reads kept failing.
      const airpods = AudioDevice(
        uid: 'airpods',
        name: 'AirPods Pro',
        rate: 48000,
        isDefault: true,
        isBluetooth: true,
      );
      outs = [airpods];
      await service.load();
      final engine = _FakeEngine()..actualOutput = airpods.uid;
      service.attachEngine(engine);
      service.setMonitorGainLive(0.5);
      var notified = 0;
      service.addListener(() => notified++);
      outs = [];
      engine.actualOutput = 'boom';
      service.reconcileMonitorSafety();
      expect(service.monitorAllowed, isFalse);
      expect(engine.monitorGains.last, 0.0);
      expect(notified, 1);
      // The read recovers without the device (one the panel can never list): back to judging
      // the selection, as for any unenumerable device, and the fader is told.
      outs = [airpods];
      service.reconcileMonitorSafety();
      expect(service.monitorAllowed, isTrue);
      expect(notified, 2);
    });

    test('a Bluetooth speaker never allows the monitor', () async {
      // A Bluetooth speaker taken for headphones fed the monitor straight back
      // into the mic. Bluetooth alone allows nothing: only a name that says
      // headphones does, or the user's mark.
      await service.load();
      Future<bool> allowedOn(String name, {bool bluetooth = true}) async {
        final device = AudioDevice(
          uid: 'bt-$name',
          name: name,
          rate: 48000,
          isBluetooth: bluetooth,
          transport: bluetooth
              ? AudioTransport.bluetooth
              : AudioTransport.builtIn,
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
      for (final headphones in [
        'AirPods Pro',
        'Galaxy Buds2',
        'Sony Headphones',
      ]) {
        expect(await allowedOn(headphones), isTrue, reason: headphones);
      }
      // Headphones whose name does not say so: the user's to mark.
      for (final unnamed in ['WH-1000XM5', 'Bose QC45', 'Marshall Emberton']) {
        expect(await allowedOn(unnamed), isFalse, reason: unnamed);
      }
      expect(await allowedOn('WH-1000XM5'), isFalse);
      await service.setOutputIsHeadphones('bt-WH-1000XM5', true);
      expect(service.monitorAllowed, isTrue);
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

  group('headphones mark', () {
    // Wired headphones on an audio interface: no name can tell them from monitors.
    const scarlett = AudioDevice(
      uid: 'scarlett-out',
      name: 'Scarlett 2i2 USB',
      rate: 48000,
      isDefault: true,
      transport: AudioTransport.usb,
    );
    const speakers = AudioDevice(
      uid: 'built-in-out',
      name: 'MacBook Pro Speakers',
      rate: 48000,
      transport: AudioTransport.builtIn,
    );

    test('a marked output is echo-free and allows the monitor', () async {
      outs = [speakers, scarlett];
      await service.load();
      final engine = _FakeEngine()..actualOutput = scarlett.uid;
      service.attachEngine(engine);
      service.setMonitorGainLive(0.5);
      expect(service.outputMarkable, isTrue);
      expect(service.outputToMark?.uid, scarlett.uid);
      expect(service.monitorAllowed, isFalse);
      expect(engine.echoFree.last, '');

      await service.setOutputIsHeadphones(scarlett.uid, true);
      expect(service.outputMarkedHeadphones, isTrue);
      expect(service.monitorAllowed, isTrue);
      expect(engine.monitorGains.last, 0.5);
      expect(engine.echoFree.last, scarlett.uid);
      expect(storage.lastWrite!['headphoneOutputUids'], [scarlett.uid]);

      // The mark is the device's: the speakers taking over are judged as themselves.
      engine.actualOutput = speakers.uid;
      service.reconcileMonitorSafety();
      expect(service.monitorAllowed, isFalse);
      expect(engine.echoFree.last, '');

      engine.actualOutput = scarlett.uid;
      service.reconcileMonitorSafety();
      expect(engine.echoFree.last, scarlett.uid);
      await service.setOutputIsHeadphones(scarlett.uid, false);
      expect(service.monitorAllowed, isFalse);
      expect(engine.monitorGains.last, 0.0);
      expect(engine.echoFree.last, '');
      expect(storage.lastWrite!['headphoneOutputUids'], isEmpty);
    });

    test(
      'only a device of its own transport takes a mark, whatever its name',
      () async {
        // The system translates device names, so no name rule can tell a port or a sink with
        // speakers of its own: on a German Mac the built-in speakers and an Intel Mac's one
        // device for speakers AND jack passed every English check.
        Future<bool> markable(String name, AudioTransport transport) async {
          outs = [
            AudioDevice(
              uid: 'out-$name',
              name: name,
              rate: 48000,
              isDefault: true,
              isBluetooth: transport == AudioTransport.bluetooth,
              transport: transport,
            ),
          ];
          service.refreshDevices();
          return service.outputMarkable;
        }

        await service.load();
        for (final (name, transport) in [
          ('MacBook Pro Speakers', AudioTransport.builtIn),
          ('MacBook Pro-Lautsprecher', AudioTransport.builtIn),
          ('Haut-parleurs MacBook Pro', AudioTransport.builtIn),
          ('Integrierte Ausgabe', AudioTransport.builtIn),
          ('Built-in Output', AudioTransport.builtIn),
          ('External Headphones', AudioTransport.builtIn),
          // Displays' own speakers come in over USB as often as not.
          ('LG UltraFine Display Audio', AudioTransport.usb),
          ('Display Audio', AudioTransport.usb),
          ('Studio Display Speakers', AudioTransport.usb),
          ('Studio Display', AudioTransport.displayPort),
          ('Living Room TV', AudioTransport.network),
          ('BlackHole 2ch', AudioTransport.virtual),
          ('Multi-Output Device', AudioTransport.aggregate),
          ('Unknown Device', AudioTransport.unknown),
          ('UE BOOM 3', AudioTransport.bluetooth),
          ('AirPods Pro', AudioTransport.bluetooth),
        ]) {
          expect(await markable(name, transport), isFalse, reason: name);
        }
        for (final (name, transport) in [
          ('Scarlett 2i2 USB', AudioTransport.usb),
          ('WH-1000XM5', AudioTransport.bluetooth),
          ('Apollo Twin', AudioTransport.thunderbolt),
        ]) {
          expect(await markable(name, transport), isTrue, reason: name);
        }
      },
    );

    test(
      'the headphone jack allows the monitor in any language, but takes no mark',
      () async {
        // Its uid, unlike its name, is never translated. No mark: it can't tell headphones from
        // desk speakers plugged into it, and a mark kept for it would one day apply to those.
        for (final name in [
          'External Headphones',
          'Externe Kopfhörer',
          '外部ヘッドフォン',
        ]) {
          outs = [
            AudioDevice(
              uid: 'BuiltInHeadphoneOutputDevice',
              name: name,
              rate: 48000,
              isDefault: true,
              transport: AudioTransport.builtIn,
            ),
          ];
          await service.load();
          final engine = _FakeEngine()
            ..actualOutput = 'BuiltInHeadphoneOutputDevice';
          service.attachEngine(engine);
          expect(service.monitorAllowed, isTrue, reason: name);
          expect(service.outputMarkable, isFalse, reason: name);
          expect(engine.echoFree.last, '', reason: name);
          service.detachEngine(engine);
        }
      },
    );

    test(
      'a locked monitor points to the mark only for the output Settings will show',
      () async {
        // The mic unit moved playback to a USB headset while the selection (the default) is the
        // speakers: Settings would show no card, so the fader must not send the user there.
        const headset = AudioDevice(
          uid: 'usb-headset',
          name: 'USB Headset',
          rate: 48000,
          transport: AudioTransport.usb,
        );
        outs = [
          const AudioDevice(
            uid: 'built-in-out',
            name: 'MacBook Pro Speakers',
            rate: 48000,
            isDefault: true,
            transport: AudioTransport.builtIn,
          ),
          headset,
        ];
        await service.load();
        final engine = _FakeEngine()..actualOutput = headset.uid;
        service.attachEngine(engine);
        expect(service.outputMarkable, isTrue);
        expect(service.suggestHeadphonesMark, isFalse);
        // On the chosen output itself, it does.
        await service.setOutputDevice(headset.uid);
        expect(service.suggestHeadphonesMark, isTrue);
      },
    );

    test(
      'an unplugged mic pick is judged as the default it falls back to',
      () async {
        // The engine records from the system default while the chosen mic is
        // unplugged; the check read the missing pick as no device and warned of
        // nothing, while the take recorded over the headset's call mode.
        final airpods = AudioDevice(
          uid: 'airpods',
          name: 'AirPods Pro',
          rate: 48000,
          isDefault: true,
          isBluetooth: true,
          transport: AudioTransport.bluetooth,
        );
        outs = [airpods];
        ins = [
          AudioDevice(
            uid: 'airpods',
            name: 'AirPods Pro',
            rate: 48000,
            isDefault: true,
            isInput: true,
            isBluetooth: true,
            transport: AudioTransport.bluetooth,
          ),
        ];
        storage.doc = {
          'inputDeviceUid': 'usb-mic',
          'inputDeviceName': 'USB Mic',
        };
        await service.load();
        expect(service.inputDeviceMissing, isTrue);
        expect(service.bluetoothInOutConflict, isTrue);
      },
    );

    test('the check at REC reads the devices as they are now', () async {
      // The lists are polled only while Settings is open: judged by the ones
      // from when the song opened, a headset connected since went unwarned.
      AudioDevice bt({bool input = false}) => AudioDevice(
        uid: 'airpods',
        name: 'AirPods Pro',
        rate: 48000,
        isDefault: true,
        isInput: input,
        isBluetooth: true,
        transport: AudioTransport.bluetooth,
      );
      await service.load(); // built-in speakers and mic
      expect(service.bluetoothInOutConflictNow(), isFalse);
      outs = [bt()];
      ins = [bt(input: true)];
      expect(service.bluetoothInOutConflict, isFalse, reason: 'stale lists');
      expect(service.bluetoothInOutConflictNow(), isTrue);
      outs = [_builtInOut];
      ins = [_builtInMic];
      expect(service.bluetoothInOutConflictNow(), isFalse);
    });

    test('a Bluetooth mic warns whatever output is picked', () async {
      // A take plays through the duplex mic unit, which renders to the mic's
      // own device: with the speakers picked as output, the AirPods mic still
      // put the AirPods (playing the take's backing) in call mode, unwarned.
      AudioDevice bt({bool input = false}) => AudioDevice(
        uid: 'airpods',
        name: 'AirPods Pro',
        rate: 48000,
        isInput: input,
        isBluetooth: true,
        transport: AudioTransport.bluetooth,
      );
      outs = [_builtInOut, bt()];
      ins = [_builtInMic, bt(input: true)];
      storage.doc = {
        'outputDeviceUid': _builtInOut.uid,
        'inputDeviceUid': 'airpods',
      };
      await service.load();
      expect(service.bluetoothInOutConflict, isTrue);
      expect(service.bluetoothInOutConflictNow(), isTrue);
    });

    test(
      'REC judges the mic the engine has open, not the returned pick',
      () async {
        // A mic opened on a fallback stays there after the pick returns.
        AudioDevice bt({bool input = false}) => AudioDevice(
          uid: 'airpods',
          name: 'AirPods Pro',
          rate: 48000,
          isDefault: true,
          isInput: input,
          isBluetooth: true,
          transport: AudioTransport.bluetooth,
        );
        outs = [bt()];
        ins = [_builtInMic, bt(input: true)];
        storage.doc = {'inputDeviceUid': _builtInMic.uid};
        await service.load();
        final engine = _FakeEngine()
          ..streamState = 1
          ..boundInput = 'airpods';
        service.attachEngine(engine);
        expect(service.bluetoothInOutConflictNow(), isTrue);
        engine.streamState = 0; // closed: the pick, the built-in mic
        expect(service.bluetoothInOutConflictNow(), isFalse);
      },
    );

    test(
      'the same Bluetooth headset listed under two uids is still one device',
      () async {
        AudioDevice bt(String uid, String name, {bool input = false}) =>
            AudioDevice(
              uid: uid,
              name: name,
              rate: 48000,
              isDefault: true,
              isInput: input,
              isBluetooth: true,
              transport: AudioTransport.bluetooth,
            );
        outs = [bt('aa:output', 'AirPods Pro')];
        ins = [bt('aa:input', 'AirPods Pro', input: true)];
        await service.load();
        expect(service.bluetoothInOutConflict, isTrue);
        ins = [bt('bb:input', 'Jabra Evolve2', input: true)];
        service.refreshDevices();
        expect(service.bluetoothInOutConflict, isFalse);
      },
    );

    test(
      'a stored mark on a device that now reads as a speaker is not in force',
      () async {
        // Renamed since it was marked: the card is not shown for it, so a mark still in force
        // would be one nobody could see to clear.
        const renamed = AudioDevice(
          uid: 'bt-out',
          name: 'Party Speaker',
          rate: 48000,
          isDefault: true,
          isBluetooth: true,
          transport: AudioTransport.bluetooth,
        );
        outs = [renamed];
        storage.doc = {
          'headphoneOutputUids': [renamed.uid],
        };
        await service.load();
        final engine = _FakeEngine()..actualOutput = renamed.uid;
        service.attachEngine(engine);
        service.setMonitorGainLive(0.5);
        expect(service.outputMarkable, isFalse);
        expect(service.monitorAllowed, isFalse);
        expect(engine.monitorGains.last, 0.0);
        expect(engine.echoFree.last, '');
      },
    );

    test(
      'Settings judges the choice, not what the last song played through',
      () async {
        // Settings opens from Home, after the song page let its engine go: what that engine
        // rendered to stayed cached, so the card offered (or hid) the mark for the wrong output.
        outs = [speakers, scarlett];
        storage.doc = {'outputDeviceUid': scarlett.uid};
        await service.load();
        final engine = _FakeEngine()..actualOutput = speakers.uid;
        service.attachEngine(engine);
        expect(service.outputToMark?.uid, speakers.uid);
        service.detachEngine(engine);
        expect(service.outputToMark?.uid, scarlett.uid);
        expect(service.outputMarkable, isTrue);
      },
    );

    test('clearing a mark during a mic open withdraws the name at once', () async {
      // A name is held while the engine reports nothing rendering (the mic opening), but not
      // past the mark it came from: the engine would act on it the moment the device bound.
      outs = [speakers, scarlett];
      await service.load();
      final engine = _FakeEngine()..actualOutput = scarlett.uid;
      service.attachEngine(engine);
      await service.setOutputIsHeadphones(scarlett.uid, true);
      expect(engine.echoFree.last, scarlett.uid);
      engine.actualOutput = '';
      service.reconcileMonitorSafety();
      expect(
        engine.echoFree.last,
        scarlett.uid,
        reason: 'held through the open',
      );
      await service.setOutputIsHeadphones(scarlett.uid, false);
      expect(engine.echoFree.last, '');
    });

    test(
      'a stored mark is loaded, but not over one changed while loading',
      () async {
        outs = [speakers, scarlett];
        storage.doc = {
          'headphoneOutputUids': [scarlett.uid, 7, 'other-out'],
        };
        await service.load();
        expect(service.outputMarkedHeadphones, isTrue);

        final again = build();
        final gate = Completer<void>();
        storage.readGate = gate;
        final loading = again.load();
        again.refreshDevices();
        await again.setOutputIsHeadphones(scarlett.uid, false);
        gate.complete();
        await loading;
        expect(again.outputMarkedHeadphones, isFalse);
        expect(storage.lastWrite!['headphoneOutputUids'], ['other-out']);
      },
    );
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
      'a take start with the mic open keys on the device it is bound to',
      () async {
        // The stream retries the chosen device only when it opens: monitoring
        // that opened on the fallback stays there after the chosen interface
        // returns, and keyed on the interface the fallback recorded under its
        // choice.
        ins = [_builtInMic];
        storage.doc = {
          'inputDeviceUid': _interface.uid,
          'inputChannelByUid': {_interface.uid: 2, _builtInMic.uid: 1},
        };
        await service.load();
        final engine = _FakeEngine();
        service.attachEngine(engine);
        ins = [_builtInMic, _interface]; // plugged back in

        engine
          ..streamState =
              1 // open, on the fallback
          ..boundInput = _builtInMic.uid;
        service.syncInputChannel();
        expect(engine.inputChannelCalls.last, 1, reason: "the bound mic's");

        engine.boundInput = ''; // open on the system default
        service.syncInputChannel();
        expect(engine.inputChannelCalls.last, 1, reason: "the default's");

        engine.streamState = 0; // closed: the next open takes the interface
        service.syncInputChannel();
        expect(engine.inputChannelCalls.last, 2, reason: "the interface's");
      },
    );

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
