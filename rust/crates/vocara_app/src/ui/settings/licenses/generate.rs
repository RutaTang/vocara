//! Generates `assets/licenses/crates.json`, the license list of every crate
//! the shipped app links, and checks the committed copy is current.
//!
//! The shipped app is the macOS bundle `packaging/macos/bundle.sh` builds:
//! `vocara_app` with the `device` feature, for Apple silicon and Intel (the
//! universal build). `cargo metadata` resolves that build from `Cargo.lock`;
//! the list is every crate reachable from `vocara_app` through NORMAL
//! dependencies (build scripts and their dependencies, and dev-dependencies,
//! do not ship), minus the workspace's own path crates (first-party: covered
//! by the app's copyright, as the Dart list left out the engine's own code).
//! Each crate's license texts are the license files it packages (`LICENSE*`,
//! `LICENCE*`, `COPYING*`, `NOTICE*`, `UNLICENSE*`, `COPYRIGHT*`, and its
//! manifest's `license-file`), read from cargo's registry checkout,
//! line endings normalized and stored once however many crates share them.
//!
//! When a dependency changes, the check fails; regenerate the list with
//!
//! ```text
//! VOCARA_UPDATE_LICENSES=1 cargo test -p vocara_app the_crate_license_list_is_current
//! ```
//!
//! and commit `crates/vocara_app/assets/licenses/crates.json`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

/// The shipped targets (the universal macOS build).
const TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];

/// The shipped features.
const FEATURES: &str = "vocara_app/device";

/// Where the list lives.
fn data_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/licenses/crates.json")
}

/// `cargo metadata` for the shipped build.
fn metadata() -> Value {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let mut command = Command::new(cargo);
    command
        .args(["metadata", "--format-version", "1", "--locked"])
        .args(["--features", FEATURES])
        .arg("--manifest-path")
        .arg(&manifest);
    for target in TARGETS {
        command.args(["--filter-platform", target]);
    }
    let output = command.output().expect("cargo metadata runs");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata prints JSON")
}

/// The third-party packages the shipped app links, by name then version.
fn shipped_packages(meta: &Value) -> Vec<&Value> {
    let packages: HashMap<&str, &Value> = meta["packages"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p)))
        .collect();
    let nodes: HashMap<&str, &Value> = meta["resolve"]["nodes"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();
    let root = packages
        .values()
        .find(|p| p["name"] == "vocara_app")
        .and_then(|p| p["id"].as_str())
        .expect("vocara_app in the workspace");
    let mut seen = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = nodes.get(id) else { continue };
        for dep in node["deps"].as_array().map_or(&[][..], Vec::as_slice) {
            // A normal dependency (kind null) ships; build and dev ones don't.
            let normal = dep["dep_kinds"]
                .as_array()
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .any(|k| k["kind"].is_null());
            if let (true, Some(pkg)) = (normal, dep["pkg"].as_str()) {
                stack.push(pkg);
            }
        }
    }
    let mut shipped: Vec<&Value> = seen
        .into_iter()
        .filter_map(|id| packages.get(id).copied())
        // Path crates (the workspace's own, the engine) are first-party.
        .filter(|p| !p["source"].is_null())
        .collect();
    shipped.sort_by(|a, b| {
        (a["name"].as_str(), a["version"].as_str())
            .cmp(&(b["name"].as_str(), b["version"].as_str()))
    });
    shipped
}

const LICENSE_FILE_PREFIXES: [&str; 6] = [
    "LICENSE",
    "LICENCE",
    "COPYING",
    "NOTICE",
    "UNLICENSE",
    "COPYRIGHT",
];

/// A license file's text: no byte-order mark, `\n` line ends, no trailing
/// blank space.
fn normalize(text: &str) -> String {
    text.trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_end()
        .to_string()
}

/// The license files `package` ships, by file name, read.
fn license_texts(package: &Value) -> Vec<String> {
    let Some(dir) = package["manifest_path"]
        .as_str()
        .and_then(|m| Path::new(m).parent())
    else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .filter(|p| {
                    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                        let upper = n.to_uppercase();
                        LICENSE_FILE_PREFIXES.iter().any(|x| upper.starts_with(x))
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    if let Some(declared) = package["license_file"].as_str() {
        let declared = dir.join(declared);
        if declared.is_file() && !files.contains(&declared) {
            files.push(declared);
        }
    }
    files
        .iter()
        .filter_map(|f| std::fs::read(f).ok())
        .map(|bytes| normalize(&String::from_utf8_lossy(&bytes)))
        .filter(|t| !t.is_empty())
        .collect()
}

/// The list, as committed.
fn generate() -> String {
    let meta = metadata();
    let mut texts: Vec<String> = Vec::new();
    let mut text_index: HashMap<String, usize> = HashMap::new();
    let crates: Vec<Value> = shipped_packages(&meta)
        .into_iter()
        .map(|p| {
            let ids: Vec<usize> = license_texts(p)
                .into_iter()
                .map(|t| {
                    *text_index.entry(t.clone()).or_insert_with(|| {
                        texts.push(t);
                        texts.len() - 1
                    })
                })
                .collect();
            json!({
                "name": p["name"],
                "version": p["version"],
                "license": p["license"],
                "repository": p["repository"],
                "texts": ids,
            })
        })
        .collect();
    let doc = json!({
        "generated": "by ui::settings::licenses (VOCARA_UPDATE_LICENSES=1 cargo test -p vocara_app the_crate_license_list_is_current); do not edit",
        "targets": TARGETS,
        "features": [FEATURES],
        "crates": crates,
        "texts": texts,
    });
    let mut out = serde_json::to_string_pretty(&doc).expect("JSON");
    out.push('\n');
    out
}

#[test]
fn the_crate_license_list_is_current() {
    let fresh = generate();
    let committed = std::fs::read_to_string(data_path()).unwrap_or_default();
    if fresh == committed {
        return;
    }
    if std::env::var_os("VOCARA_UPDATE_LICENSES").is_some() {
        std::fs::write(data_path(), &fresh).expect("write the license list");
        return;
    }
    panic!(
        "assets/licenses/crates.json is out of date with Cargo.lock: run \
         `VOCARA_UPDATE_LICENSES=1 cargo test -p vocara_app \
         the_crate_license_list_is_current` and commit the file"
    );
}

#[test]
fn license_files_are_normalized() {
    assert_eq!(normalize("\u{feff}MIT\r\n\r\nText\r\n  \n"), "MIT\n\nText");
    assert_eq!(normalize("a\rb"), "a\nb");
}
