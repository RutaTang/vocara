//! Port of `client/test/asset_dedup_across_devices_test.dart`, at the asset
//! store's level.
//!
//! Identical bytes under two asset ids is the normal cross-device state. An
//! import used to dedup to the first row by HLC whether or not its bytes
//! were here, and re-pointed a synced-in row (another device's earlier
//! import, no bytes here) at the pool file a local row already owned: two
//! live rows on one path, and a clip loaded through that path came back on a
//! save as the other asset — an edit nobody made, which won over a peer's
//! real one.
//!
//! The Dart test drove the whole story through Persistence, the song
//! repository and the SyncApplier (not ported yet): the local take's save,
//! the applied remote asset row, a peer's clip edit and a title-only save
//! with base hashes. Here the rows those write are written directly, and the
//! asset store's part is checked: the dedup picks the row whose bytes are
//! here, the synced-in row keeps no path, and the pool path a hydrated clip
//! carries resolves back to the local asset (so a save that never touched
//! the clip re-sends the same asset id). The clip-level assertions (the
//! peer's start_ms survives, no clip oplog entry) are the repository port's:
//! `tests/repositories/asset_dedup_across_devices_test.rs`.

use std::fs;

use vocara_model::util::id::new_id;
use vocara_store::db::rows::AssetRow;
use vocara_store::db::AssetStore;
use vocara_store::sync::hlc::Hlc;

use super::{shared, Fixture};

#[test]
fn an_import_dedups_to_the_row_whose_bytes_are_here_and_a_resave_keeps_that_asset() {
    let _g = shared();
    let f = Fixture::new();
    let song: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let src = f.make_src("song.wav", &song);

    // 1. A local take: its save pooled the file as A1.
    let a1 = f
        .store
        .resolve_or_import_all(std::slice::from_ref(&src), AssetStore::DEFAULT_KIND)
        .unwrap()
        .remove(0);
    let a1_row = f.asset(&a1);

    // 2. Sync delivers another device's earlier import of the SAME bytes
    //    (A2, lower HLC): the cross-device steady state, per-device path
    //    stripped, marked record-synced as the applier leaves it.
    let a2 = new_id();
    let h = Hlc::new(1000, 0, "Y".repeat(26));
    f.db.insert(&AssetRow {
        id: a2.clone(),
        content_hash: a1_row.content_hash.clone(),
        byte_size: a1_row.byte_size,
        ext: "wav".to_string(),
        kind: "audio".to_string(),
        created_at: h.wall_ms,
        updated_at: h.wall_ms,
        hlc_wall: h.wall_ms,
        hlc_ctr: h.counter,
        hlc_node: h.node.clone(),
        hlc_packed: h.packed(),
        record_sync_state: "synced".to_string(),
        ..Default::default()
    })
    .unwrap();
    assert!(h.packed() < a1_row.hlc_packed, "A2 sorts first by HLC");

    // 3. The user imports the same file again (e.g. into another practice):
    //    it dedups to A1, whose bytes are here, and A2 keeps no path.
    let again = f.make_src("again.wav", &fs::read(&src).unwrap());
    assert_eq!(f.import(&again), a1);
    assert_eq!(f.asset(&a2).local_rel_path, None);

    // 4./5. The editor loaded the practice: the clip carries A1's pool path.
    //    A save that never touched the clip resolves that path back to A1,
    //    not to the synced-in row with the same bytes, and journals nothing.
    let loaded_path = f.resolve(&a1).unwrap();
    let ops_before = f.oplog().len();
    assert_eq!(
        f.store
            .resolve_or_import_all(std::slice::from_ref(&loaded_path), AssetStore::DEFAULT_KIND)
            .unwrap(),
        std::slice::from_ref(&a1)
    );
    assert_eq!(
        f.store
            .resolve_or_import(&loaded_path, AssetStore::DEFAULT_KIND)
            .unwrap(),
        a1
    );
    assert_eq!(f.oplog().len(), ops_before);
    assert_eq!(f.asset(&a2).local_rel_path, None);
    assert_eq!(f.live_assets().len(), 2);
}
