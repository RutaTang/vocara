//! What a seek costs the streamed clips the playhead has not reached yet. Every clip's ring is
//! filled ahead with the clip's first frames, however far away the clip starts, and a seek's
//! discarding replan used to land its fence behind all of them: on the audio thread each node
//! popped its whole queue frame by frame to reach the fence, and the pump read the same frames
//! from disk again. With a hundred clips that was ~1.2 M pops in one callback, on every seek of
//! a ruler drag. A plan that starts before a clip's span queues exactly what any other such
//! plan would, so a seek that stays before the clip keeps it: nothing drained, nothing re-read.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use vocara_engine::dsp::master::MasterNode;
use vocara_engine::feed::{StreamCtrl, TrackFeeder, WavTrackReader};
use vocara_engine::graph::{Edge, GraphDesc, NodeEntry};
use vocara_engine::live::{spawn, EngineHandle, RtCell};
use vocara_engine::node::Node;
use vocara_engine::ring::spsc;
use vocara_engine::sample::{Smp, CHANNELS, ENGINE_SR};
use vocara_engine::source::{BufferSource, StreamingSource};
use vocara_engine::wav::write_wav_f32;

const BLOCK: usize = 512; // a typical device callback
const RING: usize = 16384; // ve_tracks_commit_staged's ring
const CLIPS: usize = 6;
const CLIP_FRAMES: usize = 48_000;

fn pcm(frames: usize) -> Vec<Smp> {
    let mut v = Vec::with_capacity(frames * CHANNELS);
    for i in 0..frames {
        let s = ((i as Smp) * 0.01).sin() * 0.5 + 0.1; // never 0
        v.push(s);
        v.push(-s);
    }
    v
}

/// Clip `c` starts at 2 s + 1.5 s * c: all of them ahead of the first 2 s of the song.
fn clip_start(c: usize) -> i64 {
    96_000 + 72_000 * c as i64
}

fn engine(sources: Vec<Box<dyn Node + Send>>) -> (EngineHandle, Arc<RtCell>) {
    let mut nodes = vec![NodeEntry::new(0, Box::new(MasterNode::unity()))];
    let mut edges = Vec::new();
    for (i, s) in sources.into_iter().enumerate() {
        let id = (i + 1) as u32;
        nodes.push(NodeEntry::new(id, s));
        edges.push(Edge::simple(id, 0));
    }
    let g = GraphDesc {
        nodes,
        edges,
        master: 0,
    }
    .compile(ENGINE_SR, BLOCK)
    .unwrap();
    let (_mp, mc) = spsc::<[Smp; CHANNELS]>(256);
    let (h, rt) = spawn(g, HashMap::new(), mc, 256);
    (h, RtCell::new(rt))
}

struct Rig {
    h: EngineHandle,
    rt: Arc<RtCell>,
    feeders: Vec<TrackFeeder>,
    ctrls: Vec<Arc<StreamCtrl>>,
    h2: EngineHandle,
    rt2: Arc<RtCell>,
    path: std::path::PathBuf,
    /// Frames where the streamed render differed from the in-memory one.
    diff: usize,
}

fn rig() -> Rig {
    let path = std::env::temp_dir().join(format!("seek_cost_{}.wav", std::process::id()));
    write_wav_f32(&path, &pcm(CLIP_FRAMES), CHANNELS as u16, ENGINE_SR as u32).unwrap();
    let mut streamed: Vec<Box<dyn Node + Send>> = Vec::new();
    let mut in_memory: Vec<Box<dyn Node + Send>> = Vec::new();
    let mut feeders = Vec::new();
    let mut ctrls = Vec::new();
    for c in 0..CLIPS {
        let ctrl = StreamCtrl::new();
        let (prod, cons) = spsc::<[Smp; CHANNELS]>(RING);
        let reader = WavTrackReader::open(path.to_str().unwrap()).unwrap();
        let len = reader.frames;
        streamed.push(Box::new(StreamingSource::new(
            Arc::clone(&ctrl),
            cons,
            clip_start(c),
            len,
        )));
        in_memory.push(Box::new(BufferSource::new(pcm(CLIP_FRAMES), clip_start(c))));
        feeders.push(TrackFeeder::new(
            Arc::clone(&ctrl),
            prod,
            reader,
            clip_start(c),
        ));
        ctrls.push(ctrl);
    }
    let (h, rt) = engine(streamed);
    let (h2, rt2) = engine(in_memory);
    Rig {
        h,
        rt,
        feeders,
        ctrls,
        h2,
        rt2,
        path,
        diff: 0,
    }
}

impl Rig {
    /// One device callback on both engines, every feeder pumped first as the woken pump
    /// round does (ffi::pump_round: a 4096-frame head pass, then a top-up).
    fn callback(&mut self) {
        for f in &mut self.feeders {
            f.pump_at_most(self.h.transport(), 4096).unwrap();
        }
        for f in &mut self.feeders {
            f.pump(self.h.transport()).unwrap();
        }
        let mut a = vec![0.0; BLOCK * CHANNELS];
        let mut b = vec![0.0; BLOCK * CHANNELS];
        self.rt.render_offline(&mut a, BLOCK);
        self.rt.transport().sync_consumed_to_rendered();
        self.rt2.render_offline(&mut b, BLOCK);
        self.rt2.transport().sync_consumed_to_rendered();
        self.diff += a
            .chunks(CHANNELS)
            .zip(b.chunks(CHANNELS))
            .filter(|(x, y)| x != y)
            .count();
    }
    fn seek(&self, p: i64) {
        self.h.seek(p);
        self.h2.seek(p);
    }
    fn play(&self) {
        self.h.play();
        self.h2.play();
    }
    fn pause(&self) {
        self.h.pause();
        self.h2.pause();
    }
    fn frames_read(&self) -> u64 {
        self.feeders.iter().map(|f| f.frames_read()).sum()
    }
    /// Discarding replans so far: each one makes its node drain its whole queue.
    fn drains(&self) -> u32 {
        self.ctrls
            .iter()
            .map(|c| c.flush_seq.load(Ordering::Acquire))
            .sum()
    }
}

#[test]
fn a_seek_that_stays_before_the_clips_neither_drains_nor_rereads_them() {
    let mut r = rig();
    r.play();
    for _ in 0..20 {
        r.callback(); // every ring now holds its clip's first frames
    }
    // A scrub over the first 2 s, playing and paused: every target is ahead of every clip.
    let (read0, drains0) = (r.frames_read(), r.drains());
    let mut seeks = 0;
    for (k, target) in [24_000i64, 9_600, 48_000, 4_800, 72_000, 36_000, 0, 60_000]
        .into_iter()
        .enumerate()
    {
        if k == 4 {
            r.pause();
            r.callback();
        }
        r.seek(target);
        seeks += 1;
        for _ in 0..6 {
            r.callback();
        }
        if k == 6 {
            r.play();
            r.callback();
        }
    }
    let reread = (r.frames_read() - read0) / seeks;
    let drained = r.drains() - drains0;
    eprintln!(
        "{CLIPS} clips ahead: {reread} frames re-read per seek, {drained} discarding replans \
         over {seeks} seeks"
    );
    assert_eq!(
        reread, 0,
        "a seek that stays before every clip re-read {reread} frames of them"
    );
    assert_eq!(
        drained, 0,
        "a seek that stays before every clip made the nodes drain {drained} times"
    );

    // The clips still play bit-exact: from 1.25 s through the first two clips and into the
    // third, with a seek INTO a clip on the way (that one must replan).
    r.seek(60_000);
    let into_span = r.drains();
    for _ in 0..250 {
        r.callback();
    }
    r.seek(clip_start(2) + 10_000);
    for _ in 0..60 {
        r.callback();
    }
    assert!(
        r.drains() > into_span,
        "a seek into a clip must still replan"
    );
    let _ = std::fs::remove_file(&r.path);
    assert_eq!(
        r.diff, 0,
        "{} frames differ from the in-memory render",
        r.diff
    );
}
