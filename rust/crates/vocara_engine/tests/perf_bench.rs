//! Real-time engine timing loops (criterion-free). Ignored by default; run them with
//!
//! ```text
//! cargo test --release --test perf_bench -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Each prints the fastest of several timed batches, and for the render paths the share
//! of real time one block of audio represents at 48 kHz. They assert nothing about speed (a
//! shared CI box is not a benchmark rig); they exist so a performance claim can be re-measured.
//! The decode bench needs `--features device`; the output resampler's timing loop lives with
//! its tests in `device.rs` (`bench_output_resampler`).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use vocara_engine::aec::Aec;
use vocara_engine::build::{
    self, bus_comp_id, comp_id, deesser_id, delay_id, doubler_id, eq_id, gate_id, limiter_id,
    reverb_id, saturation_id, BusSpec, ProjectSpec, TrackSpec,
};
use vocara_engine::dsp::biquad::{param as eqp, EqNode};
use vocara_engine::dsp::compressor::{param as cp, CompressorNode};
use vocara_engine::dsp::deesser::{param as dp, DeEsserNode};
use vocara_engine::dsp::delay::{param as dlp, DelayNode};
use vocara_engine::dsp::denormals::FlushGuard;
use vocara_engine::dsp::doubler::{param as dbp, DoublerNode};
use vocara_engine::dsp::gate::{param as gp, GateNode};
use vocara_engine::dsp::limiter::{param as lp, LimiterNode};
use vocara_engine::dsp::master::MasterNode;
use vocara_engine::dsp::reverb::{param as rp, ReverbNode};
use vocara_engine::dsp::saturation::{param as sp, SaturationNode};
use vocara_engine::feed::{fence_frames, StreamCtrl};
use vocara_engine::graph::Graph;
use vocara_engine::node::{Node, ProcessIo};
use vocara_engine::ring::spsc;
use vocara_engine::sample::{Smp, CHANNELS, ENGINE_SR};
use vocara_engine::source::{BufferSource, StreamingSource};

const SR: f64 = ENGINE_SR;

/// Deterministic "voice-like" stereo test material: a few partials plus low-level noise.
fn material(frames: usize, seed: u32) -> Vec<Smp> {
    let mut x = seed | 1;
    let mut v = Vec::with_capacity(frames * CHANNELS);
    for i in 0..frames {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let n = (x as f32 / u32::MAX as f32 - 0.5) * 0.02;
        let t = i as f32 / SR as f32;
        let s = 0.3 * (t * 220.0 * std::f32::consts::TAU).sin()
            + 0.1 * (t * 660.0 * std::f32::consts::TAU).sin()
            + 0.05 * (t * 5_100.0 * std::f32::consts::TAU).sin();
        let env = 0.5 + 0.5 * (t * 1.3 * std::f32::consts::TAU).sin();
        v.push(s * env + n);
        v.push(s * env * 0.8 - n);
    }
    v
}

/// The fastest of `batches` timed runs of `f`, in nanoseconds per call (`per` calls per
/// batch). The minimum, not the median: on a shared machine every disturbance only ever adds
/// time, so the fastest batch is the best estimate of the code's own cost.
fn best_ns(batches: usize, per: usize, mut f: impl FnMut()) -> f64 {
    let _fz = FlushGuard::new(); // the audio thread always renders under flush-to-zero
    for _ in 0..per {
        f(); // warm-up
    }
    (0..batches)
        .map(|_| {
            let s = Instant::now();
            for _ in 0..per {
                f();
            }
            s.elapsed().as_nanos() as f64 / per as f64
        })
        .fold(f64::MAX, f64::min)
}

fn report(name: &str, ns: f64, frames: usize) {
    let block_ns = frames as f64 / SR * 1e9;
    println!(
        "{name:<58} {:>10.2} us/block  {:>6.2}% of real time",
        ns / 1e3,
        100.0 * ns / block_ns
    );
}

/// A project the app actually builds: `tracks` vocal tracks, each with the full nine-insert
/// chain the Dart side stages (EQ, comp, de-esser, gate, delay, saturation, doubler, limiter,
/// reverb insert) and two aux sends, into two FX buses (EQ -> comp), the shared reverb and the
/// limited master. `active` switches every insert on with non-trivial settings; otherwise each
/// sits at its build default (EQ flat, the rest bypassed, the reverb insert at wet 0).
fn project(tracks: u32, active: bool, max_block: usize) -> Graph {
    let len = 48_000 * 4;
    let mut ts = Vec::new();
    for t in 0..tracks {
        let src = BufferSource::new(material(len, 1 + t), 0);
        let mut spec = TrackSpec::vocal(t, Box::new(src));
        spec.chain = (0..9).collect();
        spec.send_db = -12.0;
        spec.gain = 0.8;
        spec.pan = (t as f32 / tracks as f32) * 2.0 - 1.0;
        spec.bus = if t % 3 == 0 { Some(0) } else { None };
        spec.sends = vec![Some((0, 0.3)), Some((1, 0.2))];
        ts.push(spec);
    }
    let buses = (0..2)
        .map(|b| BusSpec {
            id: b,
            gain: 0.9,
            send_db: -18.0,
            output: None,
            chain: vec![0, 1],
            sends: Vec::new(),
            comp_meter: None,
        })
        .collect();
    let mut p = ProjectSpec::new(ts);
    p.buses = buses;
    let mut g = build::build(p).compile(SR, max_block).unwrap();
    if active {
        for t in 0..tracks {
            for band in 0..6u16 {
                g.set_param(eq_id(t), band << 3 | eqp::GAIN_DB, 3.0 - band as f32);
            }
            g.set_param(comp_id(t), cp::BYPASS, 0.0);
            g.set_param(deesser_id(t), dp::BYPASS, 0.0);
            g.set_param(gate_id(t), gp::BYPASS, 0.0);
            g.set_param(delay_id(t), dlp::BYPASS, 0.0);
            g.set_param(delay_id(t), dlp::MIX, 0.3);
            g.set_param(saturation_id(t), sp::BYPASS, 0.0);
            g.set_param(saturation_id(t), sp::MIX, 0.4);
            g.set_param(doubler_id(t), dbp::BYPASS, 0.0);
            g.set_param(doubler_id(t), dbp::MIX, 0.4);
            g.set_param(limiter_id(t), lp::BYPASS, 0.0);
            g.set_param(reverb_id(t), rp::WET, 0.3);
        }
        for b in 0..2 {
            g.set_param(bus_comp_id(b), cp::BYPASS, 0.0);
        }
    }
    g
}

fn bench_project(tracks: u32, active: bool, block: usize) {
    let mut g = project(tracks, active, block);
    let mut out = vec![0.0; block * CHANNELS];
    let mut tl = 0i64;
    let ns = best_ns(25, 60, || {
        g.render(block, tl, &mut out);
        tl = (tl + block as i64) % (48_000 * 4);
    });
    report(
        &format!(
            "project {tracks:>2} tracks, {}, block {block}",
            if active {
                "all inserts on "
            } else {
                "build defaults "
            }
        ),
        ns,
        block,
    );
}

#[test]
#[ignore]
fn bench_project_graphs() {
    for &block in &[512usize, 1024] {
        for &tracks in &[8u32, 16] {
            bench_project(tracks, false, block);
            bench_project(tracks, true, block);
        }
    }
}

fn bench_node(name: &str, mut node: Box<dyn Node>, setup: &[(u16, f32)], block: usize) {
    node.prepare(SR, block);
    for &(p, v) in setup {
        node.set_param(p, v);
    }
    let input = material(block * 64, 7);
    let mut out = vec![0.0; block * CHANNELS];
    let mut k = 0usize;
    let ns = best_ns(25, 100, || {
        let s = (k % 64) * block * CHANNELS;
        let inputs: [&[Smp]; 1] = [&input[s..s + block * CHANNELS]];
        let mut outs: [&mut [Smp]; 1] = [&mut out[..]];
        let mut io = ProcessIo {
            inputs: &inputs,
            outputs: &mut outs[..],
            frames: block,
            tl: (k * block) as i64,
            sr: SR,
        };
        node.process(&mut io);
        k += 1;
    });
    report(name, ns, block);
}

#[test]
#[ignore]
fn bench_nodes() {
    let b = 512;
    let eq_gains: Vec<(u16, f32)> = (0..6u16)
        .map(|band| (band << 3 | eqp::GAIN_DB, 3.0 - band as f32))
        .collect();
    let eq_off: Vec<(u16, f32)> = (0..6u16)
        .map(|band| (band << 3 | eqp::ENABLED, 0.0))
        .collect();
    bench_node(
        "eq 6 bands flat (default)",
        Box::new(EqNode::default_strip()),
        &[],
        b,
    );
    bench_node(
        "eq 6 bands boosted",
        Box::new(EqNode::default_strip()),
        &eq_gains,
        b,
    );
    bench_node(
        "eq 6 bands disabled",
        Box::new(EqNode::default_strip()),
        &eq_off,
        b,
    );
    bench_node(
        "comp bypassed",
        Box::new(CompressorNode::default_vocal()),
        &[],
        b,
    );
    bench_node(
        "comp on",
        Box::new(CompressorNode::default_vocal()),
        &[(cp::BYPASS, 0.0)],
        b,
    );
    bench_node("de-esser bypassed", Box::new(DeEsserNode::new()), &[], b);
    bench_node(
        "de-esser on",
        Box::new(DeEsserNode::new()),
        &[(dp::BYPASS, 0.0)],
        b,
    );
    bench_node("gate bypassed", Box::new(GateNode::new()), &[], b);
    bench_node(
        "gate on",
        Box::new(GateNode::new()),
        &[(gp::BYPASS, 0.0)],
        b,
    );
    bench_node("delay bypassed", Box::new(DelayNode::new()), &[], b);
    bench_node(
        "delay on",
        Box::new(DelayNode::new()),
        &[(dlp::BYPASS, 0.0), (dlp::MIX, 0.3)],
        b,
    );
    bench_node(
        "saturation bypassed",
        Box::new(SaturationNode::new()),
        &[],
        b,
    );
    bench_node(
        "saturation on",
        Box::new(SaturationNode::new()),
        &[(sp::BYPASS, 0.0), (sp::MIX, 0.4)],
        b,
    );
    bench_node("doubler bypassed", Box::new(DoublerNode::new()), &[], b);
    bench_node(
        "doubler on",
        Box::new(DoublerNode::new()),
        &[(dbp::BYPASS, 0.0), (dbp::MIX, 0.4)],
        b,
    );
    bench_node("limiter bypassed", Box::new(LimiterNode::new()), &[], b);
    bench_node(
        "limiter on",
        Box::new(LimiterNode::new()),
        &[(lp::BYPASS, 0.0)],
        b,
    );
    bench_node(
        "reverb insert wet 0 (default)",
        Box::new(ReverbNode::new(0.5, 0.5, 0.0, 1.0, 1.0)),
        &[],
        b,
    );
    bench_node(
        "reverb bus (wet 1)",
        Box::new(ReverbNode::default_bus()),
        &[],
        b,
    );
    bench_node(
        "master/gain (unity, no clip)",
        Box::new(MasterNode::unity()),
        &[],
        b,
    );
    bench_node("master (limited)", Box::new(MasterNode::limited()), &[], b);
    bench_node(
        "pan",
        Box::new(vocara_engine::dsp::gain_pan::PanNode::new(0.3)),
        &[],
        b,
    );
    // Sources.
    let pcm = material(48_000 * 4, 3);
    bench_node(
        "buffer source (in span)",
        Box::new(BufferSource::new(pcm, 0)),
        &[],
        b,
    );
}

#[test]
#[ignore]
fn bench_streaming_source() {
    let block = 512;
    let ctrl = StreamCtrl::new();
    let (mut prod, cons) = spsc::<[Smp; CHANNELS]>(16384);
    let mut node = StreamingSource::new(Arc::clone(&ctrl), cons, 0, i64::MAX / 4);
    node.prepare(SR, block);
    for f in fence_frames(1, 0) {
        assert!(prod.push(f));
    }
    ctrl.gen.store(1, Ordering::Release);
    ctrl.flush_seq.store(1, Ordering::Release);
    let pcm = material(block, 5);
    let mut out = vec![0.0; block * CHANNELS];
    let mut tl = 0i64;
    let ns = best_ns(25, 500, || {
        for i in 0..block {
            prod.push([pcm[2 * i], pcm[2 * i + 1]]);
        }
        let inputs: [&[Smp]; 0] = [];
        let mut outs: [&mut [Smp]; 1] = [&mut out[..]];
        let mut io = ProcessIo {
            inputs: &inputs,
            outputs: &mut outs[..],
            frames: block,
            tl,
            sr: SR,
        };
        node.process(&mut io);
        tl += block as i64;
    });
    assert_eq!(&out[..], &pcm[..], "the bench must render real audio");
    report("streaming source: push + pop one block", ns, block);
}

/// The live callback body with the singer's monitor on (reverb + gain mixed over playback).
#[test]
#[ignore]
fn bench_live_monitor() {
    use std::collections::HashMap;
    use vocara_engine::graph::{Edge, GraphDesc, NodeEntry};
    let block = 512;
    let g = GraphDesc {
        nodes: vec![
            NodeEntry::new(0, Box::new(MasterNode::limited())),
            NodeEntry::new(1, Box::new(BufferSource::new(material(48_000 * 4, 1), 0))),
        ],
        edges: vec![Edge::simple(1, 0)],
        master: 0,
    }
    .compile(SR, 1024)
    .unwrap();
    let (mut mic_tx, mic_rx) = spsc::<[Smp; CHANNELS]>(8192);
    let (h, mut rt) = vocara_engine::live::spawn(g, HashMap::new(), mic_rx, 256);
    h.set_monitor_gain(0.8);
    h.play();
    let mic = material(block, 2);
    let mut out = vec![0.0; block * CHANNELS];
    let ns = best_ns(25, 200, || {
        for &f in mic.as_chunks::<CHANNELS>().0 {
            mic_tx.push(f);
        }
        rt.process(&mut out, block);
    });
    report(
        "live callback: playback + monitor (reverb + gain)",
        ns,
        block,
    );
}

/// The production AEC configuration (vpio.rs: 250 ms tail, 48 kHz) fed `CHUNK`-frame calls.
fn bench_aec_mode(name: &str, far_on: bool, mic_on: bool, echo_free: bool) {
    const CHUNK: usize = 256;
    let mut aec = Aec::new(48_000.0, 250.0);
    aec.set_echo_free(echo_free);
    let frames = 48_000 * 4;
    let far = material(frames, 11);
    let mic = material(frames, 13);
    let far: Vec<f32> = far
        .iter()
        .step_by(2)
        .map(|&x| if far_on { x } else { 0.0 })
        .collect();
    // Mic = a delayed, scaled echo of the far end plus a little "voice".
    let mic: Vec<f32> = (0..frames)
        .map(|i| {
            let echo = if i >= 960 { 0.4 * far[i - 960] } else { 0.0 };
            if mic_on {
                echo + 0.05 * mic[2 * i]
            } else {
                echo
            }
        })
        .collect();
    let mut out = vec![0.0; CHUNK];
    let mut pos = 0usize;
    // Converge first (adaptation cost differs cold vs. warm).
    for _ in 0..(48_000 * 2 / CHUNK) {
        aec.process(&mic[pos..pos + CHUNK], &far[pos..pos + CHUNK], &mut out);
        pos = (pos + CHUNK) % (frames - CHUNK);
    }
    let ns = best_ns(25, 100, || {
        aec.process(&mic[pos..pos + CHUNK], &far[pos..pos + CHUNK], &mut out);
        pos = (pos + CHUNK) % (frames - CHUNK);
    });
    report(name, ns, CHUNK);
}

#[test]
#[ignore]
fn bench_aec() {
    bench_aec_mode(
        "aec 250 ms: far + echo + voice (speaker)",
        true,
        true,
        false,
    );
    bench_aec_mode("aec 250 ms: far + echo, no voice", true, false, false);
    bench_aec_mode(
        "aec 250 ms: far silent (transport stopped)",
        false,
        true,
        false,
    );
    bench_aec_mode("aec 250 ms: echo-free (headphones)", true, true, true);
}

#[test]
#[ignore]
fn bench_wav_writer() {
    let dir = std::env::temp_dir().join(format!("vocara_wav_bench_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("take.wav");
    let samples = material(48_000 * 10, 9);
    let ns = best_ns(5, 1, || {
        let mut w = vocara_engine::wav::WavStream::create(&path, 1, 48_000).unwrap();
        for (i, &s) in samples.iter().enumerate() {
            w.write_sample(s).unwrap();
            if i % 512 == 511 {
                w.sync_header_if_due().unwrap();
            }
        }
        w.finalize().unwrap();
    });
    println!(
        "{:<58} {:>10.2} ms per 20 s of mono 48 kHz ({:.2} ns/sample)",
        "wav stream writer",
        ns / 1e6,
        ns / samples.len() as f64
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// File decode (`device::decode_file`, the import / staging path): 60 s of 44.1 kHz stereo as
/// 16-bit and float WAV, and the HE-AAC fixture. Needs `--features device`.
#[cfg(feature = "device")]
#[test]
#[ignore]
fn bench_decode_file() {
    use vocara_engine::device::decode_file;
    let dir = std::env::temp_dir().join(format!("vocara_decode_bench_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pcm: Vec<f32> = material(44_100 * 60, 4).iter().map(|v| v * 0.5).collect();
    let i16_path = dir.join("song_i16.wav");
    let f32_path = dir.join("song_f32.wav");
    vocara_engine::wav::write_wav_i16(&i16_path, &pcm, 2, 44_100).unwrap();
    vocara_engine::wav::write_wav_f32(&f32_path, &pcm, 2, 44_100).unwrap();
    let aac = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/he_aac_440hz.m4a"
    );
    for (name, path, reps) in [
        (
            "decode 60 s 44.1 kHz i16 wav",
            i16_path.to_str().unwrap(),
            3,
        ),
        (
            "decode 60 s 44.1 kHz f32 wav",
            f32_path.to_str().unwrap(),
            3,
        ),
        ("decode 2 s HE-AAC m4a", aac, 20),
    ] {
        let ns = best_ns(5, reps, || {
            std::hint::black_box(decode_file(path, 0.0, 0, 0).unwrap());
        });
        println!("{name:<58} {:>10.2} ms", ns / 1e6);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
