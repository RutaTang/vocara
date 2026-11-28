//! The app's diagnostics log (port of `lib/data/services/diagnostics_log.dart`).
//!
//! A small rotating plain-text log under the app support directory, fed by
//! the safety-critical catch sites (boot recovery, sweeps, sync repairs) and
//! by the global panic hook.
//!
//! Design constraints, in order:
//!  * NEVER makes anything worse — every write is best-effort and swallowed;
//!    a diagnostics failure must not take down the code path it observes.
//!  * NO user content — callers log counts, error types and area tags, never
//!    lyrics, titles, paths into user media, or audio. Because the panic hook
//!    records raw messages (I/O errors carry full paths), [`DiagnosticsLog::log`]
//!    additionally scrubs absolute filesystem paths from every entry — the
//!    promise is enforced here, not just asked of callers.
//!  * Bounded — two files of 512 KiB each (`diagnostics.log` +
//!    `diagnostics.log.1`), rotated by size.
//!
//! The file format, the rotation and the cross-process write lock are the
//! Flutter app's, so both apps can share one log: the lock is a POSIX
//! `fcntl` record lock (`F_SETLK`, exclusive, whole file) on
//! `diagnostics.log.lock`, exactly what Dart's `RandomAccessFile.lock` takes.
//!
//! Writes are queued, in order, to a writer thread ([`DiagnosticsLog::log`]
//! never blocks on I/O); [`DiagnosticsLog::flush`] waits for what was queued.
//!
//! [`DiagnosticsLog::export_archive`] bundles the log files plus app version
//! and the library's schema `user_version` into one text file at a temp path,
//! for the user to attach to a bug report.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::app_support;
use crate::config::environment::AppEnvironment;
use crate::util::dart_date_time::DateTime;
use crate::util::dart_string::{self, truncate_utf16, utf16_len};

const DEFAULT_MAX_BYTES_PER_FILE: u64 = 512 * 1024;
const FILE_NAME: &str = "diagnostics.log";

/// Entries buffered in memory before a directory is bound (early boot).
const MAX_PENDING_ENTRIES: usize = 500;

/// One entry is capped so a pathological error string can't blow through
/// the rotation budget in a single write (UTF-16 code units, as Dart counts).
const MAX_ENTRY_CHARS: usize = 4096;

/// Errno values a non-blocking lock fails with while another process holds
/// it: EACCES, EAGAIN (11 on Linux, 35 on macOS), and EINTR (see
/// LibraryLock, which reads them the same way).
const HELD_ELSEWHERE: [i32; 4] = [13, 11, 35, 4];

/// How long a wait for the lock that ran out makes later entries impatient.
const IMPATIENCE: Duration = Duration::from_secs(2);

/// An open lock file.
pub trait LockFile: Send {
    /// Takes a non-blocking exclusive lock of the whole file (Dart's
    /// `lock(FileLock.exclusive)`); the OS error says why it could not.
    /// Dropping the file releases it.
    fn try_lock_exclusive(&mut self) -> io::Result<()>;
}

/// Opens the write lock's file (a test seam: a lock that refuses).
pub type OpenLock = Arc<dyn Fn(&Path) -> io::Result<Box<dyn LockFile>> + Send + Sync>;

/// Reads the schema `user_version` of the library database at a path, or
/// the type name of what went wrong (see
/// [`DiagnosticsLog::set_schema_version_reader`]).
pub type SchemaVersionReader = fn(&Path) -> Result<i64, String>;

/// Writes the bundle text to its path (a test seam: a disk that fills up).
pub type WriteBundle = Arc<dyn Fn(&Path, &str) -> io::Result<()> + Send + Sync>;

/// Waits between lock attempts (a test seam: counted, not slept).
pub type Sleep = Arc<dyn Fn(Duration) + Send + Sync>;

/// The real lock: an `fcntl` record lock, as Dart's `RandomAccessFile.lock`
/// takes on macOS and Linux (never `flock`, which does not exclude it).
struct FcntlLock(File);

impl LockFile for FcntlLock {
    #[cfg(unix)]
    fn try_lock_exclusive(&mut self) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        // SAFETY: `libc::flock` is plain old data; all-zero is a valid value
        // that the fields below then fill.
        let mut fl: libc::flock = unsafe { std::mem::zeroed() };
        fl.l_type = libc::F_WRLCK as _;
        fl.l_whence = libc::SEEK_SET as _;
        fl.l_start = 0;
        fl.l_len = 0; // to the end of the file, however it grows
                      // SAFETY: the descriptor belongs to the open `File` held by `self`,
                      // and `fl` is a live, initialized `flock` that `fcntl` only reads.
        let r = unsafe { libc::fcntl(self.0.as_raw_fd(), libc::F_SETLK, &fl) };
        if r == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[cfg(not(unix))]
    fn try_lock_exclusive(&mut self) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

fn open_lock_file(path: &Path) -> io::Result<Box<dyn LockFile>> {
    // Dart's FileMode.append: read and write, created when missing.
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)?;
    Ok(Box::new(FcntlLock(file)))
}

/// Dart's `RandomAccessFile.flush`: `fsync`.
fn fsync(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the descriptor belongs to `file`, open for the whole call.
        if unsafe { libc::fsync(file.as_raw_fd()) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        file.sync_all()
    }
}

/// How an instance is built (Dart's `DiagnosticsLog.forTesting` arguments,
/// plus the seams a deterministic test needs).
pub struct DiagnosticsOptions {
    /// The directory to log under, or `None` to bind later.
    pub directory: Option<PathBuf>,
    /// The rotation cap per file.
    pub max_bytes_per_file: u64,
    /// Opens the lock file (the real one takes an `fcntl` lock).
    pub open_lock: Option<OpenLock>,
    /// Waits between lock attempts (the real one sleeps).
    pub sleep: Option<Sleep>,
    /// Where bundle folders are made (the real one is the system temp
    /// directory).
    pub temp_root: Option<PathBuf>,
    /// Writes the bundle text (the real one writes and fsyncs).
    pub write_bundle: Option<WriteBundle>,
}

impl Default for DiagnosticsOptions {
    fn default() -> Self {
        DiagnosticsOptions {
            directory: None,
            max_bytes_per_file: DEFAULT_MAX_BYTES_PER_FILE,
            open_lock: None,
            sleep: None,
            temp_root: None,
            write_bundle: None,
        }
    }
}

struct State {
    dir: Option<PathBuf>,
    hooks_attached: bool,
    pending: Vec<String>,
    /// Set when the volume refused a lock outright: later writes skip it.
    lock_unsupported: bool,
    /// Until when a write tries the lock once instead of waiting for it.
    lock_impatient_until: Option<Instant>,
}

struct Queue {
    jobs: VecDeque<(PathBuf, String)>,
    enqueued: u64,
    completed: u64,
    worker_running: bool,
}

struct Inner {
    max_bytes_per_file: u64,
    open_lock: OpenLock,
    sleep: Sleep,
    temp_root: Option<PathBuf>,
    write_bundle: Option<WriteBundle>,
    state: Mutex<State>,
    queue: Mutex<Queue>,
    drained: Condvar,
}

/// The process-wide diagnostics log.
pub struct DiagnosticsLog {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

static SCHEMA_VERSION_READER: OnceLock<SchemaVersionReader> = OnceLock::new();

impl DiagnosticsLog {
    /// The single app-wide instance (all areas share one file, so ordering
    /// across subsystems is preserved).
    pub fn instance() -> &'static DiagnosticsLog {
        static INSTANCE: OnceLock<DiagnosticsLog> = OnceLock::new();
        INSTANCE.get_or_init(|| DiagnosticsLog::for_testing(DiagnosticsOptions::default()))
    }

    /// An isolated instance (tests): logs under `options.directory` (bind
    /// later via [`DiagnosticsLog::bind_directory`] when `None`).
    pub fn for_testing(options: DiagnosticsOptions) -> DiagnosticsLog {
        DiagnosticsLog {
            inner: Arc::new(Inner {
                max_bytes_per_file: options.max_bytes_per_file,
                open_lock: options
                    .open_lock
                    .unwrap_or_else(|| Arc::new(open_lock_file)),
                sleep: options.sleep.unwrap_or_else(|| Arc::new(thread::sleep)),
                temp_root: options.temp_root,
                write_bundle: options.write_bundle,
                state: Mutex::new(State {
                    dir: options.directory,
                    hooks_attached: false,
                    pending: Vec::new(),
                    lock_unsupported: false,
                    lock_impatient_until: None,
                }),
                queue: Mutex::new(Queue {
                    jobs: VecDeque::new(),
                    enqueued: 0,
                    completed: 0,
                    worker_running: false,
                }),
                drained: Condvar::new(),
            }),
        }
    }

    /// Lets the store supply the real schema reader (SafeMigrator's
    /// read-only `PRAGMA user_version`); the model crate has no SQLite. Until
    /// one is set, the bundle reads the `user_version` field of the database
    /// file's header directly (current unless a WAL holds a newer one).
    /// First call wins.
    pub fn set_schema_version_reader(reader: SchemaVersionReader) {
        let _ = SCHEMA_VERSION_READER.set(reader);
    }

    /// Binds the on-disk destination (`directory`, else the one already
    /// bound, else the environment's application support directory) and
    /// installs the global panic hook (logged, then forwarded to whatever
    /// hook was installed before), once. Call at the very top of `main()`;
    /// failures degrade to in-memory buffering and never panic.
    pub fn attach_global_hooks(&self, directory: Option<&Path>) {
        let bound = lock(&self.inner.state).dir.clone();
        let dir = match (directory, bound) {
            (Some(d), _) => Some(d.to_path_buf()),
            (None, Some(d)) => Some(d),
            (None, None) => application_support_directory(),
        };
        // No writable directory (headless test, broken platform): keep
        // buffering in memory; export_archive still includes the buffer.
        if let Some(dir) = dir {
            self.bind_directory(&dir);
        }
        {
            let mut s = lock(&self.inner.state);
            if s.hooks_attached {
                return;
            }
            s.hooks_attached = true;
        }
        // Uncaught panics, on any thread. Log first, then hand off to the
        // previous hook so the default reporting is unchanged.
        let previous = std::panic::take_hook();
        let log = DiagnosticsLog {
            inner: Arc::clone(&self.inner),
        };
        std::panic::set_hook(Box::new(move |info| {
            log.log("panic", &describe_panic(info));
            previous(info);
        }));
    }

    /// Binds (or rebinds) the directory the log files live under and flushes
    /// any entries buffered before boot got this far.
    pub fn bind_directory(&self, directory: &Path) {
        let mut s = lock(&self.inner.state);
        s.dir = Some(directory.to_path_buf());
        // Queued under the state lock, so an entry logged meanwhile lands
        // after the backlog.
        for entry in std::mem::take(&mut s.pending) {
            self.enqueue(directory.to_path_buf(), entry);
        }
    }

    /// Appends one timestamped entry under [area] (e.g. `boot`, `sync`,
    /// `quit`). Never fails and never blocks the caller on I/O: the write is
    /// queued (ordered) and any failure is swallowed. Absolute filesystem
    /// paths in [message] are scrubbed before anything is buffered or
    /// written.
    pub fn log(&self, area: &str, message: &str) {
        let mut entry = format!(
            "{} [{area}] {}",
            DateTime::now_utc().to_iso8601_string(),
            DiagnosticsLog::scrub_paths(message)
        );
        if utf16_len(&entry) > MAX_ENTRY_CHARS {
            entry = truncate_utf16(&entry, MAX_ENTRY_CHARS);
        }
        entry.push('\n');
        if cfg!(debug_assertions) {
            eprintln!("[diagnostics] {}", dart_string::trim_right(&entry));
        }
        let mut s = lock(&self.inner.state);
        match s.dir.clone() {
            None => {
                if s.pending.len() < MAX_PENDING_ENTRIES {
                    s.pending.push(entry);
                }
            }
            Some(dir) => self.enqueue(dir, entry),
        }
    }

    /// Waits until every entry queued so far has been written (or failed).
    pub fn flush(&self) {
        let mut q = lock(&self.inner.queue);
        let target = q.enqueued;
        while q.completed < target {
            q = self
                .inner
                .drained
                .wait(q)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    fn enqueue(&self, dir: PathBuf, entry: String) {
        let mut q = lock(&self.inner.queue);
        q.jobs.push_back((dir, entry));
        q.enqueued += 1;
        if q.worker_running {
            return;
        }
        q.worker_running = true;
        let inner = Arc::clone(&self.inner);
        let spawned = thread::Builder::new()
            .name("diagnostics-log".into())
            .spawn(move || write_queued(&inner));
        if spawned.is_err() {
            // No thread to write on: drop what is queued rather than fail (or
            // block) the caller, as a failed write is dropped.
            q.worker_running = false;
            q.completed += q.jobs.len() as u64;
            q.jobs.clear();
            self.inner.drained.notify_all();
        }
    }

    /// Writes a single plain-text diagnostics bundle to a temp path and returns
    /// that path: app version + library schema `user_version` + the rotated log
    /// files, oldest first. NO user content and NO audio ever goes in — the log
    /// itself only carries what [`DiagnosticsLog::log`] callers wrote (areas,
    /// counts, error types). Outside an app bundle the version reads
    /// "unknown"; see [`DiagnosticsLog::export_archive_with_version`].
    pub fn export_archive(&self) -> io::Result<PathBuf> {
        self.export_archive_with_version("unknown")
    }

    /// [`DiagnosticsLog::export_archive`], the version `built` (the version
    /// the binary was built as, which this crate cannot know) where no app
    /// bundle's Info.plist says one: a build run outside a bundle wrote
    /// "app version: unknown" while its About showed the version.
    pub fn export_archive_with_version(&self, built: &str) -> io::Result<PathBuf> {
        self.flush(); // the bundle reflects everything logged so far
        let (dir, pending) = {
            let s = lock(&self.inner.state);
            (s.dir.clone(), s.pending.clone())
        };
        let mut buffer = String::new();
        buffer.push_str("Vocara diagnostics bundle\n");
        buffer.push_str(&format!(
            "generated: {}\n",
            DateTime::now_utc().to_iso8601_string()
        ));
        buffer.push_str(&format!("app version: {}\n", app_version(built)));
        buffer.push_str(&format!(
            "schema user_version: {}\n",
            schema_user_version(dir.as_deref())
        ));
        buffer.push('\n');
        if let Some(dir) = &dir {
            // Oldest first: the rolled file precedes the current one.
            for name in [format!("{FILE_NAME}.1"), FILE_NAME.to_string()] {
                let f = dir.join(&name);
                if !dart_file_exists(&f) {
                    continue;
                }
                // Scrubbed again on the way out: lines an older build wrote stay on
                // disk until rotation, written with whatever its scrubber missed.
                let text = fs::read_to_string(&f)?;
                buffer.push_str(&format!("===== {name} =====\n"));
                buffer.push_str(&DiagnosticsLog::scrub_paths(&text));
                buffer.push('\n');
            }
        }
        if !pending.is_empty() {
            buffer.push_str("===== buffered (no log directory bound) =====\n");
            for entry in &pending {
                buffer.push_str(entry);
            }
        }
        let root = self
            .inner
            .temp_root
            .clone()
            .unwrap_or_else(std::env::temp_dir);
        let tmp = create_temp_dir(&root, "vocara-diagnostics-")?;
        let out = tmp.join("vocara-diagnostics.txt");
        let written = match &self.inner.write_bundle {
            Some(write) => write(&out, &buffer),
            None => write_synced(&out, &buffer),
        };
        if let Err(e) = written {
            // A write that fails (a full disk) hands the caller no path to discard:
            // the folder, and whatever part of the bundle landed, are ours to remove.
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
        Ok(out)
    }

    /// Removes a bundle [`DiagnosticsLog::export_archive`] wrote, once the
    /// caller has copied it where the user wanted it: the file, then its
    /// folder ONLY if that left it empty (a non-recursive delete, so a path
    /// from anywhere else can never take more than the file with it). Every
    /// export used to leave both behind. Best-effort; never fails.
    pub fn discard_archive(path: &Path) {
        let _ = fs::remove_file(path);
        if let Some(parent) = path.parent() {
            // Not empty, or already gone: the file was all that was ours.
            let _ = fs::remove_dir(parent);
        }
    }

    /// Replaces absolute filesystem paths with a `<path>` placeholder (the
    /// extension survives, it identifies the file kind without naming it).
    /// Anchored to user-space roots so `package:` URIs in stack frames — the
    /// most valuable diagnostic content — pass through untouched. Paths may
    /// contain spaces (song titles do), so a match runs greedily to the next
    /// quote or line end and is then cut back at the last extension in the
    /// path's final component; when in doubt this over-scrubs rather than leak.
    pub fn scrub_paths(message: &str) -> String {
        scrub::scrub_paths(message)
    }
}

/// The writer thread: drains the queue in order, then exits.
fn write_queued(inner: &Inner) {
    loop {
        let job = {
            let mut q = lock(&inner.queue);
            match q.jobs.pop_front() {
                Some(job) => job,
                None => {
                    q.worker_running = false;
                    return;
                }
            }
        };
        // Diagnostics must never become a failure source of its own.
        let _ = append(inner, &job.0, &job.1);
        let mut q = lock(&inner.queue);
        q.completed += 1;
        inner.drained.notify_all();
    }
}

/// A second running instance logs to the same file. Two appends at once
/// could write at the same offset (one entry overwriting the other), and two
/// rotations at once could roll the wrong file. An exclusive lock on a side
/// file (never renamed, unlike the log) takes them in turn; the lock is per
/// process, which is what is shared here, and the queue already orders this
/// process's own writes. Dropping the lock file releases it.
fn append(inner: &Inner, dir: &Path, entry: &str) -> io::Result<()> {
    let file = dir.join(FILE_NAME);
    let _lock = take_lock(inner, dir);
    rotate_if_needed(inner, &file, utf16_len(entry) as u64)?;
    // Flushed: an entry usually describes a failure, and the process may be
    // about to die; a buffered-and-lost diagnostic is worthless.
    let mut f = OpenOptions::new().append(true).create(true).open(&file)?;
    f.write_all(entry.as_bytes())?;
    fsync(&f)
}

/// The write lock, or `None` to write without it: a lock file that cannot be
/// opened, a volume that refuses locks, or another instance holding it for
/// over ~2 s (a process frozen mid-write, in a debugger say). Waiting for
/// good there stalled every later entry and an export behind it, and a
/// failure to lock dropped them all; unlocked, an entry is written as it
/// was before the lock existed.
///
/// Only a lock held elsewhere is waited for, and only once: a refusal of
/// any other kind means the volume cannot lock, and later entries no longer
/// try; after a wait that ran out, entries take one try each for the next
/// 2 s. Waited for in full every time, either cost 2 s an entry for good.
fn take_lock(inner: &Inner, dir: &Path) -> Option<Box<dyn LockFile>> {
    let patient = {
        let s = lock(&inner.state);
        if s.lock_unsupported {
            return None;
        }
        s.lock_impatient_until
            .is_none_or(|until| Instant::now() >= until)
    };
    let mut file = (inner.open_lock)(&dir.join(format!("{FILE_NAME}.lock"))).ok()?;
    let mut tries = 0;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Some(file),
            Err(e) => {
                let held = e
                    .raw_os_error()
                    .is_some_and(|c| HELD_ELSEWHERE.contains(&c));
                if !held {
                    lock(&inner.state).lock_unsupported = true;
                    return None;
                }
                if !patient || tries >= 40 {
                    lock(&inner.state).lock_impatient_until = Some(Instant::now() + IMPATIENCE);
                    return None;
                }
                (inner.sleep)(Duration::from_millis(50));
                tries += 1;
            }
        }
    }
}

/// Size-based rotation: when the incoming entry would push the current file
/// past the cap, the current file becomes `.1` (replacing the previous one)
/// and a fresh file starts — exactly two files ever exist.
///
/// The rename replaces `.1` in one step, and a file already rolled away,
/// when sized or when renamed, means the rotation is done: the entry still
/// goes into the fresh file instead of being dropped with the error. The
/// lock keeps a second running instance from rotating at the same time;
/// this is the fallback for a write that goes ahead without its lock.
fn rotate_if_needed(inner: &Inner, file: &Path, incoming_length: u64) -> io::Result<()> {
    let length = match fs::metadata(file) {
        Ok(m) => m.len(),
        // No file yet, or just rolled away: the entry starts a new one.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if length == 0 || length + incoming_length <= inner.max_bytes_per_file {
        return Ok(());
    }
    let mut rolled = file.as_os_str().to_owned();
    rolled.push(".1");
    match fs::rename(file, PathBuf::from(rolled)) {
        // Rotated by the other instance since the length was read.
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Dart's `File.exists`: anything but a directory, links followed.
fn dart_file_exists(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| !m.is_dir())
}

fn write_synced(path: &Path, text: &str) -> io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(text.as_bytes())?;
    fsync(&f)
}

/// `Directory.createTemp(prefix)` under [root]: a new folder with a random
/// suffix.
fn create_temp_dir(root: &Path, prefix: &str) -> io::Result<PathBuf> {
    use rand::Rng;
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::rng();
    for _ in 0..100 {
        let suffix: String = (0..6)
            .map(|_| CHARS[rng.random_range(0..CHARS.len())] as char)
            .collect();
        let dir = root.join(format!("{prefix}{suffix}"));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::from(io::ErrorKind::AlreadyExists))
}

/// The environment's application support directory
/// (`<data dir>/<bundle id>`, created when missing, as path_provider does),
/// or the App Sandbox container this launch rooted the app at
/// ([`app_support::rooted_at`]: a library a sandboxed release left there
/// that could not be moved out), which is never created again.
fn application_support_directory() -> Option<PathBuf> {
    let bundle_id = AppEnvironment::current().bundle_id();
    if let Some(rooted) = app_support::rooted_at(bundle_id) {
        return rooted.ok();
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    let base = if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support")
    } else {
        match std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => PathBuf::from(home).join(".local/share"),
        }
    };
    let dir = base.join(bundle_id);
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// What a panic says: its message and where, then the first frames of the
/// backtrace when one was captured (the entry cap bounds the rest).
fn describe_panic(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "Box<dyn Any>".to_string());
    let mut out = match info.location() {
        Some(at) => format!("{message} ({}:{}:{})", at.file(), at.line(), at.column()),
        None => message,
    };
    let trace = std::backtrace::Backtrace::capture();
    if trace.status() == std::backtrace::BacktraceStatus::Captured {
        let text = trace.to_string();
        let head: Vec<&str> = text.split('\n').take(8).collect();
        out.push('\n');
        out.push_str(&head.join("\n"));
    }
    out
}

/// The running bundle's version (CFBundleShortVersionString + build +
/// bundle id), read from the app bundle's own Info.plist; `built` outside a
/// bundled build (tests, `cargo run`).
fn app_version(built: &str) -> String {
    read_app_version().unwrap_or_else(|| built.to_string())
}

fn read_app_version() -> Option<String> {
    // <App>.app/Contents/MacOS/<exe> -> <App>.app/Contents/Info.plist
    let exe = std::env::current_exe().ok()?;
    let plist = exe.parent()?.parent()?.join("Info.plist");
    if !dart_file_exists(&plist) {
        return None;
    }
    // Built macOS Info.plists stay XML unless Xcode is told otherwise; fall
    // back to `defaults read` if the pattern ever stops matching.
    let xml = fs::read_to_string(&plist).ok()?;
    let read = |key: &str| plist_string(&xml, key);
    let mut version = read("CFBundleShortVersionString");
    if version.is_none() {
        let out = std::process::Command::new("/usr/bin/defaults")
            .arg("read")
            .arg(plist.with_extension(""))
            .arg("CFBundleShortVersionString")
            .output()
            .ok()?;
        if out.status.success() {
            version = Some(dart_string::trim(&String::from_utf8_lossy(&out.stdout)).to_string());
        }
    }
    let version = version.filter(|v| !v.is_empty())?;
    let build = read("CFBundleVersion").unwrap_or_default();
    let bundle_id = read("CFBundleIdentifier").unwrap_or_default();
    let mut parts = vec![version];
    if !build.is_empty() {
        parts.push(format!("({build})"));
    }
    if !bundle_id.is_empty() {
        parts.push(bundle_id);
    }
    Some(parts.join(" "))
}

/// The first `<key>KEY</key>\s*<string>([^<]*)</string>` in [xml].
fn plist_string(xml: &str, key: &str) -> Option<String> {
    let needle = format!("<key>{key}</key>");
    let mut from = 0;
    while let Some(at) = xml[from..].find(&needle) {
        let after = from + at + needle.len();
        let rest = xml[after..].trim_start_matches(scrub::is_js_space);
        if let Some(value) = rest.strip_prefix("<string>") {
            let end = value.find('<').unwrap_or(value.len());
            if value[end..].starts_with("</string>") {
                return Some(value[..end].to_string());
            }
        }
        from = from + at + 1;
    }
    None
}

/// The library database's schema `user_version`, read without opening the
/// store for writing (and without ever creating a file).
fn schema_user_version(dir: Option<&Path>) -> String {
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None => match application_support_directory() {
            Some(d) => d,
            None => return "unreadable (MissingPlatformDirectoryException)".to_string(),
        },
    };
    let db = dir.join("vocara.db");
    if !dart_file_exists(&db) {
        return "no database file".to_string();
    }
    let reader = SCHEMA_VERSION_READER
        .get()
        .copied()
        .unwrap_or(header_user_version);
    match reader(&db) {
        Ok(v) => v.to_string(),
        Err(kind) => format!("unreadable ({kind})"),
    }
}

/// The `user_version` field of an SQLite database header (offset 60, big
/// endian), refusing anything that is not one.
fn header_user_version(db: &Path) -> Result<i64, String> {
    let mut header = [0u8; 100];
    File::open(db)
        .and_then(|mut f| f.read_exact(&mut header))
        .map_err(|_| "SqliteException".to_string())?;
    if &header[..16] != b"SQLite format 3\0" {
        return Err("SqliteException".to_string());
    }
    Ok(i64::from(i32::from_be_bytes([
        header[60], header[61], header[62], header[63],
    ])))
}

/// The path scrubber, matching what Dart's regular expressions match:
///
/// - a path: `_roots(?:/(?:[^'"\n]|['"](?!\s*$|\s+\(OS Error|\n|[,;:)\]}]|
///   \.(?![A-Za-z0-9])))+)+`, with `_roots` =
///   `/(?:Users|private|var|tmp|Volumes|Library|Applications|home|etc|opt)`;
/// - where a second path starts inside it: `\s['"]?(?=_roots/)`;
/// - an extension: `\.[A-Za-z0-9]{1,8}(?=[\s.,;:)\]}'"]|$)`.
///
/// `\s` is JavaScript's white space; `$` is the end of the text.
mod scrub {
    const ROOTS: [&str; 10] = [
        "Users",
        "private",
        "var",
        "tmp",
        "Volumes",
        "Library",
        "Applications",
        "home",
        "etc",
        "opt",
    ];

    const KNOWN_EXTENSIONS: [&str; 36] = [
        "wav", "wave", "aif", "aiff", "aifc", "caf", "mp3", "m4a", "aac", "flac", "ogg", "oga",
        "opus", "mp4", "m4v", "mov", "webm", "mkv", "json", "db", "sqlite", "sqlite3", "ckpt",
        "pth", "onnx", "yaml", "txt", "log", "zip", "tmp", "part", "png", "jpg", "jpeg", "pdf",
        "py",
    ];

    /// JavaScript's `\s`.
    pub(super) fn is_js_space(c: char) -> bool {
        matches!(
            c,
            '\t' | '\n' | '\u{B}' | '\u{C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
                ..='\u{200A}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202F}'
                    | '\u{205F}'
                    | '\u{3000}'
                    | '\u{FEFF}'
        )
    }

    fn char_at(s: &str, i: usize) -> Option<char> {
        s.get(i..).and_then(|r| r.chars().next())
    }

    /// The end of `/<root>` at [i], if one starts there.
    fn root_at(s: &str, i: usize) -> Option<usize> {
        let rest = s.get(i..)?.strip_prefix('/')?;
        ROOTS
            .iter()
            .find(|r| rest.starts_with(*r))
            .map(|r| i + 1 + r.len())
    }

    /// `_roots/` at [i] (the lookahead of the next-root pattern).
    fn root_dir_at(s: &str, i: usize) -> bool {
        root_at(s, i).is_some_and(|end| s.as_bytes().get(end) == Some(&b'/'))
    }

    /// Whether the negative lookahead after a quote matches at [k]: the
    /// quote reads as a closing one.
    fn closes_quote(s: &str, k: usize) -> bool {
        let rest = &s[k..];
        // \s*$
        if rest.chars().all(is_js_space) {
            return true;
        }
        // \s+\(OS Error
        let trimmed = rest.trim_start_matches(is_js_space);
        if trimmed.len() < rest.len() && trimmed.starts_with("(OS Error") {
            return true;
        }
        let mut chars = rest.chars();
        match chars.next() {
            // \n and [,;:)\]}]
            Some('\n' | ',' | ';' | ':' | ')' | ']' | '}') => true,
            // \.(?![A-Za-z0-9])
            Some('.') => !chars.next().is_some_and(|c| c.is_ascii_alphanumeric()),
            _ => false,
        }
    }

    /// The end of the absolute path starting at [i], if one does.
    fn path_at(s: &str, i: usize) -> Option<usize> {
        let root_end = root_at(s, i)?;
        if s.as_bytes().get(root_end) != Some(&b'/') {
            return None;
        }
        // (?:/X+)+ with X = [^'"\n] | a quote that does not close: X takes
        // '/' too, so this is one greedy run of X after the first '/'.
        let mut j = root_end + 1;
        while let Some(c) = char_at(s, j) {
            let part = match c {
                '\n' => false,
                '\'' | '"' => !closes_quote(s, j + 1),
                _ => true,
            };
            if !part {
                break;
            }
            j += c.len_utf8();
        }
        (j > root_end + 1).then_some(j)
    }

    /// Where a second root starts a new word inside [matched]
    /// (`\s['"]?(?=_roots/)`), if anywhere.
    fn next_root(matched: &str) -> Option<usize> {
        for (p, c) in matched.char_indices() {
            if !is_js_space(c) {
                continue;
            }
            let q = p + c.len_utf8();
            if root_dir_at(matched, q) {
                return Some(p);
            }
            if matches!(matched.as_bytes().get(q), Some(b'\'' | b'"'))
                && root_dir_at(matched, q + 1)
            {
                return Some(p);
            }
        }
        None
    }

    /// Every extension-looking run in [s] from [from], as byte spans.
    fn extensions(s: &str, from: usize) -> Vec<(usize, usize)> {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut pos = from;
        while pos < b.len() {
            if b[pos] == b'.' {
                let run = b[pos + 1..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .count();
                let after = pos + 1 + run;
                let follows = match char_at(s, after) {
                    None => true,
                    Some(c) => {
                        is_js_space(c)
                            || matches!(c, '.' | ',' | ';' | ':' | ')' | ']' | '}' | '\'' | '"')
                    }
                };
                if (1..=8).contains(&run) && follows {
                    out.push((pos, after));
                    pos = after;
                    continue;
                }
            }
            pos += char_at(s, pos).map_or(1, char::len_utf8);
        }
        out
    }

    fn replace_path(matched: &str) -> String {
        // The path runs to where a second root starts a new word (prose naming
        // another path), else to the match end — and only its FINAL component
        // can carry the extension: a dot further up ('Vol.2 Live/…') belongs to
        // a directory name, and cutting there leaked the rest of the path.
        let end = next_root(matched).unwrap_or(matched.len());
        let last = matched[..end].rfind('/').map_or(0, |i| i + 1);
        // Within it, the LAST extension-looking run wins (a dotted file name,
        // 'Ms.Lauryn Hill.wav', must not leak its tail) — but a file type the
        // app actually handles wins over later runs that read as NUMBERS, so
        // 'take.wav after 12.3s' keeps its '.wav' and the context. Any other
        // later run is still part of the name ('Lesson.mov with Jane Doe.avi').
        let mut ext: Option<(usize, usize)> = None;
        let mut known: Option<(usize, usize)> = None;
        for (start, stop) in extensions(matched, last) {
            if start >= end {
                break;
            }
            let numeric = matched.as_bytes()[start + 1].is_ascii_digit();
            if known.is_some() && !numeric {
                known = None;
            }
            ext = Some((start, stop));
            if KNOWN_EXTENSIONS.contains(&matched[start + 1..stop].to_ascii_lowercase().as_str()) {
                known = Some((start, stop));
            }
        }
        // Text after the cut is prose, not path — but it may hold another path
        // of its own, so scrub it recursively (it is strictly shorter).
        match known.or(ext) {
            None => format!("<path>{}", scrub_paths(&matched[end..])),
            Some((start, stop)) => format!(
                "<path>{}{}",
                &matched[start..stop],
                scrub_paths(&matched[stop..])
            ),
        }
    }

    pub(super) fn scrub_paths(message: &str) -> String {
        let mut out = String::with_capacity(message.len());
        let mut copied = 0;
        let mut i = 0;
        while i < message.len() {
            if let Some(end) = path_at(message, i) {
                out.push_str(&message[copied..i]);
                out.push_str(&replace_path(&message[i..end]));
                copied = end;
                i = end;
                continue;
            }
            i += char_at(message, i).map_or(1, char::len_utf8);
        }
        out.push_str(&message[copied..]);
        out
    }
}
