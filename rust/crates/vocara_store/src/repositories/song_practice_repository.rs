//! The song-practice repository interface (port of
//! `lib/data/repositories/song_practice_repository.dart`).

use indexmap::IndexMap;
use vocara_model::models::practice_summary::PracticeSummary;
use vocara_model::models::song_practice::SongPractice;

use crate::error::Result;
use crate::sync::sync_hashes::Loaded;

/// Single source of truth for [`SongPractice`] aggregates. Both the home list
/// and the detail view read and write through this repository. Returns
/// [`Result`] rather than panicking.
///
/// Editing flows MUST use the base-hash pair
/// [`SongPracticeRepository::load_practice`] +
/// [`SongPracticeRepository::save_practice_with_base`]: the base hashes
/// captured at load make the save a three-way merge, so a stale open
/// aggregate can neither revert nor tombstone rows that changed underneath it
/// (e.g. applied by sync). The base-less
/// [`SongPracticeRepository::save_practice`] stays only for create/append
/// flows that just built or re-read the aggregate.
pub trait SongPracticeRepository: Send + Sync {
    /// Every live practice, hydrated, newest first. A practice whose rows
    /// this build cannot read is left out.
    fn list_practices(&self) -> Result<Vec<SongPractice>>;

    /// What the Home list shows of every live practice (see
    /// [`PracticeSummary`]), newest first like
    /// [`SongPracticeRepository::list_practices`] — without hydrating any of
    /// them. So unlike [`SongPracticeRepository::list_practices`], a practice
    /// whose rows this build cannot read (a malformed or newer-format JSON
    /// column) is still listed: Home can delete it, and opening it says it
    /// could not be loaded, where before it simply vanished from the list.
    fn list_practice_summaries(&self) -> Result<Vec<PracticeSummary>>;

    /// The live practice [id].
    fn get_practice(&self, id: &str) -> Result<SongPractice>;

    /// Loads one aggregate FOR EDITING: the practice plus the per-row base
    /// content hashes of everything read (see [`Loaded`]). Hand the hashes
    /// back to [`SongPracticeRepository::save_practice_with_base`] unchanged.
    fn load_practice(&self, id: &str) -> Result<Loaded<SongPractice>>;

    /// Creates and persists a practice titled [title] (trimmed; "Untitled
    /// practice" when blank) with one interpretation.
    fn create_practice(&self, title: &str) -> Result<SongPractice>;

    /// Persists [practice] (bumping its `updated_at`) and returns the saved
    /// copy.
    ///
    /// NO base hashes: the save adopts the CURRENT persisted state as its
    /// base, which is the old two-way diff — correct only when the aggregate
    /// was just created or re-read immediately before saving
    /// (create/append/recovery flows). A long-lived editor holding the
    /// aggregate across other writes must use
    /// [`SongPracticeRepository::save_practice_with_base`] instead, or its
    /// stale save can revert or delete concurrent changes.
    fn save_practice(&self, practice: &SongPractice) -> Result<SongPractice>;

    /// Persists [practice] as a THREE-WAY merge against [base_hashes]
    /// (captured by [`SongPracticeRepository::load_practice`] or returned by
    /// the previous save) and returns the saved aggregate re-read from the
    /// store together with FRESH base hashes for the next save — and, as
    /// [`Loaded::sent_hashes`], the hashes of [practice] as sent: the base
    /// for a caller that kept a model newer than [practice].
    ///
    /// An error means no row of the practice was written; files it imported
    /// into the asset pool before the failure may remain (the pool's sweep
    /// reclaims what nothing references). A deleted practice is refused
    /// before any import. A write that committed but whose re-read failed
    /// still succeeds, with [practice] itself and its sent hashes standing in
    /// for the re-read ([`Loaded::reread`] false).
    fn save_practice_with_base(
        &self,
        practice: &SongPractice,
        base_hashes: &IndexMap<String, String>,
    ) -> Result<Loaded<SongPractice>>;

    /// Tombstones the practice [id] and its whole subtree. Deleting a
    /// practice that is already deleted, or was never there, writes nothing.
    fn delete_practice(&self, id: &str) -> Result<()>;
}
