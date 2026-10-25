//! Disk-streaming sources (P1): the streamed render must EQUAL the
//! Arc-backed render bit-for-bit, discontinuities must resynchronize on the
//! fence, and starvation must degrade to counted silence that self-heals —
//! the invariants docs/audio-streaming-plan.md pins.

use std::collections::HashMap;
use std::sync::Arc;

use vocara_engine::dsp::master::MasterNode;
use vocara_engine::feed::{fence_frames, parked_word, StreamCtrl, TrackFeeder, WavTrackReader};
use vocara_engine::graph::{Edge, Graph, GraphDesc, NodeEntry};
use vocara_engine::live::{spawn, EngineHandle, RtCell};
use vocara_engine::ring::{spsc, Producer};
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

/// One source node into the master.
fn source_graph(node: Box<dyn vocara_engine::node::Node + Send>) -> Graph {
    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, node),
            NodeEntry::new(2, Box::new(MasterNode::unity())),
        ],
        edges: vec![Edge::simple(1, 2)],
        master: 2,
    };
    desc.compile(ENGINE_SR, BLOCK).unwrap()
}

/// A live cell + handle around one source node.
fn engine_with(node: Box<dyn vocara_engine::node::Node + Send>) -> (EngineHandle, Arc<RtCell>) {
    let g = source_graph(node);
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
        rt.render_offline(&mut out[s..e], BLOCK);
        rt.transport().sync_consumed_to_rendered();
    }
    out
}

fn render_arc(_h: &EngineHandle, rt: &Arc<RtCell>, blocks: usize) -> Vec<Smp> {
    let mut out = vec![0.0; blocks * BLOCK * CHANNELS];
    for b in 0..blocks {
        let s = b * BLOCK * CHANNELS;
        let e = s + BLOCK * CHANNELS;
        rt.render_offline(&mut out[s..e], BLOCK);
        rt.transport().sync_consumed_to_rendered();
    }
    out
}

/// A streaming node and the feeder that fills its ring.
fn streamed_source(
    path: &std::path::Path,
    start_frame: i64,
    ring: usize,
) -> (StreamingSource, TrackFeeder) {
    let ctrl = StreamCtrl::new();
    let (prod, cons) = spsc::<[Smp; CHANNELS]>(ring);
    let reader = WavTrackReader::open(path.to_str().unwrap()).unwrap();
    let len = reader.frames;
    let node = StreamingSource::new(Arc::clone(&ctrl), cons, start_frame, len);
    (node, TrackFeeder::new(ctrl, prod, reader, start_frame))
}

fn streamed_pair(
    path: &std::path::Path,
    start_frame: i64,
    ring: usize,
) -> (EngineHandle, Arc<RtCell>, TrackFeeder) {
    let (node, feeder) = streamed_source(path, start_frame, ring);
    let (h, rt) = engine_with(Box::new(node));
    (h, rt, feeder)
}

/// A source fed BY HAND: the two tests below need control over the window
/// between a fence's two pushes, which a real feeder never exposes.
fn hand_fed_source() -> (
    EngineHandle,
    Arc<RtCell>,
    Producer<[Smp; CHANNELS]>,
    Arc<StreamCtrl>,
) {
    let ctrl = StreamCtrl::new();
    let (prod, cons) = spsc::<[Smp; CHANNELS]>(1024);
    let node = StreamingSource::new(Arc::clone(&ctrl), cons, 0, 1 << 20);
    let (h, rt) = engine_with(Box::new(node));
    h.play();
    (h, rt, prod, ctrl)
}

fn render_block(rt: &Arc<RtCell>) -> Vec<Smp> {
    let mut out = vec![0.0; BLOCK * CHANNELS];
    rt.render_offline(&mut out, BLOCK);
    rt.transport().sync_consumed_to_rendered();
    out
}

fn push_audio(prod: &mut Producer<[Smp; CHANNELS]>, frames: usize, v: Smp) {
    for _ in 0..frames {
        assert!(prod.push([v, -v]), "test ring is big enough");
    }
}

/// A fence is TWO pushes, published separately, so the node can observe the
/// generation frame while the base frame is still invisible. Parking that half
/// fence must not wedge the source: here the park happens in the FLUSH drain,
/// which re-runs next block and must not throw the parked fence away (its
/// generation frame is already consumed and never comes again).
#[test]
fn a_flush_fence_split_across_blocks_still_arms_the_source() {
    let (_h, rt, mut prod, ctrl) = hand_fed_source();
    let [gen_frame, base_frame] = fence_frames(1, BLOCK as i64);
    assert!(prod.push(gen_frame));
    ctrl.gen.store(1, std::sync::atomic::Ordering::Release);
    ctrl.flush_seq
        .fetch_add(1, std::sync::atomic::Ordering::Release);

    let parked = render_block(&rt);
    assert!(
        parked.iter().all(|s| *s == 0.0),
        "a fence with no base yet renders silence"
    );

    // The base frame lands, followed by the generation's audio.
    assert!(prod.push(base_frame));
    push_audio(&mut prod, BLOCK, 0.25);
    let healed = render_block(&rt);
    assert_eq!(
        healed[0], 0.25,
        "a fence whose base arrived a block late must still arm the source"
    );
    assert!(healed.iter().all(|s| s.abs() == 0.25));
}

/// The same split fence, but IN BAND (a loop wrap / starvation jump): the node
/// pops it mid-render and unarms. Every arming path is gated on `armed`, so the
/// parked fence must be retried from the render loop itself or nothing ever
/// completes it.
#[test]
fn an_in_band_fence_split_across_blocks_still_rearms_the_source() {
    let (_h, rt, mut prod, ctrl) = hand_fed_source();
    // Arm cleanly at generation 1, with exactly one block of audio behind it.
    let [g1, b1] = fence_frames(1, 0);
    assert!(prod.push(g1));
    assert!(prod.push(b1));
    ctrl.gen.store(1, std::sync::atomic::Ordering::Release);
    ctrl.flush_seq
        .fetch_add(1, std::sync::atomic::Ordering::Release);
    push_audio(&mut prod, BLOCK, 0.5);
    assert_eq!(render_block(&rt)[0], 0.5, "generation 1 plays");

    // Generation 2 arrives in band; its base frame is not visible yet.
    let [g2, b2] = fence_frames(2, 2 * BLOCK as i64);
    assert!(prod.push(g2));
    let parked = render_block(&rt);
    assert!(
        parked.iter().all(|s| *s == 0.0),
        "a fence with no base yet renders silence"
    );

    assert!(prod.push(b2));
    push_audio(&mut prod, BLOCK, 0.25);
    let healed = render_block(&rt);
    assert_eq!(
        healed[0], 0.25,
        "an in-band fence whose base arrived late must re-arm the source"
    );
    assert!(healed.iter().all(|s| s.abs() == 0.25));
}

/// The feeder pushes a discarding replan's fence BEFORE it bumps flush_seq,
/// and a starved node polls the ring every frame while the flush counter is
/// read once per block — so the node can fully adopt the new generation IN
/// BAND inside the block the fence landed in. The next block's flush check
/// must recognize that adopted state instead of disarming it: the fence is
/// consumed, so a drain hunting it discards every frame the feeder pushes,
/// forever (cur_flush never advances), with zero counted xruns.
#[test]
fn a_flush_whose_fence_was_already_adopted_in_band_keeps_the_source_armed() {
    let (_h, rt, mut prod, ctrl) = hand_fed_source();
    // Arm cleanly at generation 1 with one block of audio.
    let [g1, b1] = fence_frames(1, 0);
    assert!(prod.push(g1));
    assert!(prod.push(b1));
    ctrl.gen.store(1, std::sync::atomic::Ordering::Release);
    ctrl.flush_seq
        .fetch_add(1, std::sync::atomic::Ordering::Release);
    push_audio(&mut prod, BLOCK, 0.5);
    assert_eq!(render_block(&rt)[0], 0.5, "generation 1 plays");

    // A discarding replan lands mid-block, feeder-ordered: fence + audio
    // pushed and gen published FIRST...
    let [g2, b2] = fence_frames(2, BLOCK as i64);
    assert!(prod.push(g2));
    assert!(prod.push(b2));
    ctrl.gen.store(2, std::sync::atomic::Ordering::Release);
    push_audio(&mut prod, BLOCK, 0.25);
    // ...and the node adopts generation 2 in band within this block (its
    // top-of-block flush read predates the bump below). The fence itself
    // costs the first frame's pop, so assert on the block's tail.
    let adopted = render_block(&rt);
    assert_eq!(
        adopted[(BLOCK - 1) * CHANNELS],
        0.25,
        "generation 2 was adopted in band"
    );

    // ...THEN the flush counter bump becomes visible.
    ctrl.flush_seq
        .fetch_add(1, std::sync::atomic::Ordering::Release);
    push_audio(&mut prod, BLOCK, 0.75);
    let kept = render_block(&rt);
    assert_eq!(
        kept[0], 0.75,
        "an already-adopted generation must survive the flush check"
    );
    assert!(kept.iter().all(|s| s.abs() == 0.75));

    // And the acknowledgment must stick: later blocks keep playing too.
    push_audio(&mut prod, BLOCK, 0.125);
    let steady = render_block(&rt);
    assert_eq!(
        steady[0], 0.125,
        "cur_flush must advance so later blocks stop re-draining the ring"
    );
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
fn a_replan_with_the_playhead_past_loop_end_models_the_render_wrap() {
    // The recording flow plays PAST loop end with the loop disengaged, pauses
    // (the clock folds beyond loop end), then re-arms the loop without a seek.
    // On resume the render clock wraps `frame >= loop_end` back to loop_start
    // before producing anything, and the feeder's pump can replan from the
    // stale pre-wrap playhead before the first post-play block renders. The
    // plan must model that wrap: planning a linear run from past loop end
    // streams positions the transport never plays, silencing the track for
    // the whole looping session.
    let frames = 4096;
    let path = write_track(frames);
    let (h, rt, mut feeder) = streamed_pair(&path, 0, 8192);
    h.play();
    let _ = render_streamed(&h, &rt, &mut feeder, 12); // playhead at 1536
    h.pause(); // folds the clock past the loop end armed next
    h.set_loop(true, 100, 1100);
    h.play();
    // render_streamed pumps BEFORE each block: the first pump's replan reads
    // the pre-wrap fold (1536 > 1100), exactly the racing order in question.
    let streamed = render_streamed(&h, &rt, &mut feeder, 20);

    let (h2, rt2) = engine_with(Box::new(BufferSource::new(ramp_pcm(frames), 0)));
    h2.play();
    let _ = render_arc(&h2, &rt2, 12);
    h2.pause();
    h2.set_loop(true, 100, 1100);
    h2.play();
    let arc = render_arc(&h2, &rt2, 20);
    assert_eq!(
        streamed, arc,
        "a plan made from a pre-wrap playhead must stream the wrapped run"
    );
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
    rt.render_offline(&mut out, 30 * BLOCK);
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
    rt2.render_offline(&mut skip, 30 * BLOCK);
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

#[test]
fn a_loop_outside_the_track_span_neither_floods_the_ring_nor_wedges_reentry() {
    use std::sync::atomic::Ordering;
    let frames = 2048;
    let path = write_track(frames);
    let (h, rt, mut feeder) = streamed_pair(&path, 0, 4096);
    // Loop entirely past the track's [0, 2048) span. The old wrap logic
    // pushed one fence pair per simulated wrap until the ring was full
    // (gen climbed by thousands per pump); with the ring wedged, the next
    // replan could not land its fence and re-entering the track began with
    // a silent block.
    h.set_loop(true, 10_000, 20_000);
    h.seek(10_000);
    h.play();
    let _ = render_streamed(&h, &rt, &mut feeder, 8);
    let gen_before = feeder.ctrl.gen.load(Ordering::Acquire);
    for _ in 0..100 {
        feeder.pump(h.transport()).unwrap();
    }
    let gen_after = feeder.ctrl.gen.load(Ordering::Acquire);
    assert!(
        gen_after.wrapping_sub(gen_before) <= 1,
        "an out-of-span loop must not mint a generation per wrap \
         (got {gen_before} -> {gen_after})"
    );

    // Move the loop back onto the track: the replan fence must land at once
    // and audio must flow again within the pumped blocks.
    h.set_loop(true, 0, 1024);
    h.seek(0);
    let out = render_streamed(&h, &rt, &mut feeder, 8);
    assert!(
        out.iter().any(|&s| s != 0.0),
        "audio must resume once the loop covers the track again"
    );
}

#[test]
fn a_failed_reopen_leaves_the_working_handle_intact() {
    // The pump thread refreshes the reader's file handle after an I/O error
    // (a stale fd on a recovered volume keeps failing where a fresh open
    // succeeds). That refresh must never COST the caller its working handle:
    // if the reopen fails, reads keep flowing through the old one — which is
    // exactly the case on a briefly-unreachable network volume.
    let path = write_track(512);
    let mut reader = WavTrackReader::open(path.to_str().unwrap()).unwrap();
    let mut out = vec![[0.0 as Smp; CHANNELS]; 64];
    let before = reader.read_at(0, &mut out).unwrap();
    assert_eq!(before, 64);
    let reference = out.clone();

    // A successful reopen keeps reading the same bytes.
    reader.reopen().unwrap();
    let mut after = vec![[0.0 as Smp; CHANNELS]; 64];
    assert_eq!(reader.read_at(0, &mut after).unwrap(), 64);
    assert_eq!(after, reference, "a reopened handle reads identical bytes");

    // A FAILED reopen (file gone) must not clobber the live handle: POSIX
    // keeps the unlinked inode alive for the open fd.
    std::fs::remove_file(&path).unwrap();
    assert!(reader.reopen().is_err(), "reopen of a missing file fails");
    let mut still = vec![[0.0 as Smp; CHANNELS]; 64];
    assert_eq!(reader.read_at(0, &mut still).unwrap(), 64);
    assert_eq!(still, reference, "the surviving handle still reads");
}

/// A ramp whose FIRST frame is non-zero: [`ramp_pcm`] starts at sin(0) = 0,
/// which hides a dropout on exactly the first sample of a track.
fn offset_pcm(frames: usize) -> Vec<Smp> {
    let mut pcm = Vec::with_capacity(frames * CHANNELS);
    for i in 0..frames {
        let v = 0.25 + i as Smp * 1e-5;
        pcm.push(v);
        pcm.push(-v);
    }
    pcm
}

fn write_pcm(tag: &str, pcm: &[Smp]) -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "vocara_stream_{tag}_{}_{seq}.wav",
        std::process::id()
    ));
    write_wav_f32(&path, pcm, CHANNELS as u16, ENGINE_SR as u32).unwrap();
    path
}

#[test]
fn a_loop_longer_than_the_ring_streams_every_pass() {
    // The real geometry: multi-second loops over a 16384-frame ring. Right
    // after the wrap fence the plan restarts at loop_start while the node is
    // still playing the pre-wrap tail out of the ring; the starvation jump
    // used to read that as "transport ahead of the plan" and push a fence
    // into the middle of the next pass, which the node then sat silent
    // waiting for. The 1000-frame loop in the test above never exposed it:
    // the feeder stays ahead of so short a pass.
    for &(ls, le, ring) in &[
        (100i64, 1100i64, 8192usize),
        (0, 3000, 8192),
        (0, 20_000, 4096),
        (0, 48_000, 16_384),
    ] {
        let frames = (le as usize + 10).max(4096);
        let path = write_track(frames);
        let (h, rt, mut feeder) = streamed_pair(&path, 0, ring);
        h.set_loop(true, ls, le);
        h.seek(ls);
        h.play();
        let blocks = ((le - ls) as usize * 3) / BLOCK + 10; // three passes
        let streamed = render_streamed(&h, &rt, &mut feeder, blocks);

        let (h2, rt2) = engine_with(Box::new(BufferSource::new(ramp_pcm(frames), 0)));
        h2.set_loop(true, ls, le);
        h2.seek(ls);
        h2.play();
        let arc = render_arc(&h2, &rt2, blocks);
        let bad = (0..streamed.len() / 2)
            .filter(|&i| streamed[2 * i] != arc[2 * i])
            .count();
        assert_eq!(bad, 0, "loop [{ls}, {le}) over a {ring}-frame ring");
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn past_a_tracks_end_the_feeder_mints_at_most_one_fence() {
    // The node never pops past the span, so a fence pushed there sits in the
    // ring. Re-minting one every pump filled the ring with fences, which a
    // later seek back then had to wade through.
    let path = write_track(1024);
    let (h, rt, mut feeder) = streamed_pair(&path, 0, 4096);
    h.play();
    let _ = render_streamed(&h, &rt, &mut feeder, 20); // playhead 2560: past the end
    let g0 = feeder.ctrl.gen.load(std::sync::atomic::Ordering::Relaxed);
    let _ = render_streamed(&h, &rt, &mut feeder, 100);
    let g1 = feeder.ctrl.gen.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        g1.wrapping_sub(g0) <= 1,
        "{} generations minted over 100 post-span pumps",
        g1.wrapping_sub(g0)
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_loop_starting_before_the_track_plays_its_first_frame_every_pass() {
    // A take that starts mid-loop: the wrap fence's base (loop_start) is
    // before the track's first frame. Adopting it re-armed the node below the
    // span AFTER this frame's pre-span clamp, silencing the track's first
    // frame on every pass after the first.
    let frames = 4096;
    let start = 500i64;
    let path = write_pcm("offset", &offset_pcm(frames));
    let (h, rt, mut feeder) = streamed_pair(&path, start, 8192);
    h.set_loop(true, 100, 1100);
    h.seek(100);
    h.play();
    let streamed = render_streamed(&h, &rt, &mut feeder, 40);

    let (h2, rt2) = engine_with(Box::new(BufferSource::new(offset_pcm(frames), start)));
    h2.set_loop(true, 100, 1100);
    h2.seek(100);
    h2.play();
    let arc = render_arc(&h2, &rt2, 40);
    assert_eq!(streamed, arc, "each pass must play the track's first frame");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_loop_edit_after_the_render_wrapped_replans_from_the_render_clock() {
    // On the resampled output path the device trails the render by its FIFO,
    // so near loop end the render can already have wrapped to loop_start
    // while the audible playhead has not. A replan then (dragging a loop
    // handle) based at the audible playhead put the plan a whole pass ahead
    // of the node, which rendered silence until the clock caught up.
    const LAG: u64 = 256;
    let frames = 8192;
    let path = write_pcm("lagged", &offset_pcm(frames));
    let run = |streamed: bool| -> Vec<Smp> {
        let (h, rt, mut feeder) = streamed_pair(&path, 0, 16_384);
        let (h2, rt2) = engine_with(Box::new(BufferSource::new(offset_pcm(frames), 0)));
        let (h, rt) = if streamed { (h, rt) } else { (h2, rt2) };
        let mut rendered = 0u64;
        let mut out = Vec::new();
        let mut render = |h: &EngineHandle, feeder: &mut TrackFeeder, blocks: usize| {
            for _ in 0..blocks {
                if streamed {
                    feeder.pump(h.transport()).unwrap();
                }
                let mut block = vec![0.0; BLOCK * CHANNELS];
                rt.render_offline(&mut block, BLOCK);
                out.extend_from_slice(&block);
                rendered += BLOCK as u64;
                // The device has consumed all but the last LAG frames.
                let t = rt.transport();
                let target = rendered.saturating_sub(LAG);
                let cur = t.consumed_frames();
                if target > cur {
                    t.advance_consumed(target - cur);
                }
            }
        };
        h.set_loop(true, 0, 4000);
        h.seek(0);
        h.play();
        // 32 blocks = 4096 frames rendered: the render wrapped at 4000, the
        // audible playhead (4096 - LAG = 3840) has not.
        render(&h, &mut feeder, 32);
        h.set_loop(true, 64, 4000);
        render(&h, &mut feeder, 40);
        out
    };
    let streamed = run(true);
    let arc = run(false);
    let bad = (0..streamed.len() / 2)
        .filter(|&i| streamed[2 * i] != arc[2 * i])
        .count();
    assert_eq!(bad, 0, "the replan must not silence the pass");
    let _ = std::fs::remove_file(&path);
}

/// Real cadence: one pump per ~512-frame device callback (4 x BLOCK), with a
/// pause landing at tick 100 BETWEEN the pump and the callback — the pump has
/// just topped the ring up and nothing has popped since. `edit` runs while
/// paused; play resumes at tick 110. Returns what was rendered from resume.
fn pause_on_a_topped_up_ring(
    streamed: bool,
    path: &std::path::Path,
    pcm: &[Smp],
    ticks: usize,
    edit: impl Fn(usize, &EngineHandle),
) -> Vec<Smp> {
    let (h, rt, mut feeder) = streamed_pair(path, 0, 16_384);
    let (h2, rt2) = engine_with(Box::new(BufferSource::new(pcm.to_vec(), 0)));
    let (h, rt) = if streamed { (h, rt) } else { (h2, rt2) };
    h.play();
    let mut out = Vec::new();
    for t in 0..ticks {
        edit(t, &h);
        if t == 110 {
            h.play();
        }
        if streamed {
            feeder.pump(h.transport()).unwrap();
        }
        if t == 100 {
            h.pause();
        }
        for _ in 0..4 {
            let mut block = vec![0.0; BLOCK * CHANNELS];
            rt.render_offline(&mut block, BLOCK);
            rt.transport().sync_consumed_to_rendered();
            if t >= 110 {
                out.extend_from_slice(&block);
            }
        }
    }
    out
}

fn differing_frames(a: &[Smp], b: &[Smp]) -> usize {
    (0..a.len() / 2).filter(|&i| a[2 * i] != b[2 * i]).count()
}

#[test]
fn a_seek_back_while_paused_on_a_full_ring_plays_from_the_new_position() {
    // The pause froze a FULL ring, so the seek's replan fence had nowhere to
    // go: on play the track stayed silent until the playhead walked back up
    // to where it had been paused.
    let pcm = offset_pcm(400_000);
    let path = write_pcm("seekback", &pcm);
    let edit = |t: usize, h: &EngineHandle| {
        if t == 105 {
            h.seek(0);
        }
    };
    let streamed = pause_on_a_topped_up_ring(true, &path, &pcm, 400, edit);
    let arc = pause_on_a_topped_up_ring(false, &path, &pcm, 400, edit);
    let bad = differing_frames(&streamed, &arc);
    assert_eq!(bad, 0, "{bad} frames differ after the seek back");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_loop_set_behind_a_full_paused_ring_plays_every_pass() {
    // The same frozen ring, then a loop that ends before the pause point (and
    // the playhead snapped into it): the plan never caught up, so every pass
    // of the loop was silent, for good.
    let pcm = offset_pcm(400_000);
    let path = write_pcm("loopbehind", &pcm);
    let edit = |t: usize, h: &EngineHandle| {
        if t == 103 {
            h.set_loop(true, 0, 24_000);
            h.seek(0);
        }
    };
    let streamed = pause_on_a_topped_up_ring(true, &path, &pcm, 1000, edit);
    let arc = pause_on_a_topped_up_ring(false, &path, &pcm, 1000, edit);
    let bad = differing_frames(&streamed, &arc);
    assert_eq!(bad, 0, "{bad} frames differ across the loop passes");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_play_and_pause_with_nothing_heard_keeps_the_playhead() {
    // Resampled output path: the device trails the render by LAG frames. The
    // pause's flush skips the cursor over the FIFO, so it then sits at the end
    // of the last rendered span; a play/pause pair inside one device callback
    // folded the clock to THAT span's end — forward by the whole backlog.
    const LAG: u64 = 1024;
    let (h, rt) = engine_with(Box::new(BufferSource::new(offset_pcm(100_000), 0)));
    let t = rt.transport();
    let mut rendered = 0u64;
    let mut block = vec![0.0; BLOCK * CHANNELS];
    h.play();
    for _ in 0..100 {
        rt.render_offline(&mut block, BLOCK);
        rendered += BLOCK as u64;
        let target = rendered.saturating_sub(LAG);
        let cur = t.consumed_frames();
        if target > cur {
            t.advance_consumed(target - cur);
        }
    }
    h.pause();
    let heard = h.frame();
    assert_eq!(heard, (rendered - LAG) as i64);
    // The callback processes the flush: the cursor skips the discarded FIFO.
    t.advance_consumed(rendered - t.consumed_frames());
    for _ in 0..10 {
        rt.render_offline(&mut block, BLOCK);
        t.sync_consumed_to_rendered();
    }
    assert_eq!(h.frame(), heard);

    h.play();
    assert_eq!(h.frame(), heard, "nothing of the new run is heard yet");
    h.pause();
    assert_eq!(h.frame(), heard, "a play/pause with nothing heard moved it");
}

/// Real cadence: one pump per ~512-frame device callback (4 x BLOCK), before
/// or after that callback; `edit` runs at the start of each tick. Returns
/// everything rendered.
fn at_real_cadence(
    streamed: bool,
    path: &std::path::Path,
    pcm: &[Smp],
    ticks: usize,
    pump_before_render: bool,
    edit: impl Fn(usize, &EngineHandle),
) -> Vec<Smp> {
    let (h, rt, mut feeder) = streamed_pair(path, 0, 16_384);
    let (h2, rt2) = engine_with(Box::new(BufferSource::new(pcm.to_vec(), 0)));
    let (h, rt) = if streamed { (h, rt) } else { (h2, rt2) };
    // The commit primes every stream ring before the graph goes live.
    if streamed {
        feeder.pump(h.transport()).unwrap();
    }
    h.play();
    let mut out = Vec::new();
    for t in 0..ticks {
        edit(t, &h);
        if streamed && pump_before_render {
            feeder.pump(h.transport()).unwrap();
        }
        for _ in 0..4 {
            let mut block = vec![0.0; BLOCK * CHANNELS];
            rt.render_offline(&mut block, BLOCK);
            rt.transport().sync_consumed_to_rendered();
            out.extend_from_slice(&block);
        }
        if streamed && !pump_before_render {
            feeder.pump(h.transport()).unwrap();
        }
    }
    out
}

fn streamed_vs_arc(
    tag: &str,
    ticks: usize,
    pump_before_render: bool,
    edit: impl Fn(usize, &EngineHandle),
) -> usize {
    let pcm = offset_pcm(400_000);
    let path = write_pcm(tag, &pcm);
    let streamed = at_real_cadence(true, &path, &pcm, ticks, pump_before_render, &edit);
    let arc = at_real_cadence(false, &path, &pcm, ticks, pump_before_render, &edit);
    let _ = std::fs::remove_file(&path);
    differing_frames(&streamed, &arc)
}

#[test]
fn a_resume_whose_first_callback_beats_the_pump_plays_at_once() {
    // Play replanned, discarding what the fill had queued behind the pause's
    // own plan; when the first playing callback came before the next pump,
    // the track opened every resume with a block of silence.
    let bad = streamed_vs_arc("resume", 200, false, |t, h| {
        if t == 100 {
            h.pause();
        }
        if t == 110 {
            h.play();
        }
    });
    assert_eq!(bad, 0);
}

#[test]
fn a_seek_during_playback_leaves_no_hole_behind_its_fence() {
    // With only the fence's two slots to spare, the fill behind a replan got
    // just what the node had consumed since the last pump, and the node ran
    // dry right before the next one.
    let bad = streamed_vs_arc("seekhole", 200, true, |t, h| {
        if t == 100 {
            h.seek(50_000);
        }
    });
    assert_eq!(bad, 0);
}

#[test]
fn dragging_a_loop_handle_during_playback_leaves_no_holes() {
    // A drag replans on every pump; each replan left the same hole.
    let bad = streamed_vs_arc("drag", 180, true, |t, h| {
        if (100..160).contains(&t) {
            h.set_loop(true, 0, 300_000 + (t as i64) * 480);
        }
    });
    assert_eq!(bad, 0);
}

#[test]
fn a_replan_whose_fill_reaches_loop_end_keeps_the_pre_wrap_run() {
    // The fill behind a replan can now reach loop end and push the wrap's
    // fence before the node takes the flush. The node discards up to the
    // flush TARGET, so the wrap's in-band fence must not become it: that
    // skipped the rest of the pass.
    for edit_at in [40usize, 45, 46] {
        let bad = streamed_vs_arc("nearend", 120, true, |t, h| {
            if t == 0 {
                h.set_loop(true, 0, 24_000);
            }
            if t == edit_at {
                h.set_loop(true, 0, 24_100);
            }
        });
        assert_eq!(bad, 0, "loop edited at tick {edit_at}");
    }
}

/// xorshift64: a deterministic op stream for the differential fuzz below.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

/// One block on a device whose consumed cursor trails the render by `lag`
/// frames (0 = the identity path), pumping first when asked.
fn lagged_block(
    h: &EngineHandle,
    rt: &Arc<RtCell>,
    feeder: Option<&mut TrackFeeder>,
    lag: u64,
    rendered: &mut u64,
) -> Vec<Smp> {
    if let Some(f) = feeder {
        f.pump(h.transport()).unwrap();
    }
    let mut block = vec![0.0; BLOCK * CHANNELS];
    rt.render_offline(&mut block, BLOCK);
    *rendered += BLOCK as u64;
    let t = rt.transport();
    let target = rendered.saturating_sub(lag);
    let cur = t.consumed_frames();
    if target > cur {
        t.advance_consumed(target - cur);
    }
    block
}

/// Random seeks, pause/play, loops (before, across, inside and outside the
/// track), loop off and feeder stalls, on a streamed track and its in-memory
/// twin; from the first pump after each op, the two renders must be identical.
fn fuzz_case(seed: u64, lag: u64) -> Vec<String> {
    let mut rng = Rng(seed * 2_654_435_761 + 1);
    let frames = 20_000usize;
    let start = [0i64, 500, 3000, 12_000][rng.below(4) as usize];
    let cadence = 1 + rng.below(3) as usize;
    let ring = [2048usize, 8192, 16_384][rng.below(3) as usize];
    let path = write_track(frames);
    let (h, rt, mut feeder) = streamed_pair(&path, start, ring);
    let (h2, rt2) = engine_with(Box::new(BufferSource::new(ramp_pcm(frames), start)));
    let (mut r1, mut r2) = (0u64, 0u64);
    let mut fails = Vec::new();
    h.play();
    h2.play();
    let mut loop_len = 0i64;
    for op in 0..20 {
        let desc = match rng.below(7) {
            0 => {
                let p = rng.below(26_000) as i64;
                h.seek(p);
                h2.seek(p);
                format!("seek {p}")
            }
            1 => {
                h.pause();
                h2.pause();
                for _ in 0..rng.below(4) {
                    lagged_block(&h, &rt, Some(&mut feeder), lag, &mut r1);
                    lagged_block(&h2, &rt2, None, lag, &mut r2);
                }
                h.play();
                h2.play();
                "pause/play".into()
            }
            2 | 3 => {
                let ls = rng.below(34_000) as i64;
                let span = if rng.below(4) == 0 { 300 } else { 9_000 };
                let len = 256 + rng.below(span) as i64;
                h.set_loop(true, ls, ls + len);
                h2.set_loop(true, ls, ls + len);
                loop_len = len;
                format!("loop [{ls},{})", ls + len)
            }
            4 => {
                h.set_loop(false, 0, 0);
                h2.set_loop(false, 0, 0);
                loop_len = 0;
                "loop off".into()
            }
            5 => {
                let n = rng.below(200);
                for _ in 0..n {
                    lagged_block(&h, &rt, None, lag, &mut r1);
                    lagged_block(&h2, &rt2, None, lag, &mut r2);
                }
                format!("stall {n}")
            }
            _ => "nothing".into(),
        };
        // Run for two loop passes (or a ring) plus slack, all of it compared:
        // the settle window opens with a pump, and that one pump must heal
        // whatever the op did. Comparing only its tail hid a stall across a
        // loop wrap, which left the track silent for most of the next pass.
        let settle = (2 * loop_len.max(ring as i64)) as usize / BLOCK + 40;
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for i in 0..settle {
            let pump = (i % cadence == 0).then_some(&mut feeder);
            a.extend(lagged_block(&h, &rt, pump, lag, &mut r1));
            b.extend(lagged_block(&h2, &rt2, None, lag, &mut r2));
        }
        let bad = differing_frames(&a, &b);
        if bad > 0 {
            fails.push(format!(
                "seed {seed} lag {lag} cadence {cadence} start {start} ring {ring} \
                 op {op} [{desc}]: {bad} frames differ after its first pump"
            ));
        }
    }
    let _ = std::fs::remove_file(&path);
    fails
}

#[test]
fn the_streamed_render_settles_onto_the_in_memory_one_under_random_transport_ops() {
    let mut fails = Vec::new();
    for seed in 100..106u64 {
        for lag in [0u64, 300, 1100] {
            fails.extend(fuzz_case(seed, lag));
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

#[test]
fn seeking_while_paused_then_playing_opens_without_a_hole() {
    // Paused, the node popped nothing, so the pause's replan backlog never
    // left the ring: a seek's fence took the last free slots with no audio
    // behind it, a second seek's could not land at all, and Play opened with
    // a whole callback of silence (one per callback size, ~85 ms at 4096).
    for seeks in 1..=4i64 {
        for pump_before_render in [true, false] {
            let bad = streamed_vs_arc("pausedscrub", 160, pump_before_render, |t, h| {
                if t == 100 {
                    h.pause();
                }
                if (102..102 + seeks as usize).contains(&t) {
                    h.seek(50_000 * (t as i64 - 101));
                }
                if t == 110 {
                    h.play();
                }
            });
            assert_eq!(
                bad, 0,
                "{seeks} seeks, pump before render: {pump_before_render}"
            );
        }
    }
}

#[test]
fn stop_then_play_opens_without_a_silent_callback() {
    // Stop is pause + seek to the play range's start. The pump woken by the
    // pause can replan in between, which left the seek's fence nothing but
    // the last two slots of a ring the paused node never drained.
    let pcm = offset_pcm(400_000);
    let path = write_pcm("stopplay", &pcm);
    for pump_between in [false, true] {
        let run = |streamed: bool| {
            let (h, rt, mut feeder) = streamed_pair(&path, 0, 16_384);
            let (h2, rt2) = engine_with(Box::new(BufferSource::new(pcm.to_vec(), 0)));
            let (h, rt) = if streamed { (h, rt) } else { (h2, rt2) };
            let mut pump = |h: &EngineHandle| {
                if streamed {
                    feeder.pump(h.transport()).unwrap();
                }
            };
            pump(&h);
            h.seek(24_000);
            h.play();
            let mut out = Vec::new();
            for t in 0..200 {
                if t == 100 {
                    h.pause();
                    if pump_between {
                        pump(&h);
                    }
                    h.seek(24_000);
                }
                if t == 110 {
                    h.play();
                }
                pump(&h);
                for _ in 0..4 {
                    let mut block = vec![0.0; BLOCK * CHANNELS];
                    rt.render_offline(&mut block, BLOCK);
                    rt.transport().sync_consumed_to_rendered();
                    out.extend_from_slice(&block);
                }
            }
            out
        };
        let bad = differing_frames(&run(true), &run(false));
        assert_eq!(bad, 0, "pump between pause and seek: {pump_between}");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_held_plan_the_node_has_partly_played_is_not_resumed() {
    // A play/pause the pump never saw (its one thread stuck on another
    // track's slow read) lets the node play the head of the paused plan; a
    // seek back to that plan's start then matched it again, and Play resumed
    // a plan the node was already past — silent until it caught up.
    let pcm = offset_pcm(400_000);
    let path = write_pcm("heldplayed", &pcm);
    let held = std::cell::Cell::new(0i64);
    let run = |streamed: bool| {
        let (h, rt, mut feeder) = streamed_pair(&path, 0, 16_384);
        let (h2, rt2) = engine_with(Box::new(BufferSource::new(pcm.to_vec(), 0)));
        let (h, rt) = if streamed { (h, rt) } else { (h2, rt2) };
        if streamed {
            feeder.pump(h.transport()).unwrap();
        }
        h.play();
        let mut out = Vec::new();
        for t in 0..160 {
            if t == 100 {
                h.pause();
                held.set(h.frame());
            }
            if t == 105 {
                h.play();
            }
            if t == 108 {
                h.pause();
                h.seek(held.get());
                h.play();
            }
            // The pump is stalled through ticks 105..108.
            if streamed && !(105..108).contains(&t) {
                feeder.pump(h.transport()).unwrap();
            }
            for _ in 0..4 {
                let mut block = vec![0.0; BLOCK * CHANNELS];
                rt.render_offline(&mut block, BLOCK);
                rt.transport().sync_consumed_to_rendered();
                if t >= 108 {
                    out.extend_from_slice(&block);
                }
            }
        }
        out
    };
    let bad = differing_frames(&run(true), &run(false));
    let _ = std::fs::remove_file(&path);
    assert_eq!(bad, 0);
}

#[test]
fn an_in_band_fence_past_an_in_band_flush_keeps_the_source_armed() {
    // A starved node adopts a discarding fence IN BAND (its block read
    // flush_seq before the feeder bumped it), then an in-band wrap/jump fence
    // in the same block. Only discarding fences publish ctrl.gen, so the next
    // block's flush target is a generation the node is already PAST: hunting
    // it exactly discarded every later frame, silently (no xruns).
    use std::sync::atomic::Ordering;
    let (_h, rt, mut prod, ctrl) = hand_fed_source();
    let [g1, b1] = fence_frames(1, 0);
    assert!(prod.push(g1));
    assert!(prod.push(b1));
    ctrl.gen.store(1, Ordering::Release);
    ctrl.flush_seq.fetch_add(1, Ordering::Release);
    push_audio(&mut prod, BLOCK, 0.5);
    assert_eq!(render_block(&rt)[0], 0.5, "generation 1 plays");

    let [g2, b2] = fence_frames(2, BLOCK as i64);
    assert!(prod.push(g2));
    assert!(prod.push(b2));
    ctrl.gen.store(2, Ordering::Release);
    push_audio(&mut prod, 32, 0.25);
    let [g3, b3] = fence_frames(3, BLOCK as i64 + 33);
    assert!(prod.push(g3));
    assert!(prod.push(b3));
    push_audio(&mut prod, BLOCK, 0.75);
    let _ = render_block(&rt); // adopts 2, then 3, both in band
    ctrl.flush_seq.fetch_add(1, Ordering::Release); // the bump lands late
    push_audio(&mut prod, 4 * BLOCK, 0.125);
    let mut tail = Vec::new();
    for _ in 0..4 {
        tail.extend(render_block(&rt));
    }
    let silent = (0..tail.len() / 2).filter(|&i| tail[2 * i] == 0.0).count();
    assert_eq!(silent, 0);
}

#[test]
fn a_pause_right_after_a_seek_keeps_the_seek() {
    // Resampled path: nothing of the post-seek render has reached the device
    // when the pause lands. The fold mapped the consumed cursor through the
    // PRE-seek spans and moved the clock back there — the seek was lost — and
    // until then the playing playhead showed the old position.
    const LAG: u64 = 1024;
    let (h, rt) = engine_with(Box::new(BufferSource::new(offset_pcm(400_000), 0)));
    let t = rt.transport();
    let mut rendered = 0u64;
    let mut block = vec![0.0; BLOCK * CHANNELS];
    let mut render = |n: usize| {
        for _ in 0..n {
            rt.render_offline(&mut block, BLOCK);
            rendered += BLOCK as u64;
            let target = rendered.saturating_sub(LAG);
            let cur = t.consumed_frames();
            if target > cur {
                t.advance_consumed(target - cur);
            }
        }
    };
    h.play();
    render(100);
    h.seek(200_000);
    render(1); // the render applies it; the device has heard none of it yet
    assert!(h.frame() >= 200_000, "playing: shows {}", h.frame());
    h.pause();
    assert!(
        (200_000..=200_000 + BLOCK as i64).contains(&h.frame()),
        "paused at {}",
        h.frame()
    );
}

#[test]
fn play_right_after_a_paused_commit_opens_on_audio() {
    // An interpretation switch pauses, commits the new graph and plays within
    // a millisecond. The commit primed nothing while paused (the pump thread
    // filled the rings, one whole ring at a time), so the tracks it reached
    // last opened on silence when the first callback beat it.
    let pcm = offset_pcm(400_000);
    let path = write_pcm("pausedcommit", &pcm);
    let run = |streamed: bool, pump_after_ticks: usize| {
        let (h, rt, mut feeder) = streamed_pair(&path, 0, 1 << 16);
        let (h2, rt2) = engine_with(Box::new(BufferSource::new(pcm.to_vec(), 0)));
        let (h, rt) = if streamed { (h, rt) } else { (h2, rt2) };
        let mut block = vec![0.0; BLOCK * CHANNELS];
        h.seek(24_000);
        for _ in 0..4 {
            rt.render_offline(&mut block, BLOCK);
        }
        if streamed {
            // What ve_tracks_commit primes while paused.
            feeder.pump_at_most(h.transport(), 4096).unwrap();
        }
        for _ in 0..4 {
            rt.render_offline(&mut block, BLOCK);
        }
        h.play();
        let mut out = Vec::new();
        for t in 0..60 {
            if streamed && t >= pump_after_ticks {
                feeder.pump(h.transport()).unwrap();
            }
            for _ in 0..4 {
                rt.render_offline(&mut block, BLOCK);
                rt.transport().sync_consumed_to_rendered();
                out.extend_from_slice(&block);
            }
        }
        out
    };
    let arc = run(false, 0);
    for late in [0, 1, 2] {
        let bad = differing_frames(&run(true, late), &arc);
        assert_eq!(bad, 0, "the pump thread landed {late} callbacks late");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_stream_starved_across_a_loop_wrap_plays_the_next_pass() {
    // The pump stalled long enough for the ring to run dry short of loop end,
    // and the render wrapped. The node stayed armed at its pre-wrap position
    // and waited for the transport to come round to it again — most of a pass
    // of silence — and the starvation jump never fired: the wrapped transport
    // sits BEHIND the plan's position, not ahead of it.
    let pcm = offset_pcm(40_000);
    let path = write_pcm("wrapstarve", &pcm);
    let run = |streamed: bool| {
        let (h, rt, mut feeder) = streamed_pair(&path, 0, 4096);
        let (h2, rt2) = engine_with(Box::new(BufferSource::new(pcm.clone(), 0)));
        let (h, rt) = if streamed { (h, rt) } else { (h2, rt2) };
        h.set_loop(true, 0, 12_000);
        h.play();
        let mut rendered = 0u64;
        let mut out = Vec::new();
        for b in 0..200 {
            // No pump from frame 6016 to 14_208: the ring, filled ~3k frames
            // ahead, runs dry near 9k and the render wraps at 12_000.
            let pump = streamed && !(47..111).contains(&b);
            let f = pump.then_some(&mut feeder);
            out.extend(lagged_block(&h, &rt, f, 0, &mut rendered));
        }
        out
    };
    let (streamed, arc) = (run(true), run(false));
    let _ = std::fs::remove_file(&path);
    let from = 111 * BLOCK * CHANNELS; // the first pump after the stall
    assert_eq!(differing_frames(&streamed[from..], &arc[from..]), 0);
}

#[test]
fn a_commit_primed_just_before_a_loop_wrap_plays_the_next_pass() {
    // ve_tracks_commit primes every new stream from the render clock, then
    // publishes the graph. When the render wrapped in between, the new node
    // adopted a base the transport had just left behind and waited for it to
    // come round again: the track sat out the whole next pass.
    let pcm = offset_pcm(40_000);
    let path = write_pcm("wrapcommit", &pcm);
    let run = |streamed: bool| {
        let (h, rt, mut feeder) = streamed_pair(&path, 0, 16_384);
        let (h2, rt2) = engine_with(Box::new(BufferSource::new(pcm.clone(), 0)));
        let (mut h, rt) = if streamed { (h, rt) } else { (h2, rt2) };
        h.set_loop(true, 0, 12_000);
        h.play();
        let mut rendered = 0u64;
        let mut out = Vec::new();
        for _ in 0..93 {
            let f = streamed.then_some(&mut feeder);
            out.extend(lagged_block(&h, &rt, f, 0, &mut rendered));
        }
        // The commit, 96 frames short of loop end: the new stream is primed
        // here, and the block the old graph is still rendering wraps.
        let (node, mut next) = streamed_source(&path, 0, 16_384);
        let graph = if streamed {
            next.pump(h.transport()).unwrap();
            source_graph(Box::new(node))
        } else {
            source_graph(Box::new(BufferSource::new(pcm.clone(), 0)))
        };
        out.extend(lagged_block(&h, &rt, None, 0, &mut rendered));
        h.publish_graph(graph);
        for _ in 0..100 {
            let f = streamed.then_some(&mut next);
            out.extend(lagged_block(&h, &rt, f, 0, &mut rendered));
        }
        out
    };
    let (streamed, arc) = (run(true), run(false));
    let _ = std::fs::remove_file(&path);
    // Only the new node can tell it was overtaken, so the pump learns it from
    // the graph's first block; from the second on, the track must play.
    let from = 95 * BLOCK * CHANNELS;
    assert_eq!(differing_frames(&streamed[from..], &arc[from..]), 0);
}

/// Where a clip that starts INSIDE the loop begins on the timeline (the loop is [0, 12_000)).
const CLIP_IN_LOOP: i64 = 3000;

/// `a - b` from frame `from` on, as "first..last differing rendered frame" for a message.
fn diff_span(a: &[Smp], b: &[Smp], from: usize) -> String {
    let n = a.len() / 2;
    let first = (from..n).find(|&i| a[2 * i] != b[2 * i]);
    let last = (from..n).rev().find(|&i| a[2 * i] != b[2 * i]);
    format!("rendered frames {first:?}..{last:?} differ")
}

#[test]
fn a_stream_starved_across_a_loop_wrap_plays_the_head_of_a_clip_inside_the_loop() {
    // As above, but the clip starts inside the loop. The recovery pump lands
    // while the wrapped render is in the loop's stretch BEFORE the clip, where
    // the node published no park: nothing replanned, and the node waited at
    // its pre-starvation position while the clip's head played silent.
    let pcm = offset_pcm(40_000);
    let path = write_pcm("wrapstarve_in", &pcm);
    let run = |streamed: bool| {
        let (h, rt, mut feeder) = streamed_pair(&path, CLIP_IN_LOOP, 4096);
        let (h2, rt2) = engine_with(Box::new(BufferSource::new(pcm.clone(), CLIP_IN_LOOP)));
        let (h, rt) = if streamed { (h, rt) } else { (h2, rt2) };
        h.set_loop(true, 0, 12_000);
        h.play();
        let mut rendered = 0u64;
        let mut out = Vec::new();
        for b in 0..200 {
            let pump = streamed && !(47..111).contains(&b);
            let f = pump.then_some(&mut feeder);
            out.extend(lagged_block(&h, &rt, f, 0, &mut rendered));
        }
        out
    };
    let (streamed, arc) = (run(true), run(false));
    let _ = std::fs::remove_file(&path);
    let from = 111 * BLOCK; // the first pump after the stall
    let bad = differing_frames(&streamed[from * CHANNELS..], &arc[from * CHANNELS..]);
    assert_eq!(bad, 0, "{}", diff_span(&streamed, &arc, from));
}

#[test]
fn a_commit_primed_just_before_a_loop_wrap_plays_the_head_of_a_clip_inside_the_loop() {
    // As above, with the clip starting inside the loop: the new node is parked
    // on a base the render has just left, and the wrapped render sits in the
    // loop's stretch before the clip, where the node published no park — so
    // the clip's head played silent on the next pass.
    let pcm = offset_pcm(40_000);
    let path = write_pcm("wrapcommit_in", &pcm);
    let run = |streamed: bool| {
        let (h, rt, mut feeder) = streamed_pair(&path, CLIP_IN_LOOP, 16_384);
        let (h2, rt2) = engine_with(Box::new(BufferSource::new(pcm.clone(), CLIP_IN_LOOP)));
        let (mut h, rt) = if streamed { (h, rt) } else { (h2, rt2) };
        h.set_loop(true, 0, 12_000);
        h.play();
        let mut rendered = 0u64;
        let mut out = Vec::new();
        for _ in 0..93 {
            let f = streamed.then_some(&mut feeder);
            out.extend(lagged_block(&h, &rt, f, 0, &mut rendered));
        }
        let (node, mut next) = streamed_source(&path, CLIP_IN_LOOP, 16_384);
        let graph = if streamed {
            next.pump(h.transport()).unwrap();
            source_graph(Box::new(node))
        } else {
            source_graph(Box::new(BufferSource::new(pcm.clone(), CLIP_IN_LOOP)))
        };
        out.extend(lagged_block(&h, &rt, None, 0, &mut rendered));
        h.publish_graph(graph);
        for _ in 0..100 {
            let f = streamed.then_some(&mut next);
            out.extend(lagged_block(&h, &rt, f, 0, &mut rendered));
        }
        out
    };
    let (streamed, arc) = (run(true), run(false));
    let _ = std::fs::remove_file(&path);
    let from = 95 * BLOCK;
    let bad = differing_frames(&streamed[from * CHANNELS..], &arc[from * CHANNELS..]);
    assert_eq!(bad, 0, "{}", diff_span(&streamed, &arc, from));
}

#[test]
fn a_stream_starved_until_exactly_loop_end_plays_the_next_pass() {
    // The ring runs dry mid-pass and the stall ends with the render clock exactly at loop end,
    // about to wrap. The plan's position models the wrap (loop start), which is behind the
    // plan, so the starvation jump never fired — and the node, behind the render rather than
    // ahead of it, reported no park. The next pass played silent up to where the ring ran dry.
    let pcm = offset_pcm(40_000);
    let path = write_pcm("wrapstarve_exact", &pcm);
    let loop_end = 94 * BLOCK as i64; // a block boundary
    let run = |streamed: bool| {
        let (h, rt, mut feeder) = streamed_pair(&path, 0, 4096);
        let (h2, rt2) = engine_with(Box::new(BufferSource::new(pcm.clone(), 0)));
        let (h, rt) = if streamed { (h, rt) } else { (h2, rt2) };
        h.set_loop(true, 0, loop_end);
        h.play();
        let mut rendered = 0u64;
        let mut out = Vec::new();
        for b in 0..250 {
            // No pump from frame 6016 to loop end: the ring runs dry near 9k.
            let pump = streamed && !(47..94).contains(&b);
            let f = pump.then_some(&mut feeder);
            out.extend(lagged_block(&h, &rt, f, 0, &mut rendered));
        }
        out
    };
    let (streamed, arc) = (run(true), run(false));
    let _ = std::fs::remove_file(&path);
    let from = 94 * BLOCK; // the first pump after the stall
    let bad = differing_frames(&streamed[from * CHANNELS..], &arc[from * CHANNELS..]);
    assert_eq!(bad, 0, "{}", diff_span(&streamed, &arc, from));
}

#[test]
fn a_stream_starved_until_loop_end_plays_a_clip_that_ends_inside_the_loop() {
    // The clip ends inside the loop, the ring runs dry mid-clip, and the stall ends with the
    // render exactly at loop end — past the clip, so the node's last frame reported no park,
    // and about to wrap, so the plan's position modelled the wrap and showed no starvation.
    // Nothing replanned: the node waited at its starved position while the next pass played
    // the clip's head as silence.
    let pcm = offset_pcm(8_000);
    let path = write_pcm("wrapstarve_end", &pcm);
    let loop_end = 94 * BLOCK as i64; // a block boundary
    let run = |streamed: bool| {
        let (h, rt, mut feeder) = streamed_pair(&path, 0, 4096);
        let (h2, rt2) = engine_with(Box::new(BufferSource::new(pcm.clone(), 0)));
        let (h, rt) = if streamed { (h, rt) } else { (h2, rt2) };
        h.set_loop(true, 0, loop_end);
        h.play();
        let mut rendered = 0u64;
        let mut out = Vec::new();
        for b in 0..250 {
            // No pump from frame 2560 to loop end: the ring runs dry near 5.6k.
            let pump = streamed && !(20..94).contains(&b);
            let f = pump.then_some(&mut feeder);
            out.extend(lagged_block(&h, &rt, f, 0, &mut rendered));
        }
        out
    };
    let (streamed, arc) = (run(true), run(false));
    let _ = std::fs::remove_file(&path);
    let from = 94 * BLOCK; // the first pump after the stall
    let bad = differing_frames(&streamed[from * CHANNELS..], &arc[from * CHANNELS..]);
    assert_eq!(bad, 0, "{}", diff_span(&streamed, &arc, from));
}

#[test]
fn a_wrap_with_its_fence_queued_is_not_reported_as_a_park() {
    // The pre-span park report must not fire on an ordinary pass: there the
    // wrap's fence is already queued behind the clip's in-loop audio, and a
    // park (read by the pump as a stranded node) would replan — discarding
    // and re-reading the ring — on every pass of the loop.
    let pcm = offset_pcm(40_000);
    let path = write_pcm("wrapqueued", &pcm);
    let (h, rt, mut feeder) = streamed_pair(&path, CLIP_IN_LOOP, 16_384);
    let (h2, rt2) = engine_with(Box::new(BufferSource::new(pcm.clone(), CLIP_IN_LOOP)));
    for e in [&h, &h2] {
        e.set_loop(true, 0, 12_000);
        e.play();
    }
    let ctrl = Arc::clone(&feeder.ctrl);
    let (mut r1, mut r2) = (0u64, 0u64);
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let mut plan_gen = None;
    for _ in 0..12_000 * 5 / BLOCK {
        a.extend(lagged_block(&h, &rt, Some(&mut feeder), 0, &mut r1));
        b.extend(lagged_block(&h2, &rt2, None, 0, &mut r2));
        assert_eq!(
            ctrl.parked.load(std::sync::atomic::Ordering::Acquire),
            0,
            "parked reported at render position {}",
            h.transport().render_pos()
        );
        // No discarding replan after the first plan: five passes, one plan.
        let g = ctrl.gen.load(std::sync::atomic::Ordering::Acquire);
        assert_eq!(*plan_gen.get_or_insert(g), g, "the feeder replanned");
    }
    let _ = std::fs::remove_file(&path);
    assert_eq!(differing_frames(&a, &b), 0, "{}", diff_span(&a, &b, 0));
}

#[test]
fn the_first_callback_after_a_seek_is_whole_on_the_largest_device_buffer() {
    // Steady playback keeps the production ring filled up to its reserve, so a
    // seek's replan lands behind a ring of stale audio: no pump can add a frame
    // until the node drains it in the next callback, which therefore has only
    // what the replan put behind its fence. A quarter of the ring left 4092
    // frames there — silence at the end of a 4096-frame callback, and over
    // 1000 frames of it in the 5120 the output resampler renders for a
    // 4096-frame buffer at 44.1 kHz.
    let frames = 400_000usize;
    let path = write_track(frames);
    for cb in [1024usize, 4096, vocara_engine::feed::MAX_CALLBACK_FRAMES] {
        let (h, rt, mut feeder) = streamed_pair(&path, 0, 16_384);
        let (h2, rt2) = engine_with(Box::new(BufferSource::new(ramp_pcm(frames), 0)));
        h.play();
        h2.play();
        let mut a = vec![0.0; cb * CHANNELS];
        let mut b = vec![0.0; cb * CHANNELS];
        for _ in 0..30 {
            // The pump runs every 10 ms: several rounds per large callback.
            for _ in 0..8 {
                feeder.pump(h.transport()).unwrap();
            }
            rt.render_offline(&mut a, cb);
            rt.transport().sync_consumed_to_rendered();
            rt2.render_offline(&mut b, cb);
            rt2.transport().sync_consumed_to_rendered();
        }
        assert_eq!(
            differing_frames(&a, &b),
            0,
            "steady playback, {cb}-frame callback"
        );
        for _ in 0..4 {
            feeder.pump(h.transport()).unwrap(); // the ring is topped up when the seek lands
        }
        h.seek(200_000);
        h2.seek(200_000);
        for _ in 0..8 {
            feeder
                .pump_at_most(h.transport(), vocara_engine::feed::MAX_CALLBACK_FRAMES)
                .unwrap();
            feeder.pump(h.transport()).unwrap();
        }
        rt.render_offline(&mut a, cb);
        rt2.render_offline(&mut b, cb);
        let bad = differing_frames(&a, &b);
        let first = (0..cb).find(|&i| a[2 * i] != b[2 * i]);
        assert_eq!(
            bad, 0,
            "{cb}-frame callback after the seek: {bad} frames silent from {first:?}"
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// Steady playback of a streamed track and its in-memory twin with `cb`-frame callbacks and
/// eight pump rounds per callback (the production ring stays topped up), then `edits` applied
/// between two callbacks, each followed by a pump round as the woken pump gives it. Returns
/// the frames of the next callback that differ.
fn burst_between_callbacks(cb: usize, edits: &[&dyn Fn(&EngineHandle)]) -> (usize, Option<usize>) {
    let frames = 400_000usize;
    let path = write_track(frames);
    let (h, rt, mut feeder) = streamed_pair(&path, 0, 16_384);
    let (h2, rt2) = engine_with(Box::new(BufferSource::new(ramp_pcm(frames), 0)));
    h.play();
    h2.play();
    let mut a = vec![0.0; cb * CHANNELS];
    let mut b = vec![0.0; cb * CHANNELS];
    for _ in 0..20 {
        for _ in 0..8 {
            feeder.pump(h.transport()).unwrap();
        }
        rt.render_offline(&mut a, cb);
        rt.transport().sync_consumed_to_rendered();
        rt2.render_offline(&mut b, cb);
        rt2.transport().sync_consumed_to_rendered();
    }
    assert_eq!(
        differing_frames(&a, &b),
        0,
        "steady playback, {cb}-frame callback"
    );
    for edit in edits {
        edit(&h);
        edit(&h2);
        feeder
            .pump_at_most(h.transport(), vocara_engine::feed::MAX_CALLBACK_FRAMES)
            .unwrap();
        feeder.pump(h.transport()).unwrap();
    }
    rt.render_offline(&mut a, cb);
    rt2.render_offline(&mut b, cb);
    let _ = std::fs::remove_file(&path);
    (
        differing_frames(&a, &b),
        (0..cb).find(|&i| a[2 * i] != b[2 * i]),
    )
}

#[test]
fn several_seeks_between_two_callbacks_leave_the_next_one_whole() {
    // A ruler drag, or quick seeks: every seek replans, and each replan's fence used to stack
    // behind the queue the one before it had not yet drained. The first replan's audio filled
    // the ring's free space, so the second fence landed with nothing behind it and the whole
    // next callback was silent (1024, 4096 and 5120 frames of it).
    let seeks: [&dyn Fn(&EngineHandle); 4] = [
        &|h| h.seek(200_000),
        &|h| h.seek(201_000),
        &|h| h.seek(150_000),
        &|h| h.seek(260_000),
    ];
    let mut bad = Vec::new();
    for cb in [1024usize, 4096, vocara_engine::feed::MAX_CALLBACK_FRAMES] {
        for n in 2..=seeks.len() {
            let (diff, first) = burst_between_callbacks(cb, &seeks[..n]);
            if diff > 0 {
                bad.push(format!(
                    "{n} seeks, {cb}-frame callback: {diff} frames differ from {first:?}"
                ));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn a_seek_and_a_loop_edit_between_two_callbacks_leave_the_next_one_whole() {
    // A loop edit is a discarding replan too.
    let edits: [&dyn Fn(&EngineHandle); 3] = [
        &|h| h.seek(100_000),
        &|h| h.set_loop(true, 99_000, 140_000),
        &|h| h.set_loop(true, 99_000, 120_000),
    ];
    for cb in [1024usize, vocara_engine::feed::MAX_CALLBACK_FRAMES] {
        let (diff, first) = burst_between_callbacks(cb, &edits);
        assert_eq!(
            diff, 0,
            "{cb}-frame callback: {diff} frames differ from {first:?}"
        );
    }
}

#[test]
fn a_pump_running_beside_the_render_never_hands_it_a_misplaced_frame() {
    // Every discarding replan takes the queue back while the node may be rendering on another
    // thread; the pop limit is all that keeps the two apart. Under a real concurrent pump and
    // bursts of seeks, every frame the render plays must be the track's own frame for its
    // timeline position — or an underrun's silence, never another position's audio.
    let frames = 400_000usize;
    let path = write_track(frames);
    let pcm = ramp_pcm(frames);
    let (h, rt, mut feeder) = streamed_pair(&path, 0, 16_384);
    let transport = Arc::clone(h.shared_arc());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pump = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                feeder.pump(&transport).unwrap();
                std::thread::yield_now();
            }
        })
    };
    h.play();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let mut tl = 0i64;
    let (mut played, mut silent, mut misplaced) = (0usize, 0usize, Vec::new());
    let mut out = vec![0.0; BLOCK * 4 * CHANNELS];
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    while std::time::Instant::now() < deadline {
        if next(2) == 0 {
            for _ in 0..1 + next(4) {
                tl = next(300_000) as i64;
                h.seek(tl);
                std::thread::sleep(std::time::Duration::from_micros(next(300)));
            }
        }
        // As fast as it goes (no real-time pacing): the more blocks overlap the pump's
        // replans, the more often the limit is put to the test. A debug build's pump falls
        // behind and much of it is underrun silence, which is fine here: misplaced audio is
        // what fails.
        rt.render_offline(&mut out, BLOCK * 4);
        rt.transport().sync_consumed_to_rendered();
        for (i, f) in out.chunks(2).enumerate() {
            let at = tl as usize + i;
            if f[0] == 0.0 && f[1] == 0.0 {
                silent += 1;
            } else if f[0] == pcm[2 * at] && f[1] == pcm[2 * at + 1] {
                played += 1;
            } else if misplaced.len() < 5 {
                misplaced.push(format!(
                    "tl {at}: {:?} expected {:?}",
                    f,
                    &pcm[2 * at..2 * at + 2]
                ));
            }
        }
        tl += (BLOCK * 4) as i64;
    }
    stop.store(true, std::sync::atomic::Ordering::Release);
    pump.join().unwrap();
    let _ = std::fs::remove_file(&path);
    eprintln!("{played} frames played, {silent} silent");
    assert!(misplaced.is_empty(), "misplaced frames: {misplaced:?}");
    assert!(
        played * 50 > played + silent,
        "the stream barely played: {played} played, {silent} silent"
    );
}

/// Loop-plays [0, 12_000), pumping before every block, except that at block
/// `at` it applies `edit` and lets only the replan's FENCE reach the ring
/// before that block renders: the audio behind it lands after.
fn audio_behind_a_late_fence(
    streamed: bool,
    path: &std::path::Path,
    pcm: &[Smp],
    at: usize,
    edit: &dyn Fn(&EngineHandle),
) -> Vec<Smp> {
    let (h, rt, mut feeder) = streamed_pair(path, 0, 16_384);
    let (h2, rt2) = engine_with(Box::new(BufferSource::new(pcm.to_vec(), 0)));
    let (h, rt) = if streamed { (h, rt) } else { (h2, rt2) };
    h.set_loop(true, 0, 12_000);
    h.play();
    let mut rendered = 0u64;
    let mut out = Vec::new();
    for b in 0..at + 120 {
        let mut f = streamed.then_some(&mut feeder);
        if b == at {
            edit(&h);
            if let Some(f) = f.take() {
                f.pump_at_most(h.transport(), 0).unwrap();
            }
        }
        out.extend(lagged_block(&h, &rt, f, 0, &mut rendered));
    }
    out
}

#[test]
fn a_replan_a_callback_short_of_loop_end_plays_the_next_pass() {
    // The replan's fence reached the ring before the callback; the audio
    // behind it did not. The node adopted the plan's base, ran dry to loop
    // end, wrapped — and then waited on that base for a whole pass.
    let pcm = offset_pcm(40_000);
    let path = write_pcm("lateaudio", &pcm);
    let check = |at: usize, what: &dyn Fn(&EngineHandle)| {
        let streamed = audio_behind_a_late_fence(true, &path, &pcm, at, what);
        let arc = audio_behind_a_late_fence(false, &path, &pcm, at, what);
        // The edit's own block is the accepted cost of a callback that beats
        // the audio; every block after it must play.
        let from = (at + 1) * BLOCK * CHANNELS;
        let bad = differing_frames(&streamed[from..], &arc[from..]);
        assert_eq!(bad, 0, "edit at block {at}");
    };
    // A seek 50 frames short of loop end, and a loop edit that moves loop end
    // to 86 frames past the playhead (block 93 starts at 11_904).
    check(40, &|h| h.seek(11_950));
    check(93, &|h| h.set_loop(true, 0, 11_990));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_park_on_a_pending_seek_is_left_to_wait() {
    // A starved node can adopt a seek's fence in band, inside a block that
    // began before the seek: it then sits parked AHEAD of the transport until
    // the next block applies the seek. That park is legitimate — replanning it
    // away would cost the seek its first callback. One the render has passed
    // is not.
    use std::sync::atomic::Ordering;
    let path = write_track(40_000);
    let (h, rt, mut feeder) = streamed_pair(&path, 0, 16_384);
    h.play();
    let _ = render_streamed(&h, &rt, &mut feeder, 4);
    h.seek(30_000);
    feeder.pump(h.transport()).unwrap(); // the seek's replan
    let ctrl = Arc::clone(&feeder.ctrl);
    let generation = ctrl.gen.load(Ordering::Relaxed);
    let flushes = ctrl.flush_seq.load(Ordering::Relaxed);
    // What the node reports from that block: parked at the seek's base.
    ctrl.parked
        .store(parked_word(generation, 30_000), Ordering::Release);
    feeder.pump(h.transport()).unwrap();
    assert_eq!(
        ctrl.flush_seq.load(Ordering::Relaxed),
        flushes,
        "a park on the pending seek was replanned"
    );
    let _ = render_streamed(&h, &rt, &mut feeder, 1); // applies the seek
    ctrl.parked
        .store(parked_word(generation, 39_000), Ordering::Release);
    feeder.pump(h.transport()).unwrap();
    assert_ne!(
        ctrl.flush_seq.load(Ordering::Relaxed),
        flushes,
        "a park the render has passed must be replanned"
    );
    let _ = std::fs::remove_file(&path);
}

/// A source that pauses the transport from INSIDE the render, between the
/// block's playing check and its end-of-block clock store — the window a
/// control-thread pause lands in whenever a callback is rendering.
struct PausesMidBlock {
    inner: BufferSource,
    /// The handle to pause through on the next block (null: none).
    handle: Arc<std::sync::atomic::AtomicPtr<EngineHandle>>,
}

impl vocara_engine::node::Node for PausesMidBlock {
    fn prepare(&mut self, sample_rate: f64, max_block: usize) {
        self.inner.prepare(sample_rate, max_block);
    }

    fn num_in(&self) -> usize {
        0
    }

    fn process(&mut self, io: &mut vocara_engine::node::ProcessIo) {
        self.inner.process(io);
        let h = self
            .handle
            .swap(std::ptr::null_mut(), std::sync::atomic::Ordering::AcqRel);
        // SAFETY: the test thread set it to a live handle it is not using
        // while the render runs; only Shared atomics are touched.
        if let Some(h) = unsafe { h.as_ref() } {
            h.pause();
        }
    }
}

#[test]
fn a_pause_during_a_render_resumes_where_the_listener_stopped() {
    // Resampled output path: the device trails the render by LAG frames. A
    // pause folds the clock back to what was heard, but a callback already
    // rendering stored ITS block end over the fold, so resume skipped the
    // unheard backlog the pause's flush had dropped (up to a render block).
    const LAG: u64 = 1024;
    let pcm = offset_pcm(100_000);
    let handle = Arc::new(std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()));
    let node = PausesMidBlock {
        inner: BufferSource::new(pcm.clone(), 0),
        handle: Arc::clone(&handle),
    };
    let (h, rt) = engine_with(Box::new(node));
    let t = rt.transport();
    let mut rendered = 0u64;
    let mut block = vec![0.0; BLOCK * CHANNELS];
    h.play();
    for _ in 0..100 {
        rt.render_offline(&mut block, BLOCK);
        rendered += BLOCK as u64;
        let target = rendered.saturating_sub(LAG);
        let cur = t.consumed_frames();
        if target > cur {
            t.advance_consumed(target - cur);
        }
    }
    let heard = (rendered - LAG) as i64;
    handle.store(
        &h as *const EngineHandle as *mut EngineHandle,
        std::sync::atomic::Ordering::Release,
    );
    rt.render_offline(&mut block, BLOCK); // the pause lands mid-block
    assert!(!h.is_playing());
    assert_eq!(h.frame(), heard, "the paused clock must hold the fold");
    // The callback processes the flush (the cursor skips the dropped FIFO),
    // then play resumes on the first frame the listener did not hear.
    t.advance_consumed(rendered + BLOCK as u64 - t.consumed_frames());
    h.play();
    rt.render_offline(&mut block, BLOCK);
    assert_eq!(
        block[0],
        pcm[heard as usize * CHANNELS],
        "resumed elsewhere"
    );
}
