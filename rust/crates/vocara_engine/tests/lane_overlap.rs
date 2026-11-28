//! Overlapping clips on one lane, as the host stages them: the clip below plays only the spans
//! of it the clip above leaves, from the right offset into its file, its cut edges faded. The
//! render must be the top clip alone in the overlap, the lower clip outside it, and continuous
//! at every join, from decoded buffers and from disk-streamed files alike.

use std::collections::HashMap;
use std::sync::Arc;

use vocara_engine::build::{build, ProjectSpec, TrackSpec};
use vocara_engine::feed::{StreamCtrl, TrackFeeder, WavTrackReader};
use vocara_engine::live::{spawn, EngineHandle, RtCell};
use vocara_engine::node::Node;
use vocara_engine::ring::spsc;
use vocara_engine::sample::{Smp, CHANNELS, ENGINE_SR};
use vocara_engine::source::{BufferSource, Cut, StreamingSource, CUT_FADE_FRAMES};
use vocara_engine::wav::write_wav_f32;

const BLOCK: usize = 128;
/// The lower clip: a second of 220 Hz at timeline 0.
const LOW_LEN: usize = 48_000;
/// The clip above it: a quarter second of 320 Hz (whole cycles, so its own ends sit near
/// zero, as a take's do) from 0.25 s.
const TOP_AT: usize = 12_000;
const TOP_LEN: usize = 12_000;

fn tone(hz: f64, amp: f64, frames: usize) -> Vec<Smp> {
    (0..frames * CHANNELS)
        .map(|i| (amp * (std::f64::consts::TAU * hz * (i / 2) as f64 / ENGINE_SR).sin()) as Smp)
        .collect()
}

fn low() -> Vec<Smp> {
    tone(220.0, 0.8, LOW_LEN)
}

fn top() -> Vec<Smp> {
    tone(320.0, 0.6, TOP_LEN)
}

/// The spans the host stages for the lane, as (timeline start, file offset, length, cut):
/// the lower clip up to the top one, the top one whole, the lower clip after it.
fn spans() -> [(usize, usize, usize, Cut); 3] {
    let end = TOP_AT + TOP_LEN;
    [
        (
            0,
            0,
            TOP_AT,
            Cut {
                fade_in: false,
                fade_out: true,
            },
        ),
        (TOP_AT, 0, TOP_LEN, Cut::default()),
        (
            end,
            end,
            LOW_LEN - end,
            Cut {
                fade_in: true,
                fade_out: false,
            },
        ),
    ]
}

/// One lane (a backing track, so nothing but the sources and the fader) of `sources`, the
/// first leading the chain.
fn lane(mut sources: Vec<Box<dyn Node>>) -> (EngineHandle, Arc<RtCell>) {
    let first = sources.remove(0);
    let mut spec = TrackSpec::backing(1, first);
    spec.lane_sources = sources
        .into_iter()
        .enumerate()
        .map(|(i, s)| (i as u32 + 2, s))
        .collect();
    let mut project = ProjectSpec::new(vec![spec]);
    project.master_limit = false;
    let g = build(project).compile(ENGINE_SR, BLOCK).unwrap();
    let (_mic, mic_cons) = spsc::<[Smp; CHANNELS]>(256);
    let (h, rt) = spawn(g, HashMap::new(), mic_cons, 256);
    h.set_declick(false);
    (h, RtCell::new(rt))
}

fn render(h: &EngineHandle, rt: &Arc<RtCell>, feeders: &mut [TrackFeeder]) -> Vec<Smp> {
    h.play();
    let blocks = LOW_LEN / BLOCK + 2;
    let mut out = vec![0.0; blocks * BLOCK * CHANNELS];
    for b in 0..blocks {
        for f in feeders.iter_mut() {
            f.pump(h.transport()).unwrap();
        }
        let s = b * BLOCK * CHANNELS;
        rt.render_offline(&mut out[s..s + BLOCK * CHANNELS], BLOCK);
        rt.transport().sync_consumed_to_rendered();
    }
    out
}

fn check(out: &[Smp]) {
    let (low, top) = (low(), top());
    let end = TOP_AT + TOP_LEN;
    let fade = CUT_FADE_FRAMES as usize;
    // The overlap: the top clip alone.
    for f in TOP_AT..end {
        let want = top[(f - TOP_AT) * CHANNELS];
        assert!(
            (out[f * CHANNELS] - want).abs() < 1e-6,
            "frame {f}: {} where the top clip alone is {want}",
            out[f * CHANNELS]
        );
    }
    // Outside it, past the cut fades: the lower clip as recorded.
    for f in (0..TOP_AT - fade).chain(end + fade..LOW_LEN) {
        assert!(
            (out[f * CHANNELS] - low[f * CHANNELS]).abs() < 1e-6,
            "frame {f}: the lower clip is not heard"
        );
    }
    // Every join is continuous: no step past the tones' own (~0.024 a frame).
    let worst = out[..LOW_LEN * CHANNELS]
        .chunks(CHANNELS)
        .zip(out[CHANNELS..LOW_LEN * CHANNELS].chunks(CHANNELS))
        .map(|(a, b)| (b[0] - a[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 0.05, "a join steps by {worst}");
}

#[test]
fn decoded_clips_play_only_the_top_one_where_they_overlap() {
    let sources: Vec<Box<dyn Node>> = spans()
        .into_iter()
        .map(|(at, from, len, cut)| {
            let pcm = if at == TOP_AT { top() } else { low() };
            Box::new(BufferSource::new(pcm, at as i64).with_span(
                from as i64,
                Some(len as i64),
                cut,
            )) as Box<dyn Node>
        })
        .collect();
    let (h, rt) = lane(sources);
    check(&render(&h, &rt, &mut []));

    // Both clips whole, as the lane used to play them: the overlap sounds both.
    let (h, rt) = lane(vec![
        Box::new(BufferSource::new(low(), 0)),
        Box::new(BufferSource::new(top(), TOP_AT as i64)),
    ]);
    let both = render(&h, &rt, &mut []);
    let f = TOP_AT + 100;
    assert!((both[f * CHANNELS] - top()[100 * CHANNELS]).abs() > 0.1);
}

#[test]
fn streamed_clips_play_only_the_top_one_where_they_overlap() {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let file = |pcm: &[Smp]| {
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "vocara_lane_overlap_{}_{seq}.wav",
            std::process::id()
        ));
        write_wav_f32(&path, pcm, CHANNELS as u16, ENGINE_SR as u32).unwrap();
        path
    };
    let (low_path, top_path) = (file(&low()), file(&top()));
    let mut feeders = Vec::new();
    let mut sources: Vec<Box<dyn Node>> = Vec::new();
    for (at, from, len, cut) in spans() {
        let path = if at == TOP_AT { &top_path } else { &low_path };
        let reader = WavTrackReader::open(path.to_str().unwrap()).unwrap();
        let ctrl = StreamCtrl::new();
        let (prod, cons) = spsc::<[Smp; CHANNELS]>(8192);
        let feeder = TrackFeeder::new(Arc::clone(&ctrl), prod, reader, at as i64).with_span(
            from as i64,
            Some(len as i64),
            cut,
        );
        sources.push(Box::new(StreamingSource::new(
            ctrl,
            cons,
            at as i64,
            feeder.len_frames(),
        )));
        feeders.push(feeder);
    }
    let (h, rt) = lane(sources);
    check(&render(&h, &rt, &mut feeders));
    let _ = std::fs::remove_file(low_path);
    let _ = std::fs::remove_file(top_path);
}
