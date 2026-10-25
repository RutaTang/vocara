//! A streamed clip the playhead has already passed keeps a two-frame jump fence in its ring
//! (out of span, the node never pops it). The next discarding replan's fence landed behind those
//! two stale frames, the fill took them for a backlog about to be freed wholesale and filled the
//! ring to all but two slots, and while the clip was not consuming (before its span, or paused)
//! the ring stayed that full: the NEXT replan found room for two frames of audio behind its
//! fence, and the first callback into the clip played two frames and then silence.

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

const BLOCK: usize = 512; // a typical device callback
const RING: usize = 16384; // ve_tracks_commit_staged's ring

fn pcm(frames: usize) -> Vec<Smp> {
    let mut v = Vec::with_capacity(frames * CHANNELS);
    for i in 0..frames {
        let s = ((i as Smp) * 0.01).sin() * 0.5 + 0.1; // never 0
        v.push(s);
        v.push(s);
    }
    v
}

fn engine(node: Box<dyn vocara_engine::node::Node + Send>) -> (EngineHandle, Arc<RtCell>) {
    let g = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, node),
            NodeEntry::new(2, Box::new(MasterNode::unity())),
        ],
        edges: vec![Edge::simple(1, 2)],
        master: 2,
    }
    .compile(ENGINE_SR, BLOCK)
    .unwrap();
    let (_mp, mc) = spsc::<[Smp; CHANNELS]>(256);
    let (h, rt) = spawn(g, HashMap::new(), mc, 256);
    (h, RtCell::new(rt))
}

fn render(rt: &Arc<RtCell>) -> Vec<Smp> {
    let mut out = vec![0.0; BLOCK * CHANNELS];
    rt.render_offline(&mut out, BLOCK);
    rt.transport().sync_consumed_to_rendered();
    out
}

/// The real pump round (ffi::pump_round): a head pass of 4096 frames, then a top-up.
fn pump_round(f: &mut TrackFeeder, h: &EngineHandle) {
    f.pump_at_most(h.transport(), 4096).unwrap();
    f.pump(h.transport()).unwrap();
}

struct Rig {
    h: EngineHandle,
    rt: Arc<RtCell>,
    feeder: TrackFeeder,
    h2: EngineHandle,
    rt2: Arc<RtCell>,
    path: std::path::PathBuf,
}

fn rig(tag: &str, start: i64, frames: usize) -> Rig {
    let path = std::env::temp_dir().join(format!("passed_clip_{tag}_{}.wav", std::process::id()));
    write_wav_f32(&path, &pcm(frames), CHANNELS as u16, ENGINE_SR as u32).unwrap();
    let ctrl = StreamCtrl::new();
    let (prod, cons) = spsc::<[Smp; CHANNELS]>(RING);
    let reader = WavTrackReader::open(path.to_str().unwrap()).unwrap();
    let len = reader.frames;
    let node = StreamingSource::new(Arc::clone(&ctrl), cons, start, len);
    let feeder = TrackFeeder::new(ctrl, prod, reader, start);
    let (h, rt) = engine(Box::new(node));
    let (h2, rt2) = engine(Box::new(BufferSource::new(pcm(frames), start)));
    Rig {
        h,
        rt,
        feeder,
        h2,
        rt2,
        path,
    }
}

impl Rig {
    /// One device callback on both engines, the feeder pumped first when `pump`.
    fn cb(&mut self, pump: bool) -> (usize, usize) {
        if pump {
            pump_round(&mut self.feeder, &self.h);
        }
        let a = render(&self.rt);
        let b = render(&self.rt2);
        let silent = a
            .chunks(2)
            .zip(b.chunks(2))
            .filter(|(x, y)| x[0] == 0.0 && y[0] != 0.0)
            .count();
        let diff = a.chunks(2).zip(b.chunks(2)).filter(|(x, y)| x != y).count();
        (silent, diff)
    }
    fn seek(&self, p: i64) {
        self.h.seek(p);
        self.h2.seek(p);
    }
}

/// While playing: play past a clip, seek back to before it, then click into it.
#[test]
fn a_seek_into_a_clip_already_passed_plays_from_its_first_frame() {
    let start = 48_000; // a 2 s clip at 1 s
    let mut r = rig("play", start, 96_000);
    r.h.play();
    r.h2.play();
    // From 0 to about 3.5 s: through the whole clip and past its end.
    for _ in 0..330 {
        r.cb(true);
    }
    // Back to 0.2 s, before the clip: the woken pump replans, then callbacks run.
    r.seek(9_600);
    for _ in 0..10 {
        r.cb(true);
    }
    // Into the clip at 1.5 s: the woken pump runs first, then the callback.
    r.seek(72_000);
    let (silent, diff) = r.cb(true);
    let _ = std::fs::remove_file(&r.path);
    assert_eq!(
        diff, 0,
        "the seek into the clip opened on {silent} frames of silence"
    );
}

/// Stop (a pause and a seek to the start, back to back) after playback passed the clip, then a
/// take started from inside it: a seek and a play, back to back.
#[test]
fn a_take_started_inside_a_clip_after_stop_plays_from_its_first_frame() {
    let start = 48_000;
    let mut r = rig("stop", start, 96_000);
    r.h.play();
    r.h2.play();
    for _ in 0..330 {
        r.cb(true);
    }
    r.h.pause();
    r.h2.pause();
    r.seek(0);
    for _ in 0..20 {
        r.cb(true);
    }
    r.seek(60_000);
    r.h.play();
    r.h2.play();
    let (silent, diff) = r.cb(true);
    let _ = std::fs::remove_file(&r.path);
    assert_eq!(
        diff, 0,
        "the take's first callback opened on {silent} frames of silence"
    );
}
