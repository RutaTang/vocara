//! Port of the repository half of
//! `client/test/asset_dedup_across_devices_test.dart` (its asset-store half
//! is `tests/asset_store/asset_dedup_across_devices_test.rs`).
//!
//! Identical bytes under two asset ids is the normal cross-device state. An
//! import used to dedup to the first row by HLC whether or not its bytes
//! were here, and re-pointed a synced-in row (another device's earlier
//! import, no bytes here) at the pool file a local row already owned: two
//! live rows on one path, and a clip loaded through that path came back on a
//! save as the other asset — an edit nobody made, which won over a peer's
//! real one.
//!
//! The Dart test applied the remote rows through the SyncApplier (not ported
//! yet); here they are written as the applier writes them: the remote
//! columns and HLC verbatim, no oplog entry.

use std::fs;

use vocara_model::models::audio_track::{AudioClip, AudioTrack};
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::id::new_id;
use vocara_store::db::rows::{AssetRow, ClipRow};
use vocara_store::db::AssetStore;
use vocara_store::sync::hlc::Hlc;

use super::{take, with_takes, Fixture};

fn clip_ops(f: &Fixture) -> usize {
    f.oplog()
        .iter()
        .filter(|o| o.entity_table == "clips")
        .count()
}

#[test]
fn an_import_dedups_to_the_row_whose_bytes_are_here_and_a_save_that_never_touched_a_clip_leaves_a_peers_edit_of_it_alone(
) {
    let f = Fixture::new();
    let song: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let src = f.file("sweep47/song.wav", &song);

    // 1. Local practice with one take whose clip C1 -> local asset A1.
    let created = f.create("P");
    let take = AudioTrack {
        clips: vec![AudioClip::new(new_id(), src.clone(), 1000)],
        ..take("Take 1", created.active_version().map(|v| v.id.as_str()))
    };
    f.save(&with_takes(&created, vec![take]));
    let a1 = f.db.select_all::<ClipRow>().unwrap().remove(0).asset_id;
    let a1_row: AssetRow = f.db.find(&a1).unwrap().unwrap();

    // 2. Sync delivers another device's earlier import of the SAME bytes
    //    (A2, lower HLC): the cross-device steady state, per-device path
    //    stripped.
    let a2 = new_id();
    let h = Hlc::new(1000, 0, "Y".repeat(26));
    f.db.insert(&AssetRow {
        id: a2.clone(),
        content_hash: a1_row.content_hash.clone(),
        byte_size: a1_row.byte_size,
        ext: "wav".into(),
        kind: "audio".into(),
        created_at: h.wall_ms,
        updated_at: h.wall_ms,
        hlc_wall: h.wall_ms,
        hlc_ctr: h.counter,
        hlc_node: h.node.clone(),
        hlc_packed: h.packed(),
        record_sync_state: "synced".into(),
        ..Default::default()
    })
    .unwrap();

    // 3. The user imports the same file again (e.g. into another practice):
    //    it dedups to A1, whose bytes are here, and A2 keeps no path.
    let again = f.file("sweep47/again.wav", &fs::read(&src).unwrap());
    assert_eq!(
        f.assets
            .import_file(&again, AssetStore::DEFAULT_KIND)
            .unwrap(),
        a1
    );
    let a2_row: AssetRow = f.db.find(&a2).unwrap().unwrap();
    assert_eq!(a2_row.local_rel_path, None);

    // 4. The editor loads P; meanwhile a peer moves C1 (remote edit, newer
    //    HLC).
    let loaded = f.load(&created.id);
    let clip_row = f.db.select_all::<ClipRow>().unwrap().remove(0);
    let moved = Hlc::new(f.writer.clock().last().wall_ms + 10, 0, "Y".repeat(26));
    f.raw(
        "UPDATE clips SET asset_id = ?, start_ms = 5000, hlc_wall = ?, hlc_ctr = ?, \
         hlc_node = ?, hlc_packed = ?, updated_at = ? WHERE id = ?",
        rusqlite::params![
            a1,
            moved.wall_ms,
            moved.counter,
            moved.node,
            moved.packed(),
            moved.wall_ms,
            clip_row.id
        ],
    );
    let ops_before = clip_ops(&f);

    // 5. The editor saves an edit that never touches C1 (a title rename).
    f.save_with_base(
        &SongPractice {
            title: "Renamed".into(),
            ..loaded.aggregate.clone()
        },
        &loaded.base_hashes,
    )
    .unwrap();

    let after = f.db.select_all::<ClipRow>().unwrap().remove(0);
    assert_eq!(
        after.start_ms, 5000,
        "the peer moved C1 and this save never touched it"
    );
    assert_eq!(after.asset_id, a1);
    assert_eq!(clip_ops(&f) - ops_before, 0);
}
