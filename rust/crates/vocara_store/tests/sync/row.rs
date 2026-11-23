//! Port of `client/test/sync_row_test.dart`.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{json, Value};
use vocara_model::util::id::{is_path_component_id, new_id};
use vocara_store::db::rows::{AssetRow, PracticeRow, TrackRow};
use vocara_store::error::Error;
use vocara_store::sync::hlc::{Hlc, HlcClock};
use vocara_store::sync::sync_applier::SyncApplier;
use vocara_store::sync::sync_row::{
    json_column_decoder, per_device_columns, synced_table, synced_table_registry, FieldValue,
    Fields, SyncRow, SyncRowBuilder,
};

use crate::support::*;

fn hlc() -> Hlc {
    Hlc::new(1234, 7, id("NODE"))
}

/// A structurally-complete field map for a practices row.
fn practice_fields(row_id: &str, h: &Hlc, title: &str) -> Fields {
    fields! {
        "id" => id(row_id),
        "title" => title,
        "timeline_duration_ms" => 0i64,
        "master_volume" => 0.5,
        "active_interpretation_id" => NULL,
        "separation_asset_id" => NULL,
        "reverb_json" => NULL,
        "lyrics_json" => NULL,
        "created_at" => h.wall_ms,
        "updated_at" => h.wall_ms,
        "deleted_at" => NULL,
        "hlc_wall" => h.wall_ms,
        "hlc_ctr" => h.counter,
        "hlc_node" => h.node.clone(),
        "hlc_packed" => h.packed(),
        "schema_v" => 1i64,
    }
}

fn is_format(e: &Error) -> bool {
    matches!(e, Error::Format(_))
}

fn json_of(row: &SyncRow) -> serde_json::Map<String, Value> {
    match row.to_json().unwrap() {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

// --- wire format round-trip ------------------------------------------------

#[test]
fn encode_decode_preserves_every_field_including_nulls_and_numeric_runtime_types() {
    let h = hlc();
    let row = SyncRow::new(
        "tracks",
        fields! {
            "id" => id("T1"),
            "practice_id" => id("P1"),
            "kind" => "vocal",
            "interpretation_id" => NULL, // null field survives
            "display_name" => "Take 1",
            "color_value" => NULL,
            "muted" => 0i64, // boolean-as-int per DB convention
            "soloed" => 1i64,
            "volume" => 1.0, // REAL: must stay a double
            "pan" => 0.0,
            "bus_id" => NULL,
            "fx_json" => NULL,
            "sends_json" => NULL,
            "order_index" => 3i64, // INTEGER: must stay an int
            "created_at" => 1234i64,
            "updated_at" => 1234i64,
            "deleted_at" => NULL,
            "hlc_wall" => h.wall_ms,
            "hlc_ctr" => h.counter,
            "hlc_node" => h.node.clone(),
            "hlc_packed" => h.packed(),
            "schema_v" => 1i64,
        },
    );

    let decoded = SyncRow::decode(&row.encode().unwrap()).unwrap();

    assert_eq!(decoded.table(), "tracks");
    assert_eq!(decoded.id().unwrap(), id("T1"));
    assert_eq!(decoded.schema_v().unwrap(), 1);
    assert_eq!(decoded.hlc_packed().unwrap(), h.packed());
    assert_eq!(decoded.hlc().unwrap(), h);
    assert_eq!(decoded.fields(), row.fields());
    // Runtime types survive JSON: SQLite binds ints and doubles differently.
    assert!(matches!(
        decoded.field("volume"),
        Some(FieldValue::Double(_))
    ));
    assert!(matches!(decoded.field("pan"), Some(FieldValue::Double(_))));
    assert!(matches!(
        decoded.field("order_index"),
        Some(FieldValue::Int(3))
    ));
    assert!(matches!(decoded.field("muted"), Some(FieldValue::Int(0))));
    assert_eq!(decoded.field("interpretation_id"), Some(&NULL));
    assert!(decoded.field_clocks().is_none()); // reserved slot stays null in v1
}

#[test]
fn the_wire_text_is_byte_for_byte_dart_s_json_encode() {
    // The exact text Dart's jsonEncode(toJson()) writes: envelope key order,
    // the field map in insertion order, a whole double with its point, an
    // int without one, and the explicit null column-clock slot.
    let h = hlc();
    let row = SyncRow::new(
        "practices",
        fields! {
            "id" => id("P1"),
            "master_volume" => 1.0,
            "timeline_duration_ms" => 0i64,
            "title" => "a \"quoted\"\ntitle",
            "deleted_at" => NULL,
            "hlc_wall" => h.wall_ms,
            "hlc_ctr" => h.counter,
            "hlc_node" => h.node.clone(),
            "hlc_packed" => h.packed(),
            "schema_v" => 1i64,
        },
    );
    let n = id("NODE");
    let p = id("P1");
    assert_eq!(
        row.encode().unwrap(),
        format!(
            "{{\"table\":\"practices\",\"id\":\"{p}\",\"schemaV\":1,\
             \"hlcPacked\":\"0000000001234-000007-{n}\",\"fields\":{{\"id\":\"{p}\",\
             \"master_volume\":1.0,\"timeline_duration_ms\":0,\
             \"title\":\"a \\\"quoted\\\"\\ntitle\",\"deleted_at\":null,\"hlc_wall\":1234,\
             \"hlc_ctr\":7,\"hlc_node\":\"{n}\",\"hlc_packed\":\"0000000001234-000007-{n}\",\
             \"schema_v\":1}},\"fieldClocks\":null}}"
        )
    );
}

#[test]
fn field_clocks_from_a_future_column_lww_peer_survive_a_round_trip_through_a_v1_device() {
    let h = hlc();
    let mut clocks = indexmap::IndexMap::new();
    clocks.insert("title".to_string(), h.packed());
    let row = SyncRow::with_field_clocks(
        "practices",
        practice_fields("P1", &h, "t"),
        Some(clocks.clone()),
    );
    let decoded = SyncRow::decode(&row.encode().unwrap()).unwrap();
    assert_eq!(decoded.field_clocks(), Some(&clocks));
}

#[test]
fn a_sync_row_is_a_value_its_fields_are_only_read() {
    // Dart froze the field map (a mutation threw UnsupportedError). Here the
    // row only hands out a shared reference; a modified copy is a NEW row and
    // leaves the original untouched.
    let row = SyncRow::new("practices", practice_fields("P", &hlc(), "t"));
    let mut copy = row.fields().clone();
    copy.insert("title".into(), "mutated".into());
    assert_eq!(row.field("title"), Some(&FieldValue::from("t")));
}

#[test]
fn decode_rejects_an_envelope_that_disagrees_with_the_field_map() {
    let mut json = json_of(&SyncRow::new(
        "practices",
        practice_fields("P1", &hlc(), "t"),
    ));
    json.insert("id".into(), Value::String(id("OTHER")));
    assert!(is_format(&SyncRow::from_json(&json).unwrap_err()));
}

#[test]
fn decode_rejects_structured_non_scalar_field_values() {
    let mut json = json_of(&SyncRow::new(
        "practices",
        practice_fields("P1", &hlc(), "t"),
    ));
    let Some(Value::Object(fields)) = json.get_mut("fields") else {
        unreachable!()
    };
    fields.insert("title".into(), json!({"not": "a scalar"}));
    assert!(is_format(&SyncRow::from_json(&json).unwrap_err()));
}

#[test]
fn decode_rejects_a_number_json_can_only_spell_as_an_overflow() {
    // Dart's jsonDecode turns 1e999 into Infinity: a "scalar" no column can
    // hold and no peer can re-encode.
    let json = SyncRow::new("practices", practice_fields("P1", &hlc(), "t"))
        .encode()
        .unwrap();
    let overflowed = json.replacen("\"master_volume\":0.5", "\"master_volume\":1e999", 1);
    assert_ne!(overflowed, json);
    assert!(is_format(&SyncRow::decode(&overflowed).unwrap_err()));
    // A hand-built map carrying a non-finite double is refused the same way.
    let mut fields = practice_fields("P1", &hlc(), "t");
    fields.insert("master_volume".into(), f64::INFINITY.into());
    let row = SyncRow::new("practices", fields);
    assert!(row.encode().is_err(), "no peer can re-encode it");
}

#[test]
fn decode_rejects_a_payload_missing_the_identity_trio_in_fields() {
    let h = hlc();
    let mut fields = practice_fields("P1", &h, "t");
    let row_id = fields["id"].clone();
    fields.shift_remove("hlc_packed");
    let mut f = serde_json::Map::new();
    for (k, v) in &fields {
        f.insert(k.clone(), v.to_json().unwrap());
    }
    let json = json!({
        "table": "practices",
        "id": row_id.as_str(),
        "schemaV": 1,
        "hlcPacked": h.packed(),
        "fields": Value::Object(f),
        "fieldClocks": null,
    });
    assert!(is_format(
        &SyncRow::from_json(json.as_object().unwrap()).unwrap_err()
    ));
}

#[test]
fn decode_rejects_non_json_and_non_object_payloads() {
    assert!(is_format(&SyncRow::decode("not json").unwrap_err()));
    assert!(is_format(&SyncRow::decode("[1,2]").unwrap_err()));
}

/// The wire JSON object of [fields] under a matching envelope.
fn envelope_of(fields: &Fields) -> serde_json::Map<String, Value> {
    let mut f = serde_json::Map::new();
    for (k, v) in fields {
        f.insert(k.clone(), v.to_json().unwrap());
    }
    let get = |k: &str| {
        fields
            .get(k)
            .and_then(FieldValue::to_json)
            .unwrap_or(Value::Null)
    };
    let json = json!({
        "table": "practices",
        "id": get("id"),
        "schemaV": get("schema_v"),
        "hlcPacked": get("hlc_packed"),
        "fields": Value::Object(f),
        "fieldClocks": null,
    });
    json.as_object().unwrap().clone()
}

#[test]
fn decode_rejects_a_mistyped_or_missing_hlc_quad() {
    // The applier must never hit a type error deep inside an apply.
    let h = hlc();
    let json_with = |patch: &[(&str, Option<FieldValue>)]| {
        let mut fields = practice_fields("P1", &h, "t");
        for (k, v) in patch {
            match v {
                Some(v) => {
                    fields.insert((*k).to_string(), v.clone());
                }
                None => {
                    fields.shift_remove(*k);
                }
            }
        }
        envelope_of(&fields)
    };
    // Well-formed control: the helper itself must decode.
    assert_eq!(
        SyncRow::from_json(&json_with(&[])).unwrap().hlc().unwrap(),
        h
    );
    for patch in [
        ("hlc_wall", Some(FieldValue::from(h.wall_ms.to_string()))), // string, not int
        ("hlc_ctr", Some(FieldValue::from("seven"))),
        ("hlc_node", Some(FieldValue::from(""))),
        ("hlc_wall", None),
    ] {
        let err = SyncRow::from_json(&json_with(&[patch.clone()])).unwrap_err();
        assert!(is_format(&err), "{patch:?}");
    }
}

#[test]
fn decode_rejects_an_hlc_packed_that_disagrees_with_the_quad() {
    // A forged LWW key must not pass the drift gate on an honest-looking
    // quad.
    let h = hlc();
    let forged = Hlc::new(h.wall_ms + 999_999_999, 0, h.node.clone()).packed();
    let mut fields = practice_fields("P1", &h, "t");
    fields.insert("hlc_packed".into(), forged.clone().into());
    // The envelope agrees with the fields — only the quad (what the drift
    // gate judges) tells the truth.
    let json = envelope_of(&fields);
    assert_eq!(json["hlcPacked"], Value::String(forged));
    assert!(is_format(&SyncRow::from_json(&json).unwrap_err()));
}

#[test]
fn decode_takes_the_quad_bounds_and_the_node_grammar_into_account() {
    let h = hlc();
    let check = |wall: i64, ctr: i64, node: &str| {
        let mut fields = practice_fields("P1", &h, "t");
        fields.insert("hlc_wall".into(), wall.into());
        fields.insert("hlc_ctr".into(), ctr.into());
        fields.insert("hlc_node".into(), node.into());
        fields.insert(
            "hlc_packed".into(),
            Hlc::new(wall, ctr, node).packed().into(),
        );
        SyncRow::check_hlc_consistency(&fields)
    };
    assert!(check(1, 0, &id("N")).is_ok());
    assert!(check(1, 0, &format!("{}!{}", id("N"), id("D"))).is_ok());
    assert!(check(1, HlcClock::MAX_COUNTER + 1, &id("N")).is_err());
    assert!(check(-1, 0, &id("N")).is_err());
    assert!(check(10_000_000_000_000, 0, &id("N")).is_err());
    assert!(check(1, 0, "short").is_err());
    assert!(check(1, 0, &format!("{}!", id("N"))).is_err());
    let too_long = vec![id("N"); HlcClock::MAX_NODE_SEGMENTS + 1].join("!");
    assert!(check(1, 0, &too_long).is_err());
    let at_ceiling = vec![id("N"); HlcClock::MAX_NODE_SEGMENTS].join("!");
    assert!(check(1, 0, &at_ceiling).is_ok());
}

// --- synced table registry -------------------------------------------------

#[test]
fn the_registry_contains_exactly_the_sync_cols_tables_and_none_of_the_local_only_bookkeeping() {
    let registry: HashSet<&str> = synced_table_registry().keys().copied().collect();
    assert_eq!(
        registry,
        HashSet::from([
            "practices",
            "interpretations",
            "tracks",
            "clips",
            "practice_notes",
            "technique_markers",
            "fx_buses",
            "techniques",
            "technique_media",
            "assets",
        ])
    );
}

#[test]
fn every_json_column_of_a_synced_table_has_a_registered_decoder() {
    // Derive the JSON columns STRUCTURALLY from the schema. A new `*_json`
    // column that ships without a decoder is refused wholesale at the door
    // (every record of that table would quarantine), so this must fail here
    // rather than in the field.
    let from_schema: HashSet<&str> = synced_table_registry()
        .values()
        .flat_map(|t| t.columns.iter())
        .map(|c| c.name)
        .filter(|n| n.ends_with("_json"))
        .collect();
    assert!(!from_schema.is_empty()); // the derivation itself must keep working
    let missing: Vec<&&str> = from_schema
        .iter()
        .filter(|c| json_column_decoder(c).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "the JSON decoders must say what each *_json column holds: {missing:?}"
    );
}

// --- SyncRowBuilder ----------------------------------------------------------

fn seed(db: &vocara_store::db::AppDatabase) {
    let h = hlc();
    db.insert(&PracticeRow {
        id: id("P1"),
        title: "Song".into(),
        master_volume: 0.75,
        created_at: 1,
        updated_at: 2,
        hlc_wall: h.wall_ms,
        hlc_ctr: h.counter,
        hlc_node: h.node.clone(),
        hlc_packed: h.packed(),
        ..Default::default()
    })
    .unwrap();
    db.insert(&TrackRow {
        id: id("T1"),
        practice_id: id("P1"),
        kind: "vocal".into(),
        muted: true,
        created_at: 1,
        updated_at: 2,
        hlc_wall: h.wall_ms,
        hlc_ctr: h.counter,
        hlc_node: h.node.clone(),
        hlc_packed: h.packed(),
        ..Default::default()
    })
    .unwrap();
}

#[test]
fn the_builder_produces_all_of_a_table_s_columns_in_db_value_conventions() {
    let db = memory_db();
    let builder = SyncRowBuilder::new(db.clone());
    seed(&db);
    let row = builder.build("tracks", &id("T1")).unwrap().unwrap();
    assert_eq!(row.table(), "tracks");
    // Every column of the schema is present — nothing is projected out.
    let expected: Vec<&str> = synced_table("tracks")
        .unwrap()
        .columns
        .iter()
        .map(|c| c.name)
        .collect();
    let got: Vec<&str> = row.fields().keys().map(String::as_str).collect();
    assert_eq!(got, expected);
    assert_eq!(row.field("muted"), Some(&FieldValue::Int(1))); // boolean rides as int
    assert!(matches!(row.field("volume"), Some(FieldValue::Double(_))));
    assert_eq!(row.field("deleted_at"), Some(&NULL));
    assert_eq!(row.id().unwrap(), id("T1"));
    assert_eq!(row.hlc_packed().unwrap(), hlc().packed());
    assert_eq!(row.schema_v().unwrap(), 1);
    assert!(row.field_clocks().is_none()); // v1 never sets the reserved slot
}

#[test]
fn the_builder_strips_the_per_device_asset_columns_from_the_wire_projection() {
    let db = memory_db();
    let builder = SyncRowBuilder::new(db.clone());
    let h = hlc();
    db.insert(&AssetRow {
        id: id("A1"),
        content_hash: "HASH_A1".into(),
        byte_size: 8,
        ext: "wav".into(),
        local_rel_path: Some("assets/HA/SH/HASH_A1.wav".into()),
        record_sync_state: "synced".into(),
        bytes_sync_state: "present".into(),
        remote_ref: Some("blob://HASH_A1".into()),
        created_at: 1,
        updated_at: 2,
        hlc_wall: h.wall_ms,
        hlc_ctr: h.counter,
        hlc_node: h.node.clone(),
        hlc_packed: h.packed(),
        ..Default::default()
    })
    .unwrap();
    let row = builder.build("assets", &id("A1")).unwrap().unwrap();
    // Exactly the schema minus the per-device set ("stripped from the wire
    // projection").
    let expected: HashSet<&str> = synced_table("assets")
        .unwrap()
        .columns
        .iter()
        .map(|c| c.name)
        .filter(|c| !per_device_columns("assets").contains(c))
        .collect();
    let got: HashSet<&str> = row.fields().keys().map(String::as_str).collect();
    assert_eq!(got, expected);
    assert_eq!(
        row.field("content_hash"),
        Some(&FieldValue::from("HASH_A1"))
    );
    for c in [
        "local_rel_path",
        "record_sync_state",
        "bytes_sync_state",
        "remote_ref",
    ] {
        assert!(!row.fields().contains_key(c), "{c}");
    }
}

#[test]
fn the_builder_returns_none_for_a_missing_row_id() {
    let db = memory_db();
    let builder = SyncRowBuilder::new(db.clone());
    seed(&db);
    assert!(builder.build("practices", &id("NOPE")).unwrap().is_none());
}

#[test]
fn the_builder_refuses_non_synced_tables() {
    let builder = SyncRowBuilder::new(memory_db());
    for table in ["oplog", "sync_state", "sync_quarantine", "no_such_table"] {
        assert!(
            matches!(builder.build(table, "x"), Err(Error::InvalidArgument(_))),
            "{table}"
        );
    }
}

#[test]
fn the_builder_ranks_parents_before_children() {
    let builder = SyncRowBuilder::new(memory_db());
    let ranks = builder.table_ranks().unwrap();
    let r = |t: &str| ranks[t];
    assert_eq!(r("assets"), 0);
    assert_eq!(r("techniques"), 0);
    assert!(r("practices") > r("assets"));
    assert!(r("interpretations") > r("practices"));
    assert!(r("fx_buses") > r("practices"));
    assert!(r("tracks") > r("interpretations"));
    assert!(r("tracks") > r("fx_buses"));
    assert!(r("clips") > r("tracks"));
    assert!(r("practice_notes") > r("tracks"));
    assert!(r("technique_markers") > r("tracks"));
    assert!(r("technique_media") > r("techniques"));
    // Memoized: the same map comes back.
    assert!(Arc::ptr_eq(&ranks, &builder.table_ranks().unwrap()));
}

#[test]
fn builder_to_wire_to_applier_a_built_row_applies_verbatim_on_another_device() {
    let db = memory_db();
    let builder = SyncRowBuilder::new(db.clone());
    seed(&db);
    let built = builder.build("practices", &id("P1")).unwrap().unwrap();
    let wire = built.encode().unwrap();

    let other = memory_db();
    let h = hlc();
    let applier = SyncApplier::new(
        other.clone(),
        Arc::new(HlcClock::new(id("OTHER")).with_now(move || h.wall_ms)),
        Arc::new(SyncRowBuilder::new(other.clone())),
        None,
    );
    let res = applier.apply(&[SyncRow::decode(&wire).unwrap()]);
    assert_eq!(res.applied, 1);
    assert_eq!(res.failed, 0);

    let landed: PracticeRow = other.find(&id("P1")).unwrap().unwrap();
    assert_eq!(landed.title, "Song");
    assert_eq!(landed.master_volume, 0.75);
    assert_eq!(landed.hlc_packed, hlc().packed());
    // And it projects back to the identical wire text.
    let rebuilt = SyncRowBuilder::new(other.clone())
        .build("practices", &id("P1"))
        .unwrap()
        .unwrap();
    assert_eq!(rebuilt.encode().unwrap(), wire);
}

// --- path-component id gate ------------------------------------------------

fn id_fields(row_id: &str) -> Fields {
    let node = id("N");
    fields! {
        "id" => row_id,
        "schema_v" => 1i64,
        "hlc_wall" => 1000i64,
        "hlc_ctr" => 0i64,
        "hlc_node" => node.clone(),
        "hlc_packed" => format!("0000000001000-000000-{node}"),
    }
}

#[test]
fn a_practice_id_the_media_services_could_not_use_is_refused_at_the_wire() {
    // practices.id and techniques.id become audio/<id>/ and
    // technique_media/<id>/, built and recursively deleted by the media
    // services under a strict shape. The wire's superset otherwise admits a
    // row that syncs cleanly and is then un-operable: every media call for it
    // fails, with nothing to fix it short of editing the database.
    assert!(is_format(
        &SyncRow::check_id_shapes("practices", &id_fields("p1")).unwrap_err()
    ));
    assert!(is_format(
        &SyncRow::check_id_shapes("techniques", &id_fields("short")).unwrap_err()
    ));
    // A real id passes.
    assert!(SyncRow::check_id_shapes("practices", &id_fields(&new_id())).is_ok());
}

#[test]
fn the_wire_rule_and_the_media_services_agree_exactly() {
    // Two rules that drift apart reintroduce the same gap from the other
    // side, so they read the ONE predicate.
    for row_id in [new_id(), format!("{:0<26}", "A"), "0".repeat(26)] {
        assert!(is_path_component_id(&row_id), "{row_id}");
        assert!(
            SyncRow::check_id_shapes("practices", &id_fields(&row_id)).is_ok(),
            "{row_id}"
        );
    }
    for row_id in [
        "p1".to_string(),
        "a".repeat(26),
        format!("{}/", "A".repeat(25)),
        format!("{}.", "A".repeat(25)),
    ] {
        assert!(!is_path_component_id(&row_id), "{row_id}");
    }
}

#[test]
fn every_id_is_26_to_36_characters_and_the_shapes_minted_and_seeded_today_all_fit() {
    let clip_fields = |row_id: &str, track_id: Option<&str>| {
        let mut f = id_fields(row_id);
        f.insert(
            "track_id".into(),
            track_id.map_or_else(new_id, str::to_string).into(),
        );
        f
    };
    for bad in ["SHORT".to_string(), "A".repeat(25), "A".repeat(37)] {
        assert!(
            SyncRow::check_id_shapes("clips", &clip_fields(&bad, None)).is_err(),
            "{bad}"
        );
        assert!(
            SyncRow::check_id_shapes("clips", &clip_fields(&new_id(), Some(&bad))).is_err(),
            "a reference is an id too: {bad}"
        );
    }
    for good in [
        new_id(),
        format!("{}-c0", new_id()),  // a legacy take's derived clip
        format!("{}__v1", new_id()), // a legacy practice's default interpretation
        "E2EPRACTICESEED00000000000".to_string(), // the e2e harness's seeded rows
        "A".repeat(36),
    ] {
        assert!(
            SyncRow::check_id_shapes("clips", &clip_fields(&good, None)).is_ok(),
            "{good}"
        );
    }
}

#[test]
fn a_child_table_keeps_the_superset() {
    // The legacy derived clip shape never names a directory.
    let track_id = new_id();
    let mut f = id_fields(&format!("{track_id}-c0"));
    f.insert("track_id".into(), track_id.into());
    assert!(SyncRow::check_id_shapes("clips", &f).is_ok());
}
