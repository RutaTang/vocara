//! Port of the repository case of
//! `client/test/technique_example_identity_test.dart`.
//!
//! Identity: two examples with identical bytes resolve to the SAME pooled
//! file. Selection and expansion were keyed by path, so both cards opened
//! with the analysis panel and a playhead-linked player, and the second
//! example could never be selected on its own. The store's part is that the
//! pool dedups them to one file while both examples load, in order, under
//! their own ids; the selection itself (keyed by id) and the naming of
//! dropped and recorded examples belong to the technique detail view
//! model's port, and the display-label rule is the model's
//! (`TechniqueMedia::display_label`).

use std::collections::HashSet;

use vocara_model::models::technique::Technique;
use vocara_model::models::technique_media::{TechniqueMedia, TechniqueMediaKind};
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::id::new_id;
use vocara_store::repositories::TechniqueRepository;

use super::Fixture;

#[test]
fn two_byte_identical_examples_share_one_pooled_file_and_keep_their_own_ids() {
    let f = Fixture::new();
    let now = DateTime::local_ymd(2026, 7, 1);
    let technique_id = new_id();
    f.save_technique(&Technique::new(technique_id.clone(), "Twang", now, now));

    let ids = [new_id(), new_id()];
    let current = f.techs.get_technique(&technique_id).unwrap();
    f.save_technique(&Technique {
        media: ids
            .iter()
            .enumerate()
            .map(|(i, id)| TechniqueMedia {
                id: id.clone(),
                path: f.file(&format!("src/take {i}.wav"), &[9, 9, 9]),
                kind: TechniqueMediaKind::Audio,
                label: format!("Take {i}"),
                added_at: now,
            })
            .collect(),
        ..current
    });

    let loaded = f.techs.get_technique(&technique_id).unwrap();
    // The pool dedups identical bytes: one file, two examples.
    let paths: HashSet<&str> = loaded.media.iter().map(|m| m.path.as_str()).collect();
    assert_eq!(paths.len(), 1);
    assert_eq!(
        loaded
            .media
            .iter()
            .map(|m| m.id.clone())
            .collect::<Vec<_>>(),
        ids.to_vec()
    );
    assert_eq!(
        loaded
            .media
            .iter()
            .map(|m| m.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Take 0", "Take 1"]
    );
}
