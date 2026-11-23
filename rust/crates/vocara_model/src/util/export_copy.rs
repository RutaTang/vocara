//! Copying an export onto the path the user picked, and measuring the exports
//! in flight (port of `lib/utils/export_copy.dart`).
//!
//! Synchronous: [`export_copy`] blocks its (worker) thread for the copy, and
//! [`export_bytes_written`] measures every file on its own thread so one that
//! stopped answering costs the reading one wait, not one each. The registry
//! of exports in flight is process-wide, as Dart's top-level maps were;
//! [`ExportRegistry::new`] makes a private one (tests).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::util::dart_path as p;

/// How a file's length is read (a test seam; the default stats the file).
pub type LengthOf = Arc<dyn Fn(&str) -> io::Result<u64> + Send + Sync>;

#[derive(Default)]
struct Registry {
    /// The files the exports in flight are writing, each to the export
    /// writing it now.
    writing: HashMap<String, u64>,
    /// What each export in flight was last seen to have written.
    last_written: HashMap<String, u64>,
    next_export: u64,
}

/// The exports in flight (Dart's `_writing` / `_lastWritten`).
#[derive(Default)]
pub struct ExportRegistry {
    state: Mutex<Registry>,
}

/// The process-wide registry [`export_copy`] and [`export_bytes_written`]
/// use.
pub fn exports() -> &'static Arc<ExportRegistry> {
    static REGISTRY: OnceLock<Arc<ExportRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Arc::new(ExportRegistry::default()))
}

/// The default per-file answer time of [`export_bytes_written`].
pub const PER_FILE: Duration = Duration::from_millis(250);

/// How much the exports in flight have written so far, each file given
/// [PER_FILE] to answer (see [`ExportRegistry::bytes_written`]).
pub fn export_bytes_written() -> u64 {
    ExportRegistry::bytes_written(exports(), PER_FILE, None)
}

/// Copies [source] to [dest_path], as an export does (see
/// [`ExportRegistry::copy_with`]).
pub fn export_copy(source: &Path, dest_path: &str) -> io::Result<()> {
    exports().copy_with(dest_path, |dest| fs::copy(source, dest).map(|_| ()))
}

/// Dart's `File.exists`: anything but a directory, links followed.
fn file_exists(path: &str) -> bool {
    fs::metadata(path).is_ok_and(|m| !m.is_dir())
}

/// `File.lastModified` as Dart reads it: whole milliseconds.
fn modified_ms(meta: &fs::Metadata) -> io::Result<i128> {
    let t = meta.modified()?;
    Ok(match t.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_millis()) as i128,
        Err(e) => -(e.duration().as_millis() as i128),
    })
}

impl ExportRegistry {
    pub fn new() -> Arc<ExportRegistry> {
        Arc::new(ExportRegistry::default())
    }

    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// How much the exports in flight have written so far. A quit holding for
    /// an export reads a change here as the export still working, not wedged
    /// (see RecordingFinalizer): a long take copying to a slow stick outlasts
    /// the quit's budget, and cutting it off left the file there cut short.
    ///
    /// Each file gets [per_file] to answer, all of them at once: one on a
    /// network share that stopped answering counts at what it last had (no
    /// movement) rather than holding up the whole reading, and with it the
    /// pool imports' movement beside it. An answer that comes later still
    /// counts for the next reading: on a share slow to answer every time,
    /// each reading timed out and the export read as frozen while it copied.
    /// [length_of] replaces the file stat (tests).
    pub fn bytes_written(
        this: &Arc<ExportRegistry>,
        per_file: Duration,
        length_of: Option<LengthOf>,
    ) -> u64 {
        let measure: LengthOf =
            length_of.unwrap_or_else(|| Arc::new(|path: &str| fs::metadata(path).map(|m| m.len())));
        // Over a copy: an export ending (or starting) while a length is awaited
        // changes the map under the reading.
        let entries: Vec<(String, u64)> = this
            .lock()
            .writing
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        let deadline = Instant::now() + per_file;
        let pending: Vec<(String, mpsc::Receiver<io::Result<u64>>)> = entries
            .into_iter()
            .map(|(path, export)| {
                let (tx, rx) = mpsc::channel();
                let (registry, measure, at) =
                    (Arc::clone(this), Arc::clone(&measure), path.clone());
                thread::spawn(move || {
                    let result = measure(&at);
                    // Kept only while this same export is still writing: one that
                    // ended (or a later export to the same file) must not count its
                    // length. Never lower: a late answer can land after a newer one.
                    if let Ok(n) = result {
                        let mut s = registry.lock();
                        if s.writing.get(&at) == Some(&export)
                            && s.last_written.get(&at).is_none_or(|seen| n > *seen)
                        {
                            s.last_written.insert(at.clone(), n);
                        }
                    }
                    let _ = tx.send(result);
                });
                (path, rx)
            })
            .collect();
        let mut total = 0u64;
        for (path, rx) in pending {
            let wait = deadline.saturating_duration_since(Instant::now());
            total += match rx.recv_timeout(wait) {
                Ok(Ok(n)) => n,
                // Not created yet, or gone: nothing written there to count.
                Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => 0,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    this.lock().last_written.get(&path).copied().unwrap_or(0)
                }
            };
        }
        total
    }

    /// Copies onto [dest_path] with [copy] (given the file the copy must
    /// write), as an export does: should the copy fail part-way (a stick
    /// filling up), the half-written destination is removed rather than left,
    /// truncated, where the user will find it. A destination the copy never
    /// touched (it failed before writing) stays, even one picked to be
    /// replaced. Returns the copy's error.
    pub fn copy_with(
        &self,
        dest_path: &str,
        copy: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<()> {
        // Onto the file a link names, not the link: the copy writes through it, and
        // a failed copy's cleanup removed the link and left that file half-written.
        let dest = export_target(dest_path);
        // Unreadable: judged as absent, so only a file the copy then made goes.
        let before = if file_exists(&dest) {
            fs::metadata(&dest)
                .and_then(|m| Ok((modified_ms(&m)?, m.len())))
                .ok()
        } else {
            None
        };
        let export = {
            let mut s = self.lock();
            s.next_export += 1;
            let export = s.next_export;
            s.writing.insert(dest.clone(), export);
            s.last_written.remove(&dest);
            export
        };
        let result = copy(Path::new(&dest));
        if result.is_err() && file_exists(&dest) {
            // Judged on the size too: a rewrite within the clock's tick (a second
            // on some disks) reads the old time. Best effort: the copy's own
            // error is what the caller reports.
            let changed = match (before, fs::metadata(&dest)) {
                (None, _) => true,
                (Some((modified, size)), Ok(now)) => {
                    modified_ms(&now).ok() != Some(modified) || now.len() != size
                }
                (Some(_), Err(_)) => false,
            };
            if changed {
                let _ = fs::remove_file(&dest);
            }
        }
        // Only its own entry: a later export to the same file may own it now.
        let mut s = self.lock();
        if s.writing.get(&dest) == Some(&export) {
            s.writing.remove(&dest);
            s.last_written.remove(&dest);
        }
        result
    }

    /// What the export last seen writing [path] has written (tests).
    #[cfg(test)]
    fn last_written(&self, path: &str) -> Option<u64> {
        self.lock().last_written.get(path).copied()
    }
}

/// The file a copy onto [path] writes: [path], or the end of the links it
/// starts (one that names nothing yet included; at most 32 hops).
pub fn export_target(path: &str) -> String {
    let mut current = path.to_string();
    for _ in 0..32 {
        let is_link = fs::symlink_metadata(&current).is_ok_and(|m| m.file_type().is_symlink());
        if !is_link {
            break;
        }
        // Unreadable: the copy itself will say so.
        let Ok(target) = fs::read_link(&current) else {
            break;
        };
        let target = target.to_string_lossy().into_owned();
        current = if p::is_absolute(&target) {
            target
        } else {
            p::join(&p::dirname(&current), &target)
        };
    }
    current
}

/// Whether the file a copy onto [path] writes lies inside [root], links in
/// both followed. False when either cannot be resolved (the copy then reports
/// its own error).
pub fn exports_into(root: &str, path: &str) -> bool {
    let resolve = |p: &str| fs::canonicalize(p).map(|r| r.to_string_lossy().into_owned());
    let Ok(dir) = resolve(root) else {
        return false;
    };
    let target = export_target(path);
    let file = if file_exists(&target) {
        resolve(&target)
    } else {
        resolve(&p::dirname(&target)).map(|d| p::join(&d, &p::basename(&target)))
    };
    match file {
        Ok(file) => p::is_within(&dir, &file),
        Err(_) => false,
    }
}

#[cfg(all(test, unix))]
mod tests {
    //! Port of `client/test/export_copy_test.dart`. The fake sources become
    //! `copy_with` closures; the exports that hold mid-copy run on threads
    //! and are released through channels.
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

    use super::*;

    fn no_space() -> io::Error {
        io::Error::other("No space left on device")
    }

    /// A copy that writes [written] to the destination, then fails (a stick
    /// filling up); `None` writes nothing (it failed opening it).
    fn failing(written: Option<&'static str>) -> impl FnOnce(&Path) -> io::Result<()> {
        move |dest| {
            if let Some(w) = written {
                fs::write(dest, w)?;
            }
            Err(no_space())
        }
    }

    fn set_mtime(path: &Path, t: SystemTime) {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    fn hour_ago() -> SystemTime {
        SystemTime::now() - Duration::from_secs(3600)
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn a_copy_that_fails_part_way_leaves_no_truncated_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("take.wav");
        let reg = ExportRegistry::new();
        assert!(reg.copy_with(&s(&dest), failing(Some("RIFF...."))).is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn a_file_picked_to_be_replaced_goes_too_once_the_copy_rewrote_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("take.wav");
        fs::write(&dest, "old").unwrap();
        // Let the old file's time differ from the rewrite's.
        set_mtime(&dest, hour_ago());
        let reg = ExportRegistry::new();
        assert!(reg.copy_with(&s(&dest), failing(Some("half"))).is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn a_file_the_failing_copy_never_touched_stays() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("take.wav");
        fs::write(&dest, "old").unwrap();
        let reg = ExportRegistry::new();
        assert!(reg.copy_with(&s(&dest), failing(None)).is_err());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "old");
    }

    #[test]
    fn a_file_rewritten_within_the_clock_tick_still_goes() {
        // Judged on its time alone, a file written and then rewritten by the
        // failing copy inside one tick read as untouched, and stayed half-written.
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("take.wav");
        fs::write(&dest, "old").unwrap();
        let stamp = fs::metadata(&dest).unwrap().modified().unwrap();
        let reg = ExportRegistry::new();
        let result = reg.copy_with(&s(&dest), |d| {
            fs::write(d, "half")?;
            set_mtime(d, stamp);
            Err(no_space())
        });
        assert!(result.is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn through_a_link_the_half_written_file_goes_not_the_link() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("real.wav");
        fs::write(&target, "o").unwrap();
        set_mtime(&target, hour_ago());
        let link = tmp.path().join("link.wav");
        symlink(&target, &link).unwrap();
        let reg = ExportRegistry::new();
        assert!(reg.copy_with(&s(&link), failing(Some("half"))).is_err());
        assert!(!target.exists());
        assert!(fs::symlink_metadata(&link).is_ok());
    }

    #[test]
    fn a_link_that_names_nothing_yet_the_file_the_copy_made_goes() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("made.wav");
        let link = tmp.path().join("link.wav");
        symlink("made.wav", &link).unwrap(); // relative, dangling
        let reg = ExportRegistry::new();
        assert!(reg.copy_with(&s(&link), failing(Some("half"))).is_err());
        assert!(!target.exists());
    }

    #[test]
    fn a_chain_of_links_naming_nothing_yet_the_made_file_goes() {
        let tmp = tempfile::tempdir().unwrap();
        let made = tmp.path().join("final.wav");
        let first = tmp.path().join("link1.wav");
        symlink("final.wav", tmp.path().join("link2.wav")).unwrap();
        symlink("link2.wav", &first).unwrap();
        let reg = ExportRegistry::new();
        assert!(reg.copy_with(&s(&first), failing(Some("half"))).is_err());
        assert!(!made.exists());
        assert!(fs::symlink_metadata(tmp.path().join("link2.wav")).is_ok());
    }

    // The quit reads this to tell an export still copying from a wedged one.
    #[test]
    fn an_export_in_flight_reports_what_it_has_written_and_nothing_once_done() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("take.wav");
        let reg = ExportRegistry::new();
        let mut during = None;
        reg.copy_with(&s(&dest), |d| {
            fs::write(d, "audio")?;
            during = Some(ExportRegistry::bytes_written(&reg, PER_FILE, None));
            Ok(())
        })
        .unwrap();
        assert_eq!(during, Some(5));
        assert_eq!(ExportRegistry::bytes_written(&reg, PER_FILE, None), 0);
    }

    /// Starts an export to [name] that holds, once its first bytes are
    /// written, until the returned sender is used (or dropped); the handle
    /// joins it.
    fn hold_export(
        reg: &Arc<ExportRegistry>,
        dir: &Path,
        name: &str,
    ) -> (mpsc::Sender<()>, thread::JoinHandle<io::Result<()>>) {
        let (release, released) = mpsc::channel::<()>();
        let (written, wrote) = mpsc::channel::<()>();
        let reg = Arc::clone(reg);
        let dest = s(&dir.join(name));
        let handle = thread::spawn(move || {
            reg.copy_with(&dest, |d| {
                fs::write(d, "audio")?;
                written.send(()).unwrap();
                let _ = released.recv();
                Ok(())
            })
        });
        wrote.recv().unwrap();
        (release, handle)
    }

    /// A length that never answers (until the test is over).
    fn stuck() -> (LengthOf, mpsc::Sender<()>) {
        let (never, wait) = mpsc::channel::<()>();
        let wait = Mutex::new(wait);
        let f: LengthOf = Arc::new(move |_: &str| {
            let _ = wait.lock().unwrap().recv();
            Err(io::Error::other("ended"))
        });
        (f, never)
    }

    #[test]
    fn an_export_ending_while_the_reading_waits_on_a_length_leaves_the_reading_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = ExportRegistry::new();
        let (release, export) = hold_export(&reg, tmp.path(), "take.wav");
        let (answer_tx, answer_rx) = mpsc::channel::<u64>();
        let (asked_tx, asked_rx) = mpsc::channel::<()>();
        let answer_rx = Mutex::new(answer_rx);
        let asked_tx = Mutex::new(asked_tx);
        let reading = {
            let reg = Arc::clone(&reg);
            thread::spawn(move || {
                let f: LengthOf = Arc::new(move |_: &str| {
                    asked_tx.lock().unwrap().send(()).unwrap();
                    Ok(answer_rx.lock().unwrap().recv().unwrap())
                });
                ExportRegistry::bytes_written(&reg, Duration::from_secs(5), Some(f))
            })
        };
        asked_rx.recv().unwrap(); // waits on the file's length
        release.send(()).unwrap(); // the copy ends: its path leaves the set
        export.join().unwrap().unwrap();
        answer_tx.send(5).unwrap();
        assert_eq!(reading.join().unwrap(), 5);
    }

    // One export on a share that stopped answering held up the whole reading,
    // so the quit couldn't see a pool import still moving beside it.
    #[test]
    fn a_file_that_stops_answering_counts_at_what_it_last_had_promptly() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = ExportRegistry::new();
        let (release, export) = hold_export(&reg, tmp.path(), "take.wav");
        assert_eq!(ExportRegistry::bytes_written(&reg, PER_FILE, None), 5);

        let clock = Instant::now();
        let (f, _never) = stuck();
        let stuck_reading = ExportRegistry::bytes_written(&reg, Duration::from_millis(50), Some(f));
        assert_eq!(stuck_reading, 5);
        assert!(clock.elapsed() < Duration::from_secs(1));

        release.send(()).unwrap();
        export.join().unwrap().unwrap();
        assert_eq!(ExportRegistry::bytes_written(&reg, PER_FILE, None), 0);
    }

    #[test]
    fn a_share_slow_to_answer_every_time_still_reads_as_moving() {
        // Every stat outlasts the per-file wait. Late answers were dropped, so
        // each reading counted the last on-time one: frozen while it copied, and
        // the quit cut the export off. Deterministic: each stat answers only
        // once its reading has timed out, and the next reading starts once the
        // late answer is on record.
        let tmp = tempfile::tempdir().unwrap();
        let reg = ExportRegistry::new();
        let (release, export) = hold_export(&reg, tmp.path(), "take.wav");
        let dest = s(&tmp.path().join("take.wav"));
        let copied = Arc::new(AtomicU64::new(0));
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = Arc::new(Mutex::new(go_rx));
        let slow = {
            let (copied, go_rx) = (Arc::clone(&copied), Arc::clone(&go_rx));
            Arc::new(move |_: &str| {
                let n = copied.fetch_add(100, Ordering::SeqCst) + 100;
                go_rx.lock().unwrap().recv().unwrap();
                Ok(n)
            }) as LengthOf
        };
        let wait = Duration::from_millis(10);
        let first = ExportRegistry::bytes_written(&reg, wait, Some(Arc::clone(&slow)));
        go_tx.send(()).unwrap(); // the late answer (100) lands
        let deadline = Instant::now() + Duration::from_secs(10);
        while reg.last_written(&dest) != Some(100) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        let second = ExportRegistry::bytes_written(&reg, wait, Some(slow));
        go_tx.send(()).unwrap();
        assert_ne!(second, first);
        assert_eq!((first, second), (0, 100));
        release.send(()).unwrap();
        export.join().unwrap().unwrap();
    }

    #[test]
    fn an_answer_landing_after_its_export_ended_is_not_counted_for_a_later_export_to_the_same_file()
    {
        let tmp = tempfile::tempdir().unwrap();
        let reg = ExportRegistry::new();
        let (release, export) = hold_export(&reg, tmp.path(), "take.wav");
        let (answer_tx, answer_rx) = mpsc::channel::<u64>();
        let (asked_tx, asked_rx) = mpsc::channel::<()>();
        let (landed_tx, landed_rx) = mpsc::channel::<()>();
        let answer_rx = Mutex::new(answer_rx);
        let asked_tx = Mutex::new(asked_tx);
        let reading = {
            let reg = Arc::clone(&reg);
            thread::spawn(move || {
                let f: LengthOf = Arc::new(move |_: &str| {
                    asked_tx.lock().unwrap().send(()).unwrap();
                    Ok(answer_rx.lock().unwrap().recv().unwrap())
                });
                let n = ExportRegistry::bytes_written(&reg, Duration::from_secs(5), Some(f));
                landed_tx.send(()).unwrap();
                n
            })
        };
        asked_rx.recv().unwrap();
        release.send(()).unwrap();
        export.join().unwrap().unwrap(); // ended while its length was awaited
        answer_tx.send(11).unwrap();
        landed_rx.recv().unwrap();
        reading.join().unwrap();

        let (next, again) = hold_export(&reg, tmp.path(), "take.wav");
        let (f, _never) = stuck();
        assert_eq!(
            ExportRegistry::bytes_written(&reg, Duration::from_millis(20), Some(f)),
            0,
            "nothing on record for this export yet"
        );
        next.send(()).unwrap();
        again.join().unwrap().unwrap();
    }

    #[test]
    fn several_stuck_files_cost_one_wait_not_one_each() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = ExportRegistry::new();
        let held: Vec<_> = ["a.wav", "b.wav", "c.wav", "d.wav"]
            .iter()
            .map(|name| hold_export(&reg, tmp.path(), name))
            .collect();
        let clock = Instant::now();
        let (f, _never) = stuck();
        ExportRegistry::bytes_written(&reg, Duration::from_millis(200), Some(f));
        // One shared wait (~200 ms), never four in a row (≥ 800 ms).
        assert!(clock.elapsed() < Duration::from_millis(600));
        for (release, export) in held {
            release.send(()).unwrap();
            export.join().unwrap().unwrap();
        }
    }

    #[test]
    fn a_good_copy_lands_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.wav");
        fs::write(&src, "audio").unwrap();
        let dest = tmp.path().join("out.wav");
        let reg = ExportRegistry::new();
        reg.copy_with(&s(&dest), |d| fs::copy(&src, d).map(|_| ()))
            .unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "audio");
        export_copy(&src, &s(&tmp.path().join("out2.wav"))).unwrap();
        assert_eq!(
            fs::read_to_string(tmp.path().join("out2.wav")).unwrap(),
            "audio"
        );
    }

    #[test]
    fn exports_into_follows_links_on_both_sides() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pool");
        fs::create_dir(&root).unwrap();
        let outside = tmp.path().join("desk");
        fs::create_dir(&outside).unwrap();
        assert!(exports_into(&s(&root), &s(&root.join("new.wav"))));
        assert!(!exports_into(&s(&root), &s(&outside.join("new.wav"))));
        let link = outside.join("sneaky.wav");
        symlink(root.join("x.wav"), &link).unwrap();
        assert!(exports_into(&s(&root), &s(&link)));
        assert!(!exports_into(&s(&tmp.path().join("missing")), &s(&link)));
        assert_eq!(export_target(&s(&link)), s(&root.join("x.wav")));
    }
}
