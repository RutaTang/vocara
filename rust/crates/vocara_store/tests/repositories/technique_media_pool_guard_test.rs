//! Port of the repository case of
//! `client/test/technique_media_pool_guard_test.dart`: removing a persisted
//! example never reclaims pool bytes another technique still shares.
//!
//! The Dart test drove the removal through the technique detail view model,
//! whose removed-paths cleanup then asked the media service to delete the
//! pooled file (which the service's pool guard refuses). Here the removal is
//! the repository save the view model makes; the service's guard, its
//! staging deletes and the view model's cleanup belong to the
//! TechniqueMediaService and view model ports.

use std::path::Path;
use std::time::Duration;

use vocara_model::models::technique::Technique;
use vocara_model::models::technique_media::{TechniqueMedia, TechniqueMediaKind};
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_path as p;
use vocara_model::util::id::new_id;
use vocara_store::repositories::TechniqueRepository;

use super::Fixture;

fn technique_with(id: &str, media_id: &str, path: String) -> Technique {
    let now = DateTime::local_ymd(2026, 7, 1);
    Technique {
        media: vec![TechniqueMedia {
            id: media_id.to_string(),
            path,
            kind: TechniqueMediaKind::Audio,
            label: String::new(),
            added_at: now,
        }],
        ..Technique::new(id, format!("T {id}"), now, now)
    }
}

#[test]
fn removing_a_persisted_example_never_reclaims_shared_pool_bytes() {
    let f = Fixture::new();
    // Two techniques whose examples dedup to the SAME content hash (one
    // pooled file, one asset row).
    let (t1_id, m1_id) = (new_id(), new_id());
    let (t2_id, m2_id) = (new_id(), new_id());
    f.techs
        .save_technique(&technique_with(
            &t1_id,
            &m1_id,
            f.file("one.wav", &[7, 7, 7]),
        ))
        .unwrap();
    f.techs
        .save_technique(&technique_with(
            &t2_id,
            &m2_id,
            f.file("two.wav", &[7, 7, 7]),
        ))
        .unwrap();

    let held = f.techs.load_technique(&t1_id).unwrap();
    let pool_path = held.aggregate.media[0].path.clone();
    assert!(p::is_within(
        &p::join(&f.root().to_string_lossy(), "assets"),
        &pool_path
    ));

    // Remove the persisted example and save.
    let saved = f
        .techs
        .save_technique_with_base(
            &Technique {
                media: Vec::new(),
                ..held.aggregate.clone()
            },
            &held.base_hashes,
        )
        .unwrap();
    assert!(saved.aggregate.media.is_empty());
    assert!(Path::new(&pool_path).exists());

    // The second technique referencing the same hash still resolves.
    let t2 = f.techs.get_technique(&t2_id).unwrap();
    assert_eq!(t2.media[0].path, pool_path);
    assert!(Path::new(&pool_path).exists());

    // Pool bytes are reclaimed ONLY by the hash-refcount GC: with t2 still
    // referencing the hash, a sweep keeps the file in place. The sweep runs
    // with no age grace: under the default 24 h window a seconds-old asset
    // is never swept whatever references it, so this proved nothing.
    assert_eq!(f.assets.sweep_unreferenced(Duration::ZERO).unwrap(), 0);
    assert!(Path::new(&pool_path).exists());

    // ...and once nothing references the hash, that same sweep reclaims it.
    f.techs.delete_technique(&t2_id).unwrap();
    assert_eq!(f.assets.sweep_unreferenced(Duration::ZERO).unwrap(), 1);
    assert!(!Path::new(&pool_path).exists());
}
