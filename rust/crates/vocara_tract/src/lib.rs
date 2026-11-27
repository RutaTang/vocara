//! The vocal tract behind Vocara's "Vocal tract" view: an articulatory model of the human head
//! (jaw, tongue, lips, velum, larynx), the acoustics of the airway it shapes, its drawing as a
//! 2D cut-away and a 3D head, and the estimate of a singer's articulation from the formants
//! the app measures.
//!
//! # For a view
//!
//! Everything is per body: a [`BodyMix`](anatomy::BodyMix) between the adult male, the adult
//! female and the child CHIMERA calibrated. Head centimetres at the body's size throughout: x
//! forward, y up, z out of the right side of the face, the glottis at the origin for a resting
//! larynx.
//!
//! - **Estimate** ([`inversion`]): [`Inverter::update`](inversion::Inverter::update) takes one
//!   analysis frame ([`FormantFrame`](inversion::FormantFrame): F0, F1–F3, voicing confidence)
//!   and returns a [`TractEstimate`](inversion::TractEstimate): the fitted
//!   [`Posture`](posture::Posture), the whole [`HumanArtic`](anatomy::HumanArtic), the model's
//!   formants, the residual and a confidence. Vocara's services wrap it for the live mic and
//!   for takes (`vocara_services::vocal_tract`).
//! - **Components** ([`components`]): [`TractComponents::of`](components::TractComponents::of)
//!   reads an articulation in plain units (jaw mm, tongue height and frontness, lip aperture and
//!   rounding, larynx mm, velum); [`VocalFolds`](components::VocalFolds) carries F0, breathiness
//!   and register.
//! - **Drawing** ([`lines`]): [`TractModel::outlines`](forward::TractModel::outlines) gives the
//!   2D cut-away ([`Outlines2d`](lines::Outlines2d), see [`outline`]),
//!   [`TractModel::head3d`](forward::TractModel::head3d) the 3D head
//!   ([`Lines3d`](lines::Lines3d), see [`head3d`]): polylines tagged with the
//!   [`Structure`](lines::Structure) they draw, rebuilt in place frame after frame.
//! - **Acoustics** ([`forward`]): [`TractModel`](forward::TractModel) also gives an
//!   articulation's formants and bandwidths, transfer function and area function.
//!
//! Costs (release, measured by the ignored timing tests): a frame's inversion 0.3–0.45 ms on
//! average, about 2 ms when it restarts from the codebook; the cut-away 0.14 ms, the head
//! 0.25 ms; a body's codebook a few tenths of a second, once per process. Once warm nothing of
//! this allocates per frame (`tests/no_alloc.rs`).
//!
//! # Ported from CHIMERA
//!
//! The anatomy, the acoustics and the drawing are a port of CHIMERA, a physical-modelling voice
//! synthesizer by Jameson Hodge (MIT licence, `LICENSE-CHIMERA`, also on the app's Licences
//! page), at upstream commit `153c7c8f22108c400e8556950a2c3b61f3f62aa2`:
//!
//! | Here | CHIMERA |
//! |---|---|
//! | `math` (private), [`physics`] | `crates/chimera/src/math.rs`, `physics.rs` (the parts used) |
//! | [`geom`] | `crates/chimera/src/anatomy/geom.rs` (and `chaikin` of `src/ui/render/vector.ts`) |
//! | [`anatomy`] | `crates/chimera/src/anatomy/human.rs` |
//! | [`airway::network`] | `crates/chimera/src/airway/network.rs` (topology and linearised elements) |
//! | [`airway::frequency`] | `crates/chimera/src/airway/frequency.rs` |
//! | [`airway::human`] | `crates/chimera/src/voice/human_airway.rs` (the network and `apply`) |
//! | [`optimize`] | `crates/chimera/src/util/optimize.rs` |
//! | [`vowels`] | `crates/chimera/src/phonetics/vowelTargets.generated.json` (the table) |
//! | `profile` (private), [`outline`] | `src/ui/render/profile.ts` (`buildProfile`), `src/ui/render/anatomy.ts` (what it draws, in order) |
//! | [`head3d`] | `src/ui/render/anatomy3d.ts` (the geometry: `buildRest`, the airway, velum, epiglottis, larynx, tongue and lips) |
//!
//! The physics and geometry are CHIMERA's operation for operation, with every transcendental
//! function through `libm` as there, so its fixtures (`tests/golden/`, copied from
//! `crates/chimera/tests/golden/`) match exactly: `tests/golden_geometry.rs` and
//! `tests/golden_analysis.rs` are ports of its tests of the same names. Two speed-ups keep its
//! results bit for bit (see `anatomy::project_on` and [`airway::frequency::TransferSolver`]).
//!
//! What is Vocara's own: the fast formant search ([`forward`]), the inversion ([`posture`],
//! [`codebook`], [`inversion`]), the components ([`components`]) and the drawing data's
//! structure ([`lines`]). CHIMERA synthesizes sound from articulators; it does not invert.

pub mod airway;
pub mod anatomy;
pub mod codebook;
pub mod components;
pub mod forward;
pub mod geom;
pub mod head3d;
pub mod inversion;
pub mod lines;
mod math;
pub mod optimize;
pub mod outline;
pub mod physics;
pub mod posture;
mod profile;
pub mod vowels;

/// CHIMERA's licence: the notice the port carries (`LICENSE-CHIMERA`), for the app's Licences
/// page.
pub const CHIMERA_LICENSE: &str = include_str!("../LICENSE-CHIMERA");

/// Where the ported code comes from, for the Licences page.
pub const CHIMERA_SOURCE: &str = "CHIMERA physical-modelling voice synthesizer, by Jameson Hodge \
     (github.com/jamesondh/chimera-vocal-synth), commit 153c7c8f22108c400e8556950a2c3b61f3f62aa2";
