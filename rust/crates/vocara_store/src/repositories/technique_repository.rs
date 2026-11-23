//! The technique library repository interface (port of
//! `lib/data/repositories/technique_repository.dart`).

use indexmap::IndexMap;
use vocara_model::models::technique::Technique;

use crate::error::Result;
use crate::sync::sync_hashes::Loaded;

/// Single source of truth for the technique library. The library is a FLAT
/// pool of [`Technique`] records organized only by their tags (there is no
/// collection container) — a technique can carry any number of tags,
/// including a framework tag like "EVT" or "CVT". Returns [`Result`] rather
/// than panicking.
///
/// Editing flows MUST use the base-hash pair
/// [`TechniqueRepository::load_technique`] +
/// [`TechniqueRepository::save_technique_with_base`]: the base hashes
/// captured at load make the save a three-way merge, so a stale open
/// technique can neither revert nor tombstone rows that changed underneath
/// it (e.g. applied by sync). The base-less
/// [`TechniqueRepository::save_technique`] stays only for creation flows.
pub trait TechniqueRepository: Send + Sync {
    /// Every live technique, newest first. A technique whose rows this build
    /// cannot read is left out.
    fn list_techniques(&self) -> Result<Vec<Technique>>;

    /// The live technique [id].
    fn get_technique(&self, id: &str) -> Result<Technique>;

    /// Loads one technique FOR EDITING: the technique plus the per-row base
    /// content hashes of everything read (see [`Loaded`]). Hand the hashes
    /// back to [`TechniqueRepository::save_technique_with_base`] unchanged.
    fn load_technique(&self, id: &str) -> Result<Loaded<Technique>>;

    /// Persists [technique] (bumping its `updated_at`) and returns the saved
    /// copy.
    ///
    /// NO base hashes: the save adopts the CURRENT persisted state as its
    /// base, which is the old two-way diff — correct only when the technique
    /// was just created or re-read immediately before saving. A long-lived
    /// editor holding the technique across other writes must use
    /// [`TechniqueRepository::save_technique_with_base`] instead, or its
    /// stale save can revert or delete concurrent changes.
    fn save_technique(&self, technique: &Technique) -> Result<Technique>;

    /// Persists [technique] as a THREE-WAY merge against [base_hashes]
    /// (captured by [`TechniqueRepository::load_technique`] or returned by
    /// the previous save) and returns the saved technique re-read from the
    /// store together with FRESH base hashes for the next save — and, as
    /// [`Loaded::sent_hashes`], the hashes of [technique] as sent.
    ///
    /// An error means no row of the technique was written; media it imported
    /// into the asset pool before the failure may remain (the pool's sweep
    /// reclaims what nothing references). A deleted technique is refused
    /// before any import. A write that committed but whose re-read failed
    /// still succeeds, with [technique] itself and its sent hashes standing
    /// in for the re-read ([`Loaded::reread`] false).
    fn save_technique_with_base(
        &self,
        technique: &Technique,
        base_hashes: &IndexMap<String, String>,
    ) -> Result<Loaded<Technique>>;

    /// Tombstones the technique [id] and its media. Deleting a technique
    /// that is already deleted, or was never there, writes nothing.
    fn delete_technique(&self, id: &str) -> Result<()>;
}
