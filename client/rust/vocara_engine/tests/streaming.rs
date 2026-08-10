//! Disk-streaming sources (P1): the streamed render must EQUAL the
//! Arc-backed render bit-for-bit, discontinuities must resynchronize on the
//! fence, and starvation must degrade to counted silence that self-heals —
//! the invariants docs/audio-streaming-plan.md pins.

use std::collections::HashMap;
use std::sync::Arc;

use vocara_engine::dsp::master::MasterNode;
use vocara_engine::feed::{fence_frames, StreamCtrl, TrackFeeder, WavTrackReader};
use vocara_engine::graph::{Edge, GraphDesc, NodeEntry};
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
