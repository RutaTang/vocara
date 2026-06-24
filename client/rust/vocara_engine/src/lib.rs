//! Vocara real-time DAG audio engine.
//!
//! A node graph processed by one owner: a control plane edits a staged graph and publishes
//! it; the audio thread renders a topo-ordered schedule over a buffer pool. The DSP core,
//! graph runtime, and transport are dependency-free and validated through a headless
//! offline render (`offline`) — the primary RT-safety + golden-file surface. The live OS
//! audio path (cpal/symphonia) lives behind the `device` feature.

pub mod aec;
pub mod build;
pub mod command;
pub mod engine;
pub mod graph;
pub mod graph_cell;
pub mod live;
pub mod node;
pub mod ring;
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
