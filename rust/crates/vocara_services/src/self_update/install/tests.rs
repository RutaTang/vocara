//! The installer's decisions, on every platform: the identity it pins and
//! the requirement it builds, the notarization policy, what it reads out of
//! `hdiutil`'s property lists (hostile volume names included), why a copy
//! cannot replace itself, and the swap helper — its text, and the script
//! itself run against dummy bundles with a recording opener (with `ditto`
//! on a Mac; elsewhere a `cp -R` stands in for it).

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

fn identity(identifier: &str, team: &str) -> SigningIdentity {
    SigningIdentity {
        identifier: identifier.into(),
        team: team.into(),
    }
}

/// Whether this process may write where it has no permission (root): the
/// checks that rest on permissions say nothing then.
pub(crate) fn root() -> bool {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// The requirement names an Apple anchor, the Developer ID intermediate and
/// leaf OIDs, the running app's own identifier, and the team from the leaf
/// certificate — and it carries the leading `=`, since codesign reads a
/// `-R` argument without it as a path to a compiled requirement. (On a Mac,
/// `macos::tests` has `csreq` compile it.)
#[test]
fn the_requirement_pins_developer_id_the_identifier_and_the_team() {
    let arg = requirement_arg(&identity("com.rutatang.vocara", "TP7LCYW3NY")).unwrap();
    assert!(arg.starts_with("=anchor apple generic and "), "{arg}");
    for clause in [
        "identifier \"com.rutatang.vocara\"",
        "certificate 1[field.1.2.840.113635.100.6.2.6] exists",
        "certificate leaf[field.1.2.840.113635.100.6.1.13] exists",
        "certificate leaf[subject.OU] = \"TP7LCYW3NY\"",
    ] {
        assert!(arg.contains(clause), "missing {clause:?} in {arg}");
    }
    assert_eq!(arg.matches(" and ").count(), 4, "{arg}");
}

/// Both values are interpolated into a requirement expression, so anything
/// that could close a quote and bolt on another clause is refused outright.
#[test]
fn the_requirement_refuses_values_that_could_reshape_it() {
    for bad in [
        "",
        "AB\" or anchor apple generic and \"",
        "AB CDE",
        "AB-CD",
        "TP7LCYW3NY\n",
        "ÄBC",
    ] {
        assert!(
            requirement_arg(&identity("com.rutatang.vocara", bad)).is_err(),
            "team {bad:?}"
        );
    }
    for bad in [
        "",
        "com.x\" or anchor apple",
        "com x",
        "com/x",
        "com.x)",
        "com.x\nand",
    ] {
        assert!(
            requirement_arg(&identity(bad, "TP7LCYW3NY")).is_err(),
            "identifier {bad:?}"
        );
    }
}

#[test]
fn the_signing_identity_is_read_from_codesigns_output() {
    let text = "Executable=/Applications/vocara.app/Contents/MacOS/vocara\n\
                Identifier=com.rutatang.vocara\n\
                Format=app bundle with Mach-O universal (x86_64 arm64)\n\
                CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=30+7 location=embedded\n\
                Authority=Developer ID Application: Weizhi Tang (TP7LCYW3NY)\n\
                Authority=Developer ID Certification Authority\n\
                Authority=Apple Root CA\n\
                TeamIdentifier=TP7LCYW3NY\n";
    assert_eq!(
        parse_signing_identity(text).unwrap(),
        identity("com.rutatang.vocara", "TP7LCYW3NY")
    );
    // Ad hoc (bundle.sh's local builds): no team, nothing to anchor to.
    let err = parse_signing_identity(
        "Identifier=com.rutatang.vocara\nSignature=adhoc\nTeamIdentifier=not set\n",
    )
    .unwrap_err();
    assert!(err.contains("no Team Identifier"), "{err}");
    // Unsigned: codesign prints neither.
    assert!(parse_signing_identity("code object is not signed at all\n").is_err());
    // A value outside the plain character set is refused, not escaped.
    assert!(parse_signing_identity("Identifier=a\"b\nTeamIdentifier=ABC\n").is_err());
    assert!(parse_signing_identity("Identifier=a\nTeamIdentifier=A\"B\n").is_err());
}

/// The shipped stance, pinned: an un-notarized image is refused in-app.
///
/// This pins the Rust half alone: nothing in `cargo test` reads a workflow.
/// The drift that hurts is the other one — a release.yml edit that lets a
/// signed DMG ship without a ticket while this still demands one — and that
/// is caught in the workflow, by release.yml's "A signed DMG is a notarized
/// DMG" step, its own step so that narrowing the notarizing step cannot
/// carry the assertion away with it.
#[test]
fn the_shipped_policy_demands_a_notarization_ticket() {
    let err = notarization_gate(Err("rejected".into()), REQUIRE_NOTARIZATION)
        .expect_err("an un-notarized image must not be installed in-app");
    assert!(err.contains("not notarized"), "{err}");
    assert_eq!(notarization_gate(Ok(()), REQUIRE_NOTARIZATION), Ok(None));
}

/// The admitting branch is still reachable and still not silent, so a
/// future rollback to `false` cannot make an un-notarized install quiet.
#[test]
fn admitting_an_unnotarized_image_still_says_why() {
    let admitted = notarization_gate(Err("rejected".into()), false).unwrap();
    assert!(
        admitted
            .as_deref()
            .is_some_and(|r| r.contains("not notarized")),
        "{admitted:?}"
    );
    assert_eq!(notarization_gate(Ok(()), false), Ok(None));
}

#[test]
fn the_bundle_is_found_from_the_executable() {
    assert_eq!(
        installed_bundle_of(Path::new("/Applications/vocara.app/Contents/MacOS/vocara")),
        Some(PathBuf::from("/Applications/vocara.app"))
    );
    assert_eq!(
        installed_bundle_of(Path::new("/Applications/Vocara.APP/Contents/MacOS/vocara")),
        Some(PathBuf::from("/Applications/Vocara.APP"))
    );
    // A `cargo run` build, and the tests: no bundle, nothing to swap.
    assert_eq!(
        installed_bundle_of(Path::new("/src/rust/target/debug/vocara")),
        None
    );
    assert_eq!(installed_bundle_of(Path::new("vocara")), None);
}

/// A bundle Vocara could not replace is refused before anything is
/// downloaded, with a reason, instead of after Vocara has quit for the swap.
#[test]
fn a_bundle_this_user_cannot_replace_is_refused_up_front() {
    use std::os::unix::fs::PermissionsExt;
    let root_dir = tempfile::tempdir().unwrap();
    let apps = root_dir.path().join("Applications");
    let bundle = apps.join("vocara.app");
    std::fs::create_dir_all(bundle.join("Contents")).unwrap();
    assert!(replaceable(&bundle).is_ok());

    let translocated = root_dir
        .path()
        .join("private/var/folders/x/AppTranslocation/0A1B/d/vocara.app");
    std::fs::create_dir_all(&translocated).unwrap();
    let err = replaceable(&translocated).unwrap_err();
    assert!(err.contains("a temporary copy that macOS made"), "{err}");

    let missing = root_dir.path().join("Gone/vocara.app");
    let err = replaceable(&missing).unwrap_err();
    assert!(
        err.contains("cannot modify the folder Vocara is installed in"),
        "{err}"
    );

    if root() {
        eprintln!("skipped in part: root may write a read-only folder");
        return;
    }
    // The folder not writable: the helper's first `mv` fails.
    std::fs::set_permissions(&apps, std::fs::Permissions::from_mode(0o555)).unwrap();
    let err = replaceable(&bundle).unwrap_err();
    std::fs::set_permissions(&apps, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(err.contains("cannot modify the folder"), "{err}");
    // The bundle not writable: it cannot be deleted once replaced.
    std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o555)).unwrap();
    let err = replaceable(&bundle).unwrap_err();
    std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(err.contains("cannot modify Vocara's app"), "{err}");
}

/// The app on an image is the folder itself: a link — verified at its
/// target, then copied from wherever it pointed by then — is refused.
#[test]
fn the_app_on_an_image_is_a_folder_not_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir_all(real.join("vocara.app/Contents")).unwrap();
    assert_eq!(app_on_image(&real), Ok(real.join("vocara.app")));
    let linked = dir.path().join("linked");
    std::fs::create_dir_all(&linked).unwrap();
    std::os::unix::fs::symlink(real.join("vocara.app"), linked.join("vocara.app")).unwrap();
    assert!(app_on_image(&linked)
        .unwrap_err()
        .contains("not an app folder"));
    let file = dir.path().join("file");
    std::fs::create_dir_all(&file).unwrap();
    std::fs::write(file.join("vocara.app"), "not a bundle").unwrap();
    assert!(app_on_image(&file)
        .unwrap_err()
        .contains("not an app folder"));
    assert!(app_on_image(&dir.path().join("empty"))
        .unwrap_err()
        .contains("no vocara.app"));
}

// ------------------------------------------------------------ hdiutil

/// What `hdiutil attach -plist` prints for an HFS+ image (release.yml makes
/// Vocara's with `-fs HFS+`), its tabs aside: the volume's entity first,
/// the whole disk's after it.
const HFS_ATTACH_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>system-entities</key>
  <array>
    <dict>
      <key>content-hint</key>
      <string>Apple_HFS</string>
      <key>dev-entry</key>
      <string>/dev/disk12s1</string>
      <key>mount-point</key>
      <string>/Volumes/Vocara &amp; &lt;Test&gt;</string>
      <key>potentially-mountable</key>
      <true/>
      <key>unmapped-content-hint</key>
      <string>48465300-0000-11AA-AA11-00306543ECAC</string>
      <key>volume-kind</key>
      <string>hfs</string>
    </dict>
    <dict>
      <key>content-hint</key>
      <string>GUID_partition_scheme</string>
      <key>dev-entry</key>
      <string>/dev/disk12</string>
      <key>potentially-mountable</key>
      <false/>
      <key>unmapped-content-hint</key>
      <string>GUID_partition_scheme</string>
    </dict>
  </array>
</dict>
</plist>
"#;

/// An `hdiutil attach -plist` document of `entities`, each a list of string
/// fields written as XML text.
fn attach_plist(entities: &[&[(&str, &str)]]) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n\
         <dict>\n<key>system-entities</key>\n<array>\n",
    );
    for fields in entities {
        xml.push_str("<dict>\n");
        for (key, value) in *fields {
            xml.push_str(&format!("<key>{key}</key>\n<string>{value}</string>\n"));
        }
        xml.push_str("<key>potentially-mountable</key>\n<true/>\n</dict>\n");
    }
    xml.push_str("</array>\n</dict>\n</plist>\n");
    xml
}

/// The update volume is read from `hdiutil`'s property list: the first
/// entity with a mount point (and a device) — its name's escapes decoded,
/// and whole even where it holds the tabs and newlines that cut the text
/// output's columns.
#[test]
fn the_update_volume_is_read_from_hdiutils_plist() {
    let volume = |mount_point: &str| Some(PathBuf::from(mount_point));
    assert_eq!(
        parse_attach_plist(HFS_ATTACH_PLIST),
        volume("/Volumes/Vocara & <Test>")
    );
    // APFS: the volume on a container disk listed after the image's.
    let apfs = attach_plist(&[
        &[("dev-entry", "/dev/disk12")],
        &[
            ("content-hint", "Apple_APFS"),
            ("dev-entry", "/dev/disk12s1"),
        ],
        &[
            ("dev-entry", "/dev/disk14s1"),
            ("mount-point", "/Volumes/Vocara"),
            ("volume-kind", "apfs"),
        ],
        &[("dev-entry", "/dev/disk14")],
    ]);
    assert_eq!(parse_attach_plist(&apfs), volume("/Volumes/Vocara"));
    // A hostile volume name: escapes, a tab, a newline and what reads like
    // another mount and device in the text output — one path here.
    let crafted = attach_plist(&[&[
        ("dev-entry", "/dev/disk5s1"),
        (
            "mount-point",
            "/Volumes/&#x56;ocara\t/Volumes/Evil\n/dev/disk1&#33;",
        ),
    ]]);
    assert_eq!(
        parse_attach_plist(&crafted),
        volume("/Volumes/Vocara\t/Volumes/Evil\n/dev/disk1!")
    );
    // Markup in a name is text, never structure.
    let markup = attach_plist(&[&[
        ("dev-entry", "/dev/disk6s1"),
        (
            "mount-point",
            "/Volumes/a&lt;/string&gt;&lt;key&gt;mount-point&lt;/key&gt;&lt;string&gt;/etc",
        ),
    ]]);
    assert_eq!(
        parse_attach_plist(&markup),
        volume("/Volumes/a</string><key>mount-point</key><string>/etc")
    );
    // No volume: nothing mounted, a relative mount point, a device that is
    // not one, the old text output, a list cut short, a tag that does not
    // close its own, an unknown escape, trailing junk.
    let unmounted = attach_plist(&[&[("dev-entry", "/dev/disk4")]]);
    let relative = attach_plist(&[&[("dev-entry", "/dev/disk4s1"), ("mount-point", "Volumes/V")]]);
    let deviceless = attach_plist(&[&[("dev-entry", "disk4s1"), ("mount-point", "/Volumes/V")]]);
    let unclosed = apfs.replace("</key>", "</string>");
    let unknown = crafted.replace("&#33;", "&bang;");
    let trailing = format!("{HFS_ATTACH_PLIST}<plist/>");
    for xml in [
        unmounted.as_str(),
        relative.as_str(),
        deviceless.as_str(),
        "/dev/disk4s1\tApple_HFS\t/Volumes/Vocara\n",
        &HFS_ATTACH_PLIST[..HFS_ATTACH_PLIST.len() / 2],
        unclosed.as_str(),
        unknown.as_str(),
        trailing.as_str(),
        "",
    ] {
        assert_eq!(parse_attach_plist(xml), None, "{xml}");
    }
}

/// An `hdiutil info -plist` document listing `images`, each the path it was
/// attached by and the device nodes of its entities, in order.
fn info_plist(images: &[(&str, &[&str])]) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
         <key>framework</key>\n<string>683.100.3</string>\n<key>images</key>\n<array>\n",
    );
    for (path, nodes) in images {
        xml.push_str(&format!(
            "<dict>\n<key>autodiskmount</key>\n<false/>\n<key>blockcount</key>\n\
             <integer>40960</integer>\n<key>image-path</key>\n<string>{path}</string>\n\
             <key>system-entities</key>\n<array>\n"
        ));
        for node in *nodes {
            xml.push_str(&format!(
                "<dict>\n<key>content-hint</key>\n<string>Apple_HFS</string>\n\
                 <key>dev-entry</key>\n<string>{node}</string>\n</dict>\n"
            ));
        }
        xml.push_str("</array>\n</dict>\n");
    }
    xml.push_str("</array>\n</dict>\n</plist>\n");
    xml
}

/// The update image is found in `hdiutil info` by the file it was attached
/// from, never by a device node: listed, it is its first whole disk;
/// not listed, it is detached, whatever disk now has the number it had.
#[test]
fn the_update_image_is_found_in_hdiutil_info_by_its_file() {
    let ours = Path::new(
        "/Users/me/Library/Application Support/com.rutatang.vocara/updates/dl-1-0/Vocara-9.9.9.dmg",
    );
    let disk = |node: &str| Some(Some(PathBuf::from(node)));
    let apfs: &[&str] = &[
        "/dev/disk12",
        "/dev/disk12s1",
        "/dev/disk14",
        "/dev/disk14s1",
    ];
    let listed = info_plist(&[
        ("/Library/Other.dmg", &["/dev/disk4", "/dev/disk4s1"]),
        (&ours.to_string_lossy(), apfs),
    ]);
    assert_eq!(parse_info_plist(&listed, ours), disk("/dev/disk12"));
    // Ejected by someone else, and its number taken by another image.
    let reused = info_plist(&[("/Users/me/Other.dmg", apfs)]);
    assert_eq!(parse_info_plist(&reused, ours), Some(None));
    // Nothing attached at all, however `hdiutil` says it.
    let none = info_plist(&[]);
    assert_eq!(parse_info_plist(&none, ours), Some(None));
    let empty = none.replace("<array>\n</array>", "<array/>");
    assert_eq!(parse_info_plist(&empty, ours), Some(None));
    let absent = "<plist version=\"1.0\">\n<dict>\n<key>framework</key>\n\
                  <string>683</string>\n</dict>\n</plist>\n";
    assert_eq!(parse_info_plist(absent, ours), Some(None));
    // No whole disk listed: any node of the image detaches it.
    let partial = info_plist(&[(&ours.to_string_lossy(), &["/dev/disk9s1"])]);
    assert_eq!(parse_info_plist(&partial, ours), disk("/dev/disk9s1"));

    // Attached by another spelling of the same file: through a symlinked
    // directory.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("real")).unwrap();
    let file = dir.path().join("real/Vocara.dmg");
    std::fs::write(&file, b"image").unwrap();
    std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
    let file = file.canonicalize().unwrap();
    let spelled = dir.path().join("link/Vocara.dmg");
    let linked = info_plist(&[(&spelled.to_string_lossy(), &["/dev/disk7"])]);
    assert_eq!(parse_info_plist(&linked, &file), disk("/dev/disk7"));
    // An exact entry is looked for in the whole list before any other
    // entry's path is resolved.
    let both = info_plist(&[
        (&spelled.to_string_lossy(), &["/dev/disk7"]),
        (&file.to_string_lossy(), &["/dev/disk8"]),
    ]);
    assert_eq!(parse_info_plist(&both, &file), disk("/dev/disk8"));
    // Another name is another file, even one linked to it.
    std::os::unix::fs::symlink(&file, dir.path().join("Other.dmg")).unwrap();
    let other = info_plist(&[(
        &dir.path().join("Other.dmg").to_string_lossy(),
        &["/dev/disk9"],
    )]);
    assert_eq!(parse_info_plist(&other, &file), Some(None));

    // Not what `hdiutil info` prints: cut short, `images` not a list, our
    // entry without its entities or without a disk.
    let not_a_list = absent.replace("</dict>", "<key>images</key>\n<string>no</string>\n</dict>");
    let entityless = info_plist(&[(&ours.to_string_lossy(), &[])])
        .replace("<key>system-entities</key>\n<array>\n</array>\n", "");
    let diskless = info_plist(&[(&ours.to_string_lossy(), &[])]);
    for xml in [
        &listed[..listed.len() / 2],
        not_a_list.as_str(),
        entityless.as_str(),
        diskless.as_str(),
    ] {
        assert_eq!(parse_info_plist(xml, ours), None, "{xml}");
    }
}

// ------------------------------------------------------------- the swap

#[test]
fn quoting_keeps_every_byte_as_written() {
    assert_eq!(
        quote(b"/Applications/vocara.app"),
        b"'/Applications/vocara.app'"
    );
    assert_eq!(quote(b"/a b/c"), b"'/a b/c'");
    assert_eq!(quote(b"it's"), b"'it'\\''s'");
    assert_eq!(quote(b""), b"''");
    // What the shell reads back is the word, whatever it holds.
    for word in [
        "it's \"odd\" $HOME `id` & ; \\ \n !",
        "''''",
        "$(touch /tmp/x)",
    ] {
        let echoed = Command::new("/bin/sh")
            .arg("-c")
            .arg(OsString::from_vec(
                [b"printf %s ".as_slice(), &quote(word.as_bytes())].concat(),
            ))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&echoed.stdout), word);
    }
}

fn plan_for<'a>(paths: &'a [PathBuf; 7]) -> SwapPlan<'a> {
    SwapPlan {
        pid: 4242,
        staged_app: &paths[0],
        staging: &paths[1],
        target: &paths[2],
        log: &paths[3],
        failed_marker: &paths[4],
        opener: &paths[5],
        copier: &paths[6],
        wait_ticks: EXIT_WAIT_TICKS,
    }
}

/// The helper's text: its PATH pinned, every tool by absolute path, every
/// path single-quoted, the wait bounded, and no file it runs from.
#[test]
fn the_swap_script_pins_its_tools_and_quotes_every_path() {
    let paths = [
        PathBuf::from("/Users/me/Library/Application Support/com.rutatang.vocara/updates/stage-1-0/vocara.app"),
        PathBuf::from("/Users/me/Library/Application Support/com.rutatang.vocara/updates/stage-1-0"),
        PathBuf::from("/Applications/vocara.app"),
        PathBuf::from("/Users/me/Library/Application Support/com.rutatang.vocara/updates/update.log"),
        PathBuf::from("/Users/me/Library/Application Support/com.rutatang.vocara/updates/last-update-failed"),
        PathBuf::from(OPENER),
        PathBuf::from(COPIER),
    ];
    let script = swap_script(&plan_for(&paths)).into_string().unwrap();
    assert!(script.starts_with("set -u\nPATH=/usr/bin:/bin:/usr/sbin:/sbin\nexport PATH\n"));
    assert!(!script.starts_with("#!"), "an argument, never a file");
    for line in [
        "TARGET='/Applications/vocara.app'\n",
        "STAGING='/Users/me/Library/Application Support/com.rutatang.vocara/updates/stage-1-0'\n",
        "OPENER='/usr/bin/open'\n",
        "COPIER='/usr/bin/ditto'\n",
        "while kill -0 4242 2>/dev/null; do\n",
        &format!("if [ \"$i\" -ge {EXIT_WAIT_TICKS} ]; then\n"),
        "fail \"Vocara did not quit, so the update was not applied\"\n",
        "BACKUP=\"$TARGET.bak-$$\"\n",
        "if \"$COPIER\" \"$STAGED\" \"$TARGET\" 2>>\"$LOG\"; then\n",
        "fail \"could not copy the new version into place; the previous version was restored\"\n",
    ] {
        assert!(script.contains(line), "missing {line:?} in\n{script}");
    }
    // Every command that changes files is named by its absolute path.
    for bare in [" rm ", " mv ", " sleep ", "\nrm ", "\nmv "] {
        assert!(!script.contains(bare), "{bare:?} in\n{script}");
    }
    assert!(
        script.matches("/bin/rm -rf \"$STAGING\"").count() >= 4,
        "{script}"
    );
    // Three minutes, past the quit guard's longest wait.
    assert!(
        u64::from(EXIT_WAIT_TICKS) * 100 > crate::quit_request::REPEAT_WINDOW.as_millis() as u64,
    );
}

/// A path no UTF-8 can spell still reaches the script as its own bytes.
#[test]
fn the_swap_script_keeps_a_paths_bytes() {
    let odd = PathBuf::from(std::ffi::OsStr::from_bytes(b"/Applications/voc\xffara.app"));
    let paths = [
        PathBuf::from("/s/vocara.app"),
        PathBuf::from("/s"),
        odd,
        PathBuf::from("/l"),
        PathBuf::from("/f"),
        PathBuf::from(OPENER),
        PathBuf::from(COPIER),
    ];
    let script = swap_script(&plan_for(&paths));
    let bytes = script.as_bytes();
    let wanted = b"TARGET='/Applications/voc\xffara.app'\n";
    assert!(bytes.windows(wanted.len()).any(|w| w == wanted));
}

/// Writes an executable shell script.
fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A private fixture for running the real swap script: an installed app, a
/// staged update, and a recorder standing in for `open`.
pub(super) struct SwapFixture {
    _dir: tempfile::TempDir,
    pub(super) root: PathBuf,
    pub(super) target: PathBuf,
    pub(super) staging: PathBuf,
    pub(super) staged: PathBuf,
    opener: PathBuf,
    copier: PathBuf,
}

impl SwapFixture {
    pub(super) fn new(name: &str) -> SwapFixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(name);
        let target = root.join("Applications/vocara.app");
        std::fs::create_dir_all(target.join("Contents")).unwrap();
        std::fs::write(target.join("Contents/version"), "old").unwrap();
        let staging = root.join("updates/stage-1-0");
        let staged = staging.join("vocara.app");
        std::fs::create_dir_all(staged.join("Contents")).unwrap();
        std::fs::write(staged.join("Contents/version"), "new").unwrap();
        let opener = root.join("opener.sh");
        write_script(
            &opener,
            "printf '%s\\n' \"$@\" >> \"$(dirname \"$0\")/opened.txt\"",
        );
        // `ditto` where there is one; elsewhere a copy that behaves as it
        // does for a folder copied to a new name.
        let copier = if Path::new(COPIER).exists() {
            PathBuf::from(COPIER)
        } else {
            let copier = root.join("ditto.sh");
            write_script(&copier, "exec /bin/cp -R \"$1\" \"$2\"");
            copier
        };
        SwapFixture {
            _dir: dir,
            root,
            target,
            staging,
            staged,
            opener,
            copier,
        }
    }

    /// Runs the script to completion against a pid that has already exited
    /// (or `pid`, when given), returning bash's exit status.
    pub(super) fn run(&self, pid: Option<u32>, ticks: u32) -> std::process::ExitStatus {
        let pid = pid.unwrap_or_else(|| {
            let mut child = Command::new("/usr/bin/true").spawn().unwrap();
            let id = child.id();
            child.wait().unwrap();
            id
        });
        let plan = SwapPlan {
            pid,
            staged_app: &self.staged,
            staging: &self.staging,
            target: &self.target,
            log: &self.root.join("update.log"),
            failed_marker: &self.root.join("failed"),
            opener: &self.opener,
            copier: &self.copier,
            wait_ticks: ticks,
        };
        let script = swap_script(&plan);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Command::new("/bin/bash").arg("-c").arg(script).status());
        });
        // A hang detector, not a speed bound: macOS assesses the fixture's
        // fresh scripts the first time they run, which on a busy runner
        // takes seconds.
        rx.recv_timeout(std::time::Duration::from_secs(90))
            .expect("the swap script hung")
            .unwrap()
    }

    pub(super) fn version(&self) -> Option<String> {
        std::fs::read_to_string(self.target.join("Contents/version")).ok()
    }

    pub(super) fn opened(&self) -> String {
        std::fs::read_to_string(self.root.join("opened.txt")).unwrap_or_default()
    }

    pub(super) fn failure(&self) -> Option<String> {
        std::fs::read_to_string(self.root.join("failed")).ok()
    }

    pub(super) fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("update.log")).unwrap_or_default()
    }

    pub(super) fn backups(&self) -> usize {
        std::fs::read_dir(self.target.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".bak-"))
            .count()
    }
}

impl Drop for SwapFixture {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            self.target.parent().unwrap(),
            std::fs::Permissions::from_mode(0o755),
        );
    }
}

/// The happy path, executed for real: the new bundle replaces the old, the
/// backup and the staging directory are gone, and the NEW app is opened.
#[test]
fn the_swap_script_installs_the_staged_copy_and_relaunches_it() {
    let fx = SwapFixture::new("ok");
    assert!(fx.run(None, 50).success(), "{}", fx.log());
    assert_eq!(fx.version().as_deref(), Some("new"));
    assert!(!fx.staging.exists(), "the staging directory must be pruned");
    assert_eq!(fx.backups(), 0, "the backup must be removed");
    assert_eq!(fx.failure(), None);
    assert_eq!(fx.opened(), format!("{}\n", fx.target.display()));
    assert!(fx.log().contains("installed"), "{}", fx.log());
}

/// Every path reaches the script single-quoted: an install and a staging
/// directory whose names hold a single quote, a double quote, `$`,
/// backticks and spaces still swap, nothing in a name is expanded or run,
/// and the app is reopened by its exact name.
#[test]
fn paths_with_quotes_and_shell_characters_swap_as_named() {
    let fx = SwapFixture::new("it's \"odd\" $HOME `id` & ;");
    assert!(fx.run(None, 50).success(), "{}", fx.log());
    assert_eq!(fx.version().as_deref(), Some("new"));
    assert!(!fx.staging.exists());
    assert_eq!(fx.backups(), 0);
    assert_eq!(fx.failure(), None);
    assert_eq!(
        fx.opened(),
        format!("{}\n", fx.target.display()),
        "the opener must get the path as one argument, verbatim"
    );
}

/// The copy fails after the old app was moved aside: the old app must come
/// back, be RELAUNCHED (Vocara already quit for this), and the reason must
/// be left for it to report.
#[test]
fn a_failed_copy_restores_and_relaunches_the_previous_version() {
    let fx = SwapFixture::new("copy");
    std::fs::remove_dir_all(&fx.staged).unwrap(); // nothing to copy
    assert!(!fx.run(None, 50).success());
    assert_eq!(fx.version().as_deref(), Some("old"));
    assert_eq!(fx.backups(), 0);
    assert!(!fx.staging.exists());
    assert_eq!(fx.opened(), format!("{}\n", fx.target.display()));
    assert!(
        fx.failure()
            .is_some_and(|f| f.contains("previous version was restored")),
        "{:?}",
        fx.failure()
    );
    // The log says why: the copier's own complaint about the missing source.
    let log = fx.log();
    assert!(
        log.lines()
            .any(|l| l.contains(&*fx.staged.to_string_lossy()) && !l.contains("update failed")),
        "{log}"
    );
}

/// The copy fails halfway and leaves a half copy `rm` cannot fully remove
/// (a folder it may not write): the half is moved aside, not restored INTO
/// — `mv` onto a directory that still exists moves inside it, which used to
/// bury the previous version in the half copy and report it restored.
#[test]
fn a_half_copy_that_cannot_be_removed_is_moved_aside_before_the_restore() {
    use std::os::unix::fs::PermissionsExt;
    if root() {
        eprintln!("skipped: root removes what a read-only folder holds");
        return;
    }
    let mut fx = SwapFixture::new("half");
    let copier = fx.root.join("half-ditto.sh");
    write_script(
        &copier,
        "/bin/mkdir -p \"$2/Contents/locked\" && : > \"$2/Contents/locked/half\" \
         && /bin/chmod 555 \"$2/Contents/locked\"\nexit 1",
    );
    fx.copier = copier;
    assert!(!fx.run(None, 50).success());
    let apps = fx.target.parent().unwrap().to_path_buf();
    let halves: Vec<PathBuf> = std::fs::read_dir(&apps)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().contains(".incomplete-"))
        .collect();
    for half in &halves {
        let _ = std::fs::set_permissions(
            half.join("Contents/locked"),
            std::fs::Permissions::from_mode(0o755),
        );
    }
    assert_eq!(fx.version().as_deref(), Some("old"), "{}", fx.log());
    assert!(!fx.target.join("Contents/locked").exists());
    assert_eq!(fx.backups(), 0);
    assert_eq!(halves.len(), 1, "the half copy is set aside: {halves:?}");
    assert!(!fx.staging.exists());
    assert_eq!(fx.opened(), format!("{}\n", fx.target.display()));
    assert!(
        fx.failure()
            .is_some_and(|f| f.contains("previous version was restored")),
        "{:?}",
        fx.failure()
    );
    assert!(
        fx.log().contains("left the incomplete copy at"),
        "{}",
        fx.log()
    );
}

/// The installed app cannot even be moved aside: nothing changes, the old
/// app is relaunched and the reason is recorded.
#[test]
fn an_unmovable_install_is_left_alone_and_relaunched() {
    use std::os::unix::fs::PermissionsExt;
    if root() {
        eprintln!("skipped: root may move out of a read-only folder");
        return;
    }
    let fx = SwapFixture::new("mv");
    std::fs::set_permissions(
        fx.target.parent().unwrap(),
        std::fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    assert!(!fx.run(None, 50).success());
    assert_eq!(fx.version().as_deref(), Some("old"));
    assert!(!fx.staging.exists());
    assert_eq!(fx.opened(), format!("{}\n", fx.target.display()));
    assert!(fx
        .failure()
        .is_some_and(|f| f.contains("nothing was changed")));
    let log = fx.log();
    assert!(
        log.contains("mv") && log.contains("ermission denied"),
        "mv's own error reaches the log: {log}"
    );
}

/// A Vocara that never exits is never swapped out from under itself, and no
/// second copy is opened next to it.
#[test]
fn a_vocara_that_does_not_quit_is_not_swapped() {
    let fx = SwapFixture::new("alive");
    assert!(!fx.run(Some(std::process::id()), 3).success());
    assert_eq!(fx.version().as_deref(), Some("old"));
    assert_eq!(
        fx.opened(),
        "",
        "nothing may be opened next to a running Vocara"
    );
    assert!(!fx.staging.exists());
    assert!(fx.failure().is_some_and(|f| f.contains("did not quit")));
}
