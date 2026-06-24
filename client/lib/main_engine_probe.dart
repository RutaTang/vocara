// Standalone P0 probe for the native Rust audio engine. It does NOT touch the real app.
//
//   flutter run -t lib/main_engine_probe.dart -d macos
//
// Drag a song onto the window (or paste a path) → it decodes natively and plays through the
// vocara_engine framework. Use it to clear the P0 exit gate: engine opens, audio is
// glitch-free, the playhead advances, seek/loop work.

import 'package:desktop_drop/desktop_drop.dart';
import 'package:flutter/material.dart';
import 'package:window_manager/window_manager.dart';

import 'data/services/graph_audio_playback_service.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await windowManager.ensureInitialized();
  windowManager.waitUntilReadyToShow(
    const WindowOptions(title: 'vocara_engine probe', size: Size(640, 560)),
    () async {
      await windowManager.show();
      await windowManager.focus();
    },
  );
  runApp(const _ProbeApp());
}

class _ProbeApp extends StatelessWidget {
  const _ProbeApp();
  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      debugShowCheckedModeBanner: false,
      theme: ThemeData.dark(useMaterial3: true),
      home: const _ProbeScreen(),
    );
  }
}

class _ProbeScreen extends StatefulWidget {
  const _ProbeScreen();
  @override
  State<_ProbeScreen> createState() => _ProbeScreenState();
}

class _ProbeScreenState extends State<_ProbeScreen> {
  GraphAudioPlaybackService? _svc;
  final _pathCtrl = TextEditingController();
  final _log = <String>[];
  bool _dragging = false;
  bool _looping = false;

  @override
  void initState() {
    super.initState();
    try {
      final svc = GraphAudioPlaybackService();
      _svc = svc;
      _add(
        'engine created: available=${svc.isAvailable} '
        'sampleRate=${svc.sampleRate.toStringAsFixed(0)} sourceId=${svc.sourceId}',
      );
      if (!svc.isAvailable) {
        _add('⚠️  ve_create returned null — no output device opened.');
      }
    } catch (e) {
      _add('❌ failed to load the engine: $e');
    }
  }

  void _add(String line) {
    setState(() => _log.insert(0, line));
  }

  void _loadAndPlay(String path) {
    final svc = _svc;
    if (svc == null || !svc.isAvailable) {
      _add('engine not available');
      return;
    }
    _add('loading: $path');
    svc.loadAndPlayFile(path);
  }

  @override
  void dispose() {
    _svc?.dispose();
    _pathCtrl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final svc = _svc;
    final available = svc?.isAvailable ?? false;
    return Scaffold(
      appBar: AppBar(title: const Text('vocara_engine probe')),
      body: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(
              available
                  ? 'Engine ready · ${svc!.sampleRate.toStringAsFixed(0)} Hz · source ${svc.sourceId}'
                  : 'Engine NOT available',
              style: TextStyle(
                color: available ? Colors.greenAccent : Colors.redAccent,
                fontWeight: FontWeight.bold,
              ),
            ),
            const SizedBox(height: 12),

            // Drag-and-drop zone.
            DropTarget(
              onDragEntered: (_) => setState(() => _dragging = true),
              onDragExited: (_) => setState(() => _dragging = false),
              onDragDone: (detail) {
                setState(() => _dragging = false);
                if (detail.files.isNotEmpty) {
                  final p = detail.files.first.path;
                  _pathCtrl.text = p;
                  _loadAndPlay(p);
                }
              },
              child: Container(
                height: 96,
                decoration: BoxDecoration(
                  border: Border.all(
                    color: _dragging ? Colors.blueAccent : Colors.white24,
                    width: 2,
                  ),
                  borderRadius: BorderRadius.circular(10),
                  color: _dragging
                      ? Colors.blueAccent.withValues(alpha: 0.1)
                      : null,
                ),
                alignment: Alignment.center,
                child: const Text('Drag a song here to decode + play'),
              ),
            ),
            const SizedBox(height: 12),

            // Path field fallback.
            Row(
              children: [
                Expanded(
                  child: TextField(
                    controller: _pathCtrl,
                    decoration: const InputDecoration(
                      isDense: true,
                      border: OutlineInputBorder(),
                      hintText: '/absolute/path/to/song.mp3',
                    ),
                  ),
                ),
                const SizedBox(width: 8),
                FilledButton(
                  onPressed: () => _loadAndPlay(_pathCtrl.text.trim()),
                  child: const Text('Load & Play'),
                ),
              ],
            ),
            const SizedBox(height: 12),

            // Transport.
            Row(
              children: [
                IconButton.filledTonal(
                  onPressed: available ? () => svc!.play() : null,
                  icon: const Icon(Icons.play_arrow),
                ),
                IconButton.filledTonal(
                  onPressed: available ? () => svc!.pause() : null,
                  icon: const Icon(Icons.pause),
                ),
                IconButton.filledTonal(
                  onPressed: available
                      ? () {
                          svc!.pause();
                          svc.seek(Duration.zero);
                        }
                      : null,
                  icon: const Icon(Icons.stop),
                ),
                const SizedBox(width: 16),
                FilterChip(
                  label: const Text('loop 0–5 s'),
                  selected: _looping,
                  onSelected: available
                      ? (v) {
                          setState(() => _looping = v);
                          svc!.setLoop(v, 0, 5000);
                        }
                      : null,
                ),
                const Spacer(),
                if (available)
                  StreamBuilder<Duration>(
                    stream: svc!.position,
                    builder: (_, snap) {
                      final d = snap.data ?? Duration.zero;
                      final m = d.inMinutes
                          .remainder(60)
                          .toString()
                          .padLeft(2, '0');
                      final s = d.inSeconds
                          .remainder(60)
                          .toString()
                          .padLeft(2, '0');
                      final ms = (d.inMilliseconds.remainder(1000) ~/ 10)
                          .toString()
                          .padLeft(2, '0');
                      return Text(
                        '$m:$s.$ms',
                        style: const TextStyle(
                          fontFeatures: [FontFeature.tabularFigures()],
                          fontSize: 18,
                        ),
                      );
                    },
                  ),
              ],
            ),
            const SizedBox(height: 12),
            const Divider(),
            const Text('Log', style: TextStyle(fontWeight: FontWeight.bold)),
            Expanded(
              child: ListView.builder(
                itemCount: _log.length,
                itemBuilder: (_, i) => Text(
                  _log[i],
                  style: const TextStyle(fontSize: 12, fontFamily: 'monospace'),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
