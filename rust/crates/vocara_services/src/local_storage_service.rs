//! JSON documents and asset directories under the app support directory
//! (port of `lib/data/services/local_storage_service.dart`).
//!
//! Persists JSON documents as one file per record, and hands out
//! per-collection directories for binary assets (e.g. imported/recorded
//! audio). Stateless apart from a cached root directory and the per-document
//! write locks.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Map, Value};
use vocara_model::util::{dart_json, id::new_id};

/// The macOS application support directory for [bundle_id]:
/// `~/Library/Application Support/<bundle id>` (inside the app's container
/// when sandboxed), which is what Flutter's `getApplicationSupportDirectory`
/// returns. Elsewhere, the platform's data directory plus the bundle id.
pub fn application_support_dir(bundle_id: &str) -> io::Result<PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no data directory"))?;
    Ok(base.join(bundle_id))
}

/// How the service finds its root the first time it needs it.
enum Root {
    Fixed(PathBuf),
    Lazy {
        resolve: Box<dyn Fn() -> io::Result<PathBuf> + Send + Sync>,
        cell: OnceLock<PathBuf>,
    },
}

/// Files under the app support directory (see the module doc).
pub struct LocalStorageService {
    root: Root,
    /// Per-file write locks: writes to the same document are serialized, so
    /// a burst (e.g. a settings slider firing a persist on every drag tick)
    /// can never interleave two truncating writes into a torn, invalid-JSON
    /// file. One failure does not poison the lock: each caller observes its
    /// own write's result.
    write_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl LocalStorageService {
    /// A service rooted at the application support directory of
    /// [bundle_id], resolved (and created) on first use.
    pub fn new(bundle_id: &str) -> Self {
        let bundle_id = bundle_id.to_string();
        Self::with_resolver(Box::new(move || {
            let dir = application_support_dir(&bundle_id)?;
            fs::create_dir_all(&dir)?;
            Ok(dir)
        }))
    }

    /// A service rooted at [root] (tests, and callers that already resolved it).
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        LocalStorageService {
            root: Root::Fixed(root.into()),
            write_locks: Mutex::new(HashMap::new()),
        }
    }

    /// A service whose root comes from [resolve], called once on first use.
    pub fn with_resolver(resolve: Box<dyn Fn() -> io::Result<PathBuf> + Send + Sync>) -> Self {
        LocalStorageService {
            root: Root::Lazy {
                resolve,
                cell: OnceLock::new(),
            },
            write_locks: Mutex::new(HashMap::new()),
        }
    }

    fn root(&self) -> io::Result<PathBuf> {
        match &self.root {
            Root::Fixed(p) => Ok(p.clone()),
            Root::Lazy { resolve, cell } => {
                if let Some(p) = cell.get() {
                    return Ok(p.clone());
                }
                let p = resolve()?;
                Ok(cell.get_or_init(|| p).clone())
            }
        }
    }

    fn collection_dir(&self, collection: &str) -> io::Result<PathBuf> {
        let dir = self.root()?.join(collection);
        if !dir.exists() {
            fs::create_dir_all(&dir)?;
        }
        Ok(dir)
    }

    /// The document, or `None` when there is none to keep: no file, or one
    /// that is not a JSON object. Fails when the file is there but could not
    /// be read: its values still exist, and a caller that took that as "no
    /// document" wrote its defaults over them.
    pub fn read(&self, collection: &str, id: &str) -> io::Result<Option<Map<String, Value>>> {
        let dir = self.collection_dir(collection)?;
        let file = dir.join(format!("{id}.json"));
        if !file.exists() {
            return Ok(None);
        }
        // Read, then decode: bytes that are not UTF-8 are a corrupt document,
        // not an unreadable one (taken as unreadable, they blocked every
        // write of a document that is only corrupt).
        let bytes = fs::read(&file)?;
        // A truncated / corrupt / non-object file (partial write after a
        // crash, manual edit) must not crash startup — treat it as "no
        // value", so the caller falls back to defaults.
        let Ok(text) = String::from_utf8(bytes) else {
            return Ok(None);
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(Some(map)),
            _ => Ok(None),
        }
    }

    /// Writes [json] as the document `<collection>/<id>.json`, serialized
    /// after any in-flight write of the same document, as Dart's `jsonEncode`
    /// would write it (so the Flutter app reads it back unchanged).
    pub fn write(&self, collection: &str, id: &str, json: &Map<String, Value>) -> io::Result<()> {
        let key = format!("{collection}/{id}");
        let lock = {
            let mut locks = self.write_locks.lock().unwrap_or_else(|e| e.into_inner());
            locks.entry(key).or_default().clone()
        };
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        self.write_atomic(collection, id, json)
    }

    /// Writes to a unique temp file then atomically renames over the target,
    /// so a reader (or a crash) never observes a half-written document.
    /// Unique across processes too: a second running instance shares this
    /// folder, and a per-instance counter gave both the same temp name, so
    /// two writes of one document tore it.
    fn write_atomic(
        &self,
        collection: &str,
        id: &str,
        json: &Map<String, Value>,
    ) -> io::Result<()> {
        let dir = self.collection_dir(collection)?;
        let file = dir.join(format!("{id}.json"));
        let tmp = PathBuf::from(format!("{}.{}.tmp", file.display(), new_id()));
        let text = dart_json::encode(&Value::Object(json.clone()))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let result = (|| {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            fs::rename(&tmp, &file)
        })();
        if result.is_err() && tmp.exists() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    /// Returns (creating if needed) a directory for storing binary assets
    /// that belong to [collection], e.g. `audio/`. Callers join their own
    /// file names.
    pub fn asset_dir(&self, collection: &str) -> io::Result<PathBuf> {
        self.collection_dir(collection)
    }

    /// Where [collection]'s assets live, without creating the COLLECTION
    /// directory. Guard checks and delete paths must use this instead of
    /// [`asset_dir`]: a delete must never mkdir the collection as a side
    /// effect, and resolving a path guard must still succeed while the
    /// storage tree is being torn down. Caveat, stated honestly: the FIRST
    /// call on a cold instance resolves the app-support ROOT, which may
    /// create that root directory — everything after (and every warmed
    /// instance) is pure path math.
    ///
    /// [`asset_dir`]: LocalStorageService::asset_dir
    pub fn asset_path(&self, collection: &str) -> io::Result<PathBuf> {
        Ok(self.root()?.join(collection))
    }

    /// The root directory (the app support directory), resolving it if needed.
    pub fn root_dir(&self) -> io::Result<PathBuf> {
        self.root()
    }
}

/// Lists the entries of [dir] by file name (tests and sweeps).
pub fn file_names(dir: &Path) -> io::Result<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => unreachable!(),
        }
    }

    fn write_raw(root: &Path, bytes: &[u8]) {
        let dir = root.join("settings");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("app.json"), bytes).unwrap();
    }

    #[test]
    fn two_instances_sharing_the_folder_never_tear_a_document() {
        // A second running app shares the library. Each instance numbered its
        // temp files from 0, so both wrote the same temp file: one rename
        // failed and the document was left as torn JSON.
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(LocalStorageService::with_root(tmp.path()));
        let other = Arc::new(LocalStorageService::with_root(tmp.path()));
        let big = map(json!({"blob": "x".repeat(200_000)}));
        for i in 0..5 {
            let s = storage.clone();
            let b = big.clone();
            let a = std::thread::spawn(move || s.write("settings", "app", &b).unwrap());
            let o = other.clone();
            let small = map(json!({"a": i}));
            let c = std::thread::spawn(move || o.write("settings", "app", &small).unwrap());
            a.join().unwrap();
            c.join().unwrap();
            let read = storage.read("settings", "app").unwrap().unwrap();
            assert!(read == big || read == map(json!({"a": i})));
        }
        assert_eq!(
            file_names(&tmp.path().join("settings")).unwrap(),
            ["app.json"]
        );
    }

    #[test]
    fn a_document_reads_back_as_written() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = LocalStorageService::with_root(tmp.path());
        storage
            .write("settings", "app", &map(json!({"a": 1})))
            .unwrap();
        assert_eq!(
            storage.read("settings", "app").unwrap(),
            Some(map(json!({"a": 1})))
        );
    }

    #[test]
    fn no_file_or_one_that_is_not_a_json_object_is_no_document() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = LocalStorageService::with_root(tmp.path());
        assert_eq!(storage.read("settings", "app").unwrap(), None);
        write_raw(tmp.path(), b"[1, 2]");
        assert_eq!(storage.read("settings", "app").unwrap(), None);
        write_raw(tmp.path(), b"{\"a\": ");
        assert_eq!(storage.read("settings", "app").unwrap(), None);
    }

    #[test]
    fn bytes_that_are_not_utf8_are_a_corrupt_document_not_an_unreadable_one() {
        // Taken as unreadable, the settings it held were never written again.
        let tmp = tempfile::tempdir().unwrap();
        let storage = LocalStorageService::with_root(tmp.path());
        let mut bytes = b"{\"outputDeviceName\":\"Ren".to_vec();
        bytes.push(0xE9); // é in Latin-1, as an editor might save it
        bytes.extend_from_slice(b"\"}");
        write_raw(tmp.path(), &bytes);
        assert_eq!(storage.read("settings", "app").unwrap(), None);
    }

    #[test]
    fn documents_are_written_as_dart_writes_them() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = LocalStorageService::with_root(tmp.path());
        storage
            .write(
                "settings",
                "app",
                &map(json!({"volume": 1.0, "name": "x", "n": 3})),
            )
            .unwrap();
        let text = fs::read_to_string(tmp.path().join("settings/app.json")).unwrap();
        assert_eq!(text, r#"{"volume":1.0,"name":"x","n":3}"#);
    }

    #[test]
    fn asset_path_does_not_create_the_collection_but_asset_dir_does() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = LocalStorageService::with_root(tmp.path());
        let p = storage.asset_path("audio").unwrap();
        assert_eq!(p, tmp.path().join("audio"));
        assert!(!p.exists());
        let d = storage.asset_dir("audio").unwrap();
        assert!(d.is_dir());
    }
}
