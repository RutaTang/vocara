//! The vocal tract behind Vocara's "Vocal tract" view: an articulatory model of the human head
//! (jaw, tongue, lips, velum, larynx), the acoustics of the airway it shapes, and (built on
//! them) the estimate of a singer's articulation from the formants the app measures.
//!
//! # Ported from CHIMERA
//!
//! The anatomy and the acoustics are a port of CHIMERA, a physical-modelling voice synthesizer
//! by Jameson Hodge (MIT licence, `LICENSE-CHIMERA`), at upstream commit
//! `153c7c8f22108c400e8556950a2c3b61f3f62aa2`:
//!
//! | Here | CHIMERA |
//! |---|---|
//! | [`math`](crate) (private), [`physics`] | `crates/chimera/src/math.rs`, `physics.rs` (the parts used) |
//! | [`geom`] | `crates/chimera/src/anatomy/geom.rs` |
//! | [`anatomy`] | `crates/chimera/src/anatomy/human.rs` |
//! | [`airway::network`] | `crates/chimera/src/airway/network.rs` (topology and linearised elements) |
//! | [`airway::frequency`] | `crates/chimera/src/airway/frequency.rs` |
//! | [`airway::human`] | `crates/chimera/src/voice/human_airway.rs` (the network and `apply`) |
//! | [`optimize`] | `crates/chimera/src/util/optimize.rs` |
//! | [`vowels`] | `crates/chimera/src/phonetics/vowelTargets.generated.json` (the table) |
//!
//! The physics and geometry are CHIMERA's operation for operation, with every transcendental
//! function through `libm` as there, so its fixtures (`tests/golden/`, copied from
//! `crates/chimera/tests/golden/`) match exactly: `tests/golden_geometry.rs` and
//! `tests/golden_analysis.rs` are ports of its tests of the same names.

pub mod airway;
pub mod anatomy;
pub mod forward;
pub mod geom;
mod math;
pub mod optimize;
pub mod physics;
pub mod vowels;
