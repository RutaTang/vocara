//! Disk-streaming sources (P1): the streamed render must EQUAL the
//! Arc-backed render bit-for-bit, discontinuities must resynchronize on the
//! fence, and starvation must degrade to counted silence that self-heals —
//! the invariants docs/audio-streaming-plan.md pins.

use std::collections::HashMap;
use std::sync::Arc;

use vocara_engine::dsp::master::MasterNode;
use vocara_engine::feed::{StreamCtrl, TrackFeeder, WavTrackReader};
use vocara_engine::graph::{Edge, GraphDesc, NodeEntry};
use vocara_engine::live::{spawn, EngineHandle, RtCell};
use vocara_engine::ring::spsc;
use vocara_engine::sample::{Smp, CHANNELS, ENGINE_SR};
use vocara_engine::source::{BufferSource, StreamingSource};
use vocara_engine::wav::write_wav_f32;

const BLOCK: usize = 128;

fn ramp_pcm(frames: usize) -> Vec<Smp> {
    let mut pcm = Vec::with_capacity(frames * CHANNELS);
    for i in 0..frames {
        let v = (i as Smp * 1e-4).sin() * 0.5; // non-trivial, deterministic
        pcm.push(v);
        pcm.push(-v);
    }
    pcm
}

fn write_track(frames: usize) -> std::path::PathBuf {
    // Unique per CALL, not just per (pid, frames): the harness runs tests in
    // parallel threads, and two tests sharing a frame count would otherwise
    // share a path — one's truncating rewrite tears the file under the
    // other's reader (observed as a rare short-read → silence mismatch).
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "vocara_stream_{}_{}_{}.wav",
        std::process::id(),
        frames,
        seq
    ));
    write_wav_f32(&path, &ramp_pcm(frames), CHANNELS as u16, ENGINE_SR as u32).unwrap();
    path
}

/// A live cell + handle around one source node.
fn engine_with(node: Box<dyn vocara_engine::node::Node + Send>) -> (EngineHandle, Arc<RtCell>) {
    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, node),
            NodeEntry::new(2, Box::new(MasterNode::unity())),
        ],
        edges: vec![Edge::simple(1, 2)],
        master: 2,
    };
    let g = desc.compile(ENGINE_SR, BLOCK).unwrap();
    let (_mic_prod, mic_cons) = spsc::<[Smp; CHANNELS]>(256);
    let (h, rt) = spawn(g, HashMap::new(), mic_cons, 256);
    (h, RtCell::new(rt))
}

/// Renders `blocks` blocks of `BLOCK` frames, pumping the feeder before each
/// (the deterministic stand-in for the host's periodic pump).
fn render_streamed(
    h: &EngineHandle,
    rt: &Arc<RtCell>,
    feeder: &mut TrackFeeder,
    blocks: usize,
) -> Vec<Smp> {
    let mut out = vec![0.0; blocks * BLOCK * CHANNELS];
    for b in 0..blocks {
        feeder.pump(h.transport()).unwrap();
        let s = b * BLOCK * CHANNELS;
        let e = s + BLOCK * CHANNELS;
        unsafe { rt.render(&mut out[s..e], BLOCK) };
        rt.transport().sync_consumed_to_rendered();
    }
    out
}

fn render_arc(_h: &EngineHandle, rt: &Arc<RtCell>, blocks: usize) -> Vec<Smp> {
    let mut out = vec![0.0; blocks * BLOCK * CHANNELS];
    for b in 0..blocks {
        let s = b * BLOCK * CHANNELS;
        let e = s + BLOCK * CHANNELS;
        unsafe { rt.render(&mut out[s..e], BLOCK) };
        rt.transport().sync_consumed_to_rendered();
    }
    out
}

fn streamed_pair(
    path: &std::path::Path,
    start_frame: i64,
    ring: usize,
) -> (EngineHandle, Arc<RtCell>, TrackFeeder) {
    let ctrl = StreamCtrl::new();
    let (prod, cons) = spsc::<[Smp; CHANNELS]>(ring);
    let reader = WavTrackReader::open(path.to_str().unwrap()).unwrap();
    let len = reader.frames;
    let node = StreamingSource::new(Arc::clone(&ctrl), cons, start_frame, len);
    let (h, rt) = engine_with(Box::new(node));
    let feeder = TrackFeeder::new(ctrl, prod, reader, start_frame);
    (h, rt, feeder)
}

#[test]
fn streamed_render_equals_the_arc_render_bit_for_bit() {
    let frames = 4096;
    let path = write_track(frames);

    let (h1, rt1, mut feeder) = streamed_pair(&path, 100, 8192);
    h1.play();
    let streamed = render_streamed(&h1, &rt1, &mut feeder, 40);

    let (h2, rt2) = engine_with(Box::new(BufferSource::new(ramp_pcm(frames), 100)));
    h2.play();
    let arc = render_arc(&h2, &rt2, 40);

    assert_eq!(streamed.len(), arc.len());
    for (i, (s, a)) in streamed.iter().zip(arc.iter()).enumerate() {
        assert!(
            s == a,
            "sample {i} diverged: streamed {s} vs arc {a} (bit-exact contract)"
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_seek_resynchronizes_on_the_fence_within_one_pump() {
    let frames = 8192;
    let path = write_track(frames);
    let (h, rt, mut feeder) = streamed_pair(&path, 0, 8192);
    h.play();
    let _ = render_streamed(&h, &rt, &mut feeder, 10);

    h.seek(4000);
    let post = render_streamed(&h, &rt, &mut feeder, 4);
    // Reference: the Arc render after the same seek.
    let (h2, rt2) = engine_with(Box::new(BufferSource::new(ramp_pcm(frames), 0)));
    h2.play();
    let _ = render_arc(&h2, &rt2, 10);
    h2.seek(4000);
    let post_arc = render_arc(&h2, &rt2, 4);
    assert_eq!(
        post, post_arc,
        "post-seek audio must equal the random-access reference"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_loop_wrap_streams_seamlessly_via_the_wrap_fence() {
    let frames = 4096;
    let path = write_track(frames);
    let (h, rt, mut feeder) = streamed_pair(&path, 0, 8192);
    h.set_loop(true, 100, 1100); // region 1000, several wraps in 40 blocks
    h.seek(100);
    h.play();
    let streamed = render_streamed(&h, &rt, &mut feeder, 40);

    let (h2, rt2) = engine_with(Box::new(BufferSource::new(ramp_pcm(frames), 0)));
    h2.set_loop(true, 100, 1100);
    h2.seek(100);
    h2.play();
    let arc = render_arc(&h2, &rt2, 40);
    assert_eq!(streamed, arc, "looped stream must equal the reference");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_streamed_track_through_the_full_project_graph_equals_the_arc_build() {
    // Wiring-level: the streaming source must behave identically inside the
    // REAL track chain (vocal insert chain, gains, master) that
    // ve_tracks_commit builds — not just as a bare node.
    use vocara_engine::build::{build, ProjectSpec, TrackSpec};
    let frames = 4096;
    let path = write_track(frames);

    let ctrl = StreamCtrl::new();
    let (prod, cons) = spsc::<[Smp; CHANNELS]>(8192);
    let reader = WavTrackReader::open(path.to_str().unwrap()).unwrap();
    let len = reader.frames;
    let streamed_node = StreamingSource::new(Arc::clone(&ctrl), cons, 0, len);
    let mut feeder = TrackFeeder::new(ctrl, prod, reader, 0);

    let mk = |source: Box<dyn vocara_engine::node::Node + Send>| {
        let desc = build(ProjectSpec::new(vec![TrackSpec::backing(1, source)]));
        let g = desc.compile(ENGINE_SR, BLOCK).unwrap();
        let (_p, mic) = spsc::<[Smp; CHANNELS]>(256);
        let (h, rt) = spawn(g, HashMap::new(), mic, 256);
        (h, RtCell::new(rt))
    };
    let (h1, rt1) = mk(Box::new(streamed_node));
    h1.play();
    let streamed = render_streamed(&h1, &rt1, &mut feeder, 30);

    let (h2, rt2) = mk(Box::new(BufferSource::new(ramp_pcm(frames), 0)));
    h2.play();
    let arc = render_arc(&h2, &rt2, 30);
    assert_eq!(streamed, arc, "full-graph streamed render must match");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn out_of_span_positions_are_silent_without_consuming_the_ring() {
    let frames = 512;
    let path = write_track(frames);
    // Track placed far ahead: everything rendered here is pre-span.
    let (h, rt, mut feeder) = streamed_pair(&path, 100_000, 4096);
    h.play();
    let out = render_streamed(&h, &rt, &mut feeder, 10);
    assert!(out.iter().all(|s| *s == 0.0), "pre-span must be silence");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn starvation_produces_counted_silence_and_self_heals() {
    let frames = 8192;
    let path = write_track(frames);
    let (h, rt, mut feeder) = streamed_pair(&path, 0, 1024);
    h.play();
    // Prime once, then render MORE than the ring holds without pumping:
    // the tail of the window starves.
    feeder.pump(h.transport()).unwrap();
    let mut out = vec![0.0; 30 * BLOCK * CHANNELS];
    unsafe { rt.render(&mut out, 30 * BLOCK) };
    rt.transport().sync_consumed_to_rendered();
    let starved_tail = &out[(29 * BLOCK) * CHANNELS..];
    assert!(
        starved_tail.iter().all(|s| *s == 0.0),
        "a starved stream must degrade to silence, never stale frames"
    );
    // The feeder catches up: rendering continues with the RIGHT content
    // for the CURRENT positions (self-heal without a discontinuity).
    let healed = render_streamed(&h, &rt, &mut feeder, 4);
    let (h2, rt2) = engine_with(Box::new(BufferSource::new(ramp_pcm(frames), 0)));
    h2.play();
    let mut skip = vec![0.0; 30 * BLOCK * CHANNELS];
    unsafe { rt2.render(&mut skip, 30 * BLOCK) };
    rt2.transport().sync_consumed_to_rendered();
    let healed_arc = render_arc(&h2, &rt2, 4);
    assert_eq!(
        healed, healed_arc,
        "post-starvation audio must resume at the correct positions"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn reader_admission_mirrors_the_dart_import_gate() {
    // Vectors matched by isEngineStreamableWav (audio_edit_service.dart):
    // only a canonical 44-byte header at the engine rate with 1-2 PCM16/f32
    // channels streams from disk. If the two rules drift, Dart skips the
    // import transcode for a file this reader then refuses at commit, and
    // the track silently falls back to the full-decode RAM path.
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let write = |name: &str, bytes: &[u8]| {
        let p = dir.join(format!("vocara_adm_{pid}_{name}.wav"));
        std::fs::write(&p, bytes).unwrap();
        p
    };
    let admitted = |p: &std::path::Path| WavTrackReader::open(p.to_str().unwrap()).is_ok();

    let ok = dir.join(format!("vocara_adm_{pid}_ok.wav"));
    write_wav_f32(&ok, &ramp_pcm(64), CHANNELS as u16, 48_000).unwrap();
    assert!(admitted(&ok), "canonical 48 kHz stereo f32 must stream");
    let canonical = std::fs::read(&ok).unwrap();

    let cd = dir.join(format!("vocara_adm_{pid}_cd.wav"));
    write_wav_f32(&cd, &ramp_pcm(64), CHANNELS as u16, 44_100).unwrap();
    assert!(!admitted(&cd), "44.1 kHz is not the engine rate");

    let quad = dir.join(format!("vocara_adm_{pid}_quad.wav"));
    write_wav_f32(&quad, &ramp_pcm(64), 4, 48_000).unwrap();
    assert!(!admitted(&quad), "more than 2 channels is refused");

    // PCM24: canonical layout, refused sample format.
    let mut pcm24 = canonical.clone();
    pcm24[20] = 1; // format: float -> integer PCM
    pcm24[34] = 24; // bits: 32 -> 24
    assert!(!admitted(&write("pcm24", &pcm24)), "PCM24 is refused");

    // A chunk between fmt and data breaks the canonical 44-byte layout.
    let mut chunked = Vec::with_capacity(canonical.len() + 12);
    chunked.extend_from_slice(&canonical[..36]);
    chunked.extend_from_slice(b"JUNK");
    chunked.extend_from_slice(&4u32.to_le_bytes());
    chunked.extend_from_slice(&[0; 4]);
    chunked.extend_from_slice(&canonical[36..]);
    let riff = u32::from_le_bytes(canonical[4..8].try_into().unwrap()) + 12;
    chunked[4..8].copy_from_slice(&riff.to_le_bytes());
    assert!(
        !admitted(&write("junk", &chunked)),
        "extra chunks between fmt and data are refused"
    );

    for p in [ok, cd, quad] {
        let _ = std::fs::remove_file(p);
    }
}
