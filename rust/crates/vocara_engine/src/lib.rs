//! Vocara real-time DAG audio engine.
//!
//! A node graph processed by one owner: a control plane edits a staged graph and publishes
//! it; the audio thread renders a topo-ordered schedule over a buffer pool. The DSP core,
//! graph runtime, and transport are dependency-free and validated through a headless
//! offline render (`offline`) — the primary RT-safety + golden-file surface. The live OS
//! audio path (cpal/symphonia) lives behind the `device` feature.

pub mod aec;
pub mod build;
pub mod capture_layout;
pub mod command;
// The mic's diagnostics lines, queued for the host app's log (`ve_take_diagnostic`).
pub mod diag;
pub mod engine;
pub mod feed;
pub mod graph;
pub mod graph_cell;
// Whether an open mic stream still delivers (the host's reopen decision).
pub mod input_health;
pub mod live;
// The pre-decode file screen: no external crates, so it builds and tests without `device`.
pub mod media_screen;
pub mod node;
pub mod ring;
pub mod ring_sync;
pub mod sample;
pub mod source;
pub mod wav;

pub mod dsp;

#[cfg(feature = "device")]
pub mod device;
#[cfg(feature = "device")]
pub mod ffi;
// macOS VoiceProcessingIO mic capture + AEC (replaces the cpal input path on Apple).
#[cfg(all(feature = "device", target_vendor = "apple"))]
mod vpio;

pub use engine::{Engine, Transport};
pub use graph::{Edge, Graph, GraphDesc, NodeEntry, NodeId};
pub use node::{Node, ProcessIo};
pub use sample::{Smp, CHANNELS};
pub use source::BufferSource;
