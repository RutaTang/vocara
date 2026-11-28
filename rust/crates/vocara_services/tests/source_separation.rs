//! Port of `client/test/source_separation_test.dart`: the fake engine, and
//! the process engine driven by bash stubs standing in for the Python
//! sidecar (Unix only, as the Dart tests were).
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::source_separation_service::*;

#[test]
fn fake_produces_two_distinct_stem_files_and_reports_progress_to_one() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.wav");
    fs::write(&src, [7u8; 64]).unwrap();
    let progress = std::sync::Mutex::new(Vec::new());
    let cb = |v: f64| progress.lock().unwrap().push(v);
    let result = FakeSourceSeparationService::default()
        .separate(&src, Some(&cb))
        .unwrap();
    assert!(result.instrumental_path.exists());
    assert!(result.vocal_path.exists());
    assert_ne!(result.instrumental_path, result.vocal_path);
    // The fake copies the source verbatim into both stems.
    assert_eq!(fs::read(&result.instrumental_path).unwrap().len(), 64);
    let p = progress.lock().unwrap();
    assert!(!p.is_empty());
    assert_eq!(*p.last().unwrap(), 1.0);
}

#[test]
fn fake_is_available_reflects_the_configured_flag() {
    assert!(FakeSourceSeparationService::default().is_available());
    assert!(!FakeSourceSeparationService { available: false }.is_available());
}

#[test]
fn fake_still_yields_two_files_when_the_source_is_missing() {
    let r = FakeSourceSeparationService::default()
        .separate(Path::new("/no/such/file.wav"), None)
        .unwrap();
    assert!(r.instrumental_path.exists());
    assert!(r.vocal_path.exists());
}

#[test]
fn fake_cancel_is_a_safe_no_op() {
    FakeSourceSeparationService::default().cancel();
}

#[test]
fn the_null_engine_is_unavailable_and_refuses() {
    let svc = NullSourceSeparationService;
    assert!(!svc.is_available());
    assert!(matches!(
        svc.separate(Path::new("/x.wav"), None),
        Err(SeparationError::Unsupported(_))
    ));
}

/// A test root, its stub interpreter, and a service with a quit guard of its
/// own.
/// Waits until the freshly written stub at [path] can be executed. A test
/// thread that forks while another one writes a stub hands the child a copy
/// of the write descriptor until the child execs, and Linux refuses to run
/// a file open for writing (ETXTBSY), which the service's engine probe would
/// read as "no engine". Once one run succeeds, no such copy is left.
fn wait_until_executable(path: &Path) {
    const ETXTBSY: i32 = 26;
    for _ in 0..500 {
        match std::process::Command::new(path)
            .arg("-c")
            .stdin(std::process::Stdio::null())
            .status()
        {
            Err(e) if e.raw_os_error() == Some(ETXTBSY) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => return,
        }
    }
    panic!("{} stayed busy", path.display());
}

struct Rig {
    root: tempfile::TempDir,
    guard: Arc<SeparationQuitGuard>,
}

// Stub bodies: block until killed / write valid stems and succeed. `exec`
// replaces bash so the kill signal reaches sleep directly.
const SLEEP_BODY: &str = "exec sleep 300";
// Models a torch sidecar in a long compute call: SIGTERM is ignored (the
// disposition survives exec), only SIGKILL ends it. Records its PID so tests
// can watch the process itself, not just the run's verdict.
const STUBBORN_BODY: &str = "trap \"\" TERM\necho $$ >> \"$dir/pids\"\nexec sleep 300";
const SUCCESS_BODY: &str = r#"inst=""; voc=""
while [ $# -gt 0 ]; do
  case "$1" in
    --instrumental) inst="$2"; shift 2;;
    --vocal) voc="$2"; shift 2;;
    *) shift;;
  esac
done
head -c 100 /dev/zero > "$inst"
head -c 100 /dev/zero > "$voc"
exit 0"#;

impl Rig {
    fn new() -> Self {
        Rig {
            root: tempfile::tempdir().unwrap(),
            guard: Arc::new(SeparationQuitGuard::new()),
        }
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    /// Writes an executable interpreter stub. It answers the `-c` engine
    /// probe with success; a real (sidecar) invocation logs its argv to
    /// `argv` and then a line to `starts`, then runs [body]. In that order: a
    /// test that cancels a run as soon as `starts` counts it can kill the
    /// stub before its next line.
    fn write_stub(&self, body: &str) -> String {
        let f = self.root().join("stub.sh");
        fs::write(
            &f,
            format!(
                "#!/bin/bash\nif [ \"$1\" = \"-c\" ]; then exit 0; fi\ndir=\"$(dirname \"$0\")\"\n\
                 echo \"$@\" >> \"$dir/argv\"\necho start >> \"$dir/starts\"\n{body}\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
        wait_until_executable(&f);
        f.to_string_lossy().into_owned()
    }

    fn service(&self, stub: &str) -> Arc<ProcessSourceSeparationService> {
        Arc::new(ProcessSourceSeparationService::with_guard(
            Arc::new(LocalStorageService::with_root(self.root())),
            stub,
            Some("test-model"),
            self.guard.clone(),
        ))
    }

    fn write_source(&self) -> PathBuf {
        let f = self.root().join("song.wav");
        fs::write(&f, [3u8; 256]).unwrap();
        f
    }

    /// How many sidecar (non-probe) invocations the stub has served.
    fn starts(&self) -> usize {
        fs::read_to_string(self.root().join("starts"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    fn argv(&self) -> Vec<String> {
        fs::read_to_string(self.root().join("argv"))
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn tmp_files_in_cache(&self) -> Vec<PathBuf> {
        let Ok(rd) = fs::read_dir(self.root().join("stems_cache")) else {
            return vec![];
        };
        rd.flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().ends_with(".tmp"))
            .collect()
    }
}

fn wait_for(cond: impl Fn() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Runs a separation on its own thread (the app's blocking task), with the
/// run registered first on this thread as the app does.
fn spawn_separate(
    svc: &Arc<ProcessSourceSeparationService>,
    src: &Path,
) -> std::thread::JoinHandle<Result<SeparationResult, SeparationError>> {
    let ticket = svc.begin();
    let svc = svc.clone();
    let src = src.to_path_buf();
    std::thread::spawn(move || svc.separate_with(ticket, &src, None))
}

fn failure(rig: &Rig, body: &str) -> String {
    let svc = rig.service(&rig.write_stub(body));
    match svc.separate(&rig.write_source(), None) {
        Err(SeparationError::Failed(m)) => m,
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn a_successful_run_commits_stems_and_leaves_no_tmp_behind() {
    let rig = Rig::new();
    let svc = rig.service(&rig.write_stub(SUCCESS_BODY));
    let src = rig.write_source();
    let result = svc.separate(&src, None).unwrap();
    assert!(result
        .instrumental_path
        .to_string_lossy()
        .ends_with(".instrumental.wav"));
    assert!(result.vocal_path.to_string_lossy().ends_with(".vocal.wav"));
    assert!(fs::metadata(&result.instrumental_path).unwrap().len() > 44);
    assert!(fs::metadata(&result.vocal_path).unwrap().len() > 44);
    assert!(rig.tmp_files_in_cache().is_empty());
    // A repeat separate() of the same source is a cache hit: no new sidecar.
    let again = svc.separate(&src, None).unwrap();
    assert_eq!(again.instrumental_path, result.instrumental_path);
    assert_eq!(rig.starts(), 1);
}

#[test]
fn the_sidecar_keeps_its_model_checkpoint_in_app_support() {
    // audio-separator's default model dir is /tmp, which macOS clears at
    // reboot: every first separation after one re-downloaded the model.
    let rig = Rig::new();
    let svc = rig.service(&rig.write_stub(SUCCESS_BODY));
    svc.separate(&rig.write_source(), None).unwrap();
    let argv = rig.argv();
    assert_eq!(argv.len(), 1);
    let parts: Vec<&str> = argv[0].split(' ').collect();
    let i = parts
        .iter()
        .position(|a| *a == "--model-dir")
        .expect("--model-dir");
    let dir = rig.root().join("separation").join("models");
    assert_eq!(parts[i + 1], dir.to_string_lossy());
    assert!(dir.is_dir());
}

#[test]
fn a_second_separate_cancels_the_draining_old_run_and_the_new_run_stays_cancellable() {
    let rig = Rig::new();
    let svc = rig.service(&rig.write_stub(SLEEP_BODY));
    let src = rig.write_source();

    let sep_a = spawn_separate(&svc, &src);
    wait_for(|| rig.starts() >= 1, "run A sidecar start");

    // Run B enters while A's sidecar is alive.
    let sep_b = spawn_separate(&svc, &src);

    // A dies cleanly reporting CANCELLED.
    assert_eq!(sep_a.join().unwrap(), Err(SeparationError::Cancelled));

    // B starts only after A wound down, and it must still be registered.
    wait_for(|| rig.starts() >= 2, "run B sidecar start");
    svc.cancel();
    assert_eq!(sep_b.join().unwrap(), Err(SeparationError::Cancelled));

    // Each run passed its OWN nonce'd .tmp paths to the sidecar.
    let argv = rig.argv();
    assert_eq!(argv.len(), 2);
    let inst_arg = |line: &str| {
        let parts: Vec<String> = line.split(' ').map(str::to_string).collect();
        let i = parts.iter().position(|a| a == "--instrumental").unwrap();
        parts[i + 1].clone()
    };
    assert!(inst_arg(&argv[0]).ends_with(".tmp"));
    assert!(inst_arg(&argv[1]).ends_with(".tmp"));
    assert_ne!(inst_arg(&argv[0]), inst_arg(&argv[1]));
    assert!(rig.tmp_files_in_cache().is_empty());
}

// A model run the system kills (out of memory) leaves no traceback, only
// tqdm's \r-redrawn bar: its whole run was the "last line" shown as the
// cause, and the exit signal that explains it was dropped.
#[test]
fn a_sidecar_killed_by_a_signal_says_so_not_its_progress_bar() {
    let rig = Rig::new();
    let m = failure(
        &rig,
        "printf '  0%%|          | 0/38\\r  3%%|3         | 1/38\\r' >&2\nkill -9 $$",
    );
    assert!(m.contains("signal 9"), "{m}");
    assert!(!m.contains("%|"), "{m}");
}

#[test]
fn a_native_abort_keeps_the_reason_it_printed() {
    let rig = Rig::new();
    let m = failure(
        &rig,
        "printf 'failed assertion: MPS buffer too large\\n' >&2\nkill -6 $$",
    );
    assert_eq!(
        m,
        "Separation failed (signal 6): failed assertion: MPS buffer too large"
    );
}

#[test]
fn an_abort_printed_while_the_bar_was_on_screen_keeps_its_reason() {
    // tqdm redraws with \r and no line break, so the native assertion lands
    // on the bar's own line: dropping the whole line as a bar lost it.
    let rig = Rig::new();
    let m = failure(
        &rig,
        "printf ' 45%%|####5     | 17/38 [00:10<00:12,  1.70it/s]failed assertion: MPS buffer too large\\n' >&2\nkill -6 $$",
    );
    assert_eq!(
        m,
        "Separation failed (signal 6): failed assertion: MPS buffer too large"
    );
}

#[test]
fn a_killed_run_says_it_was_killed_whatever_the_library_last_logged() {
    let rig = Rig::new();
    let m = failure(
        &rig,
        "printf '2026-10-26 10:00:00,000 - INFO - separator - Starting separation process for audio_file_path: /x.wav\\n' >&2\nkill -9 $$",
    );
    assert!(m.contains("possibly for lack of memory"), "{m}");
    assert!(!m.contains("INFO"), "{m}");
}

#[test]
fn the_library_narrating_its_run_is_not_named_as_the_cause() {
    let rig = Rig::new();
    let m = failure(
        &rig,
        "printf 'Separating:  50%%|#    | 19/38 [00:01<00:01, 9.5it/s]\\n2026-10-26 10:00:00,000 - INFO - separator - Separation done\\n' >&2\nexit 3",
    );
    assert_eq!(m, "Separation failed (exit 3).");
}

#[test]
fn an_error_that_mentions_a_percentage_keeps_its_words() {
    let rig = Rig::new();
    let m = failure(
        &rig,
        "printf 'ValueError: shape mismatch at 100%%|x| boundary\\n' >&2\nexit 1",
    );
    assert_eq!(
        m,
        "Separation failed: ValueError: shape mismatch at 100%|x| boundary"
    );
}

#[test]
fn a_bar_cut_off_inside_its_brackets_is_no_cause() {
    let rig = Rig::new();
    let m = failure(
        &rig,
        "printf 'RuntimeError: model failed\\n 45%%|####5 | 17/38 [00:10<00:' >&2\nexit 2",
    );
    assert_eq!(m, "Separation failed: RuntimeError: model failed");
}

#[test]
fn an_abort_that_lands_inside_the_bars_brackets_keeps_its_words() {
    let rig = Rig::new();
    let m = failure(
        &rig,
        "printf ' 45%%|####5 | 17/38 [00:10<00:libc++abi: terminating\\n' >&2\nkill -6 $$",
    );
    assert!(m.contains("libc++abi: terminating"), "{m}");
}

#[test]
fn a_bar_cut_off_after_its_bar_or_mid_count_is_no_cause() {
    let rig = Rig::new();
    let m = failure(
        &rig,
        "printf 'RuntimeError: model failed\\n 45%%|####5 |\\n 46%%|####6 | 17/' >&2\nexit 2",
    );
    assert_eq!(m, "Separation failed: RuntimeError: model failed");
}

#[test]
fn a_bar_cut_off_inside_its_unit_body_or_count_is_no_cause() {
    let rig = Rig::new();
    let m = failure(
        &rig,
        "printf 'RuntimeError: model failed\\n 45%%|####5 | 17/38 [00:10<00:12, 1.7it\\nmodel.bin:  27%%|##7       | 12.3M/45.6M [00:02<00:05, 6.1MiB/\\n 45%%|####5\\n 46%%|####6 | 17' >&2\nexit 2",
    );
    assert_eq!(m, "Separation failed: RuntimeError: model failed");
}

#[test]
fn an_error_ending_in_percentages_keeps_its_words() {
    let rig = Rig::new();
    let m = failure(
        &rig,
        "printf 'ValueError: expected 100%%|got 50%%|\\n' >&2\nexit 1",
    );
    assert_eq!(m, "Separation failed: ValueError: expected 100%|got 50%|");
}

#[test]
fn an_error_ending_in_one_percentage_keeps_its_words() {
    let rig = Rig::new();
    let m = failure(&rig, "printf 'ValueError: expected 100%%|\\n' >&2\nexit 1");
    assert_eq!(m, "Separation failed: ValueError: expected 100%|");
}

#[test]
fn an_abort_right_after_the_count_keeps_its_first_letter() {
    let rig = Rig::new();
    let m = failure(
        &rig,
        "printf ' 46%%|####6 | 17Error: CUDA out of memory\\n' >&2\nexit 1",
    );
    assert!(m.contains("Error: CUDA out of memory"), "{m}");
}

#[test]
fn an_abort_that_lands_mid_count_keeps_only_its_words() {
    let rig = Rig::new();
    let m = failure(
        &rig,
        "printf ' 46%%|####6 | 17/libc++abi: terminating\\n' >&2\nkill -6 $$",
    );
    assert_eq!(m, "Separation failed (signal 6): libc++abi: terminating");
}

#[test]
fn a_failure_names_its_last_line_that_is_not_a_progress_bar() {
    let rig = Rig::new();
    let m = failure(
        &rig,
        "printf 'loading\\nValueError: bad model\\n 50%%|#    | 19/38\\r' >&2\nexit 1",
    );
    assert_eq!(m, "Separation failed: ValueError: bad model");
}

#[test]
fn cancel_right_after_entry_aborts_before_or_at_sidecar_start() {
    let rig = Rig::new();
    let svc = rig.service(&rig.write_stub(SLEEP_BODY));
    let src = rig.write_source();
    // The run is registered at entry, so a cancel landing during the
    // hashing/cache phase must stick to THIS run.
    let ticket = svc.begin();
    svc.cancel();
    assert_eq!(
        svc.separate_with(ticket, &src, None),
        Err(SeparationError::Cancelled)
    );
}

#[test]
fn a_cancel_at_about_100_percent_still_reports_cancelled_not_stems() {
    // Cancelling on the final progress tick lands the cancel after every one
    // of the commit's steps (size checks, renames, cache cap).
    let rig = Rig::new();
    let svc = rig.service(&rig.write_stub(SUCCESS_BODY));
    let src = rig.write_source();
    let s2 = svc.clone();
    let cb = move |v: f64| {
        if v >= 1.0 {
            s2.cancel();
        }
    };
    assert_eq!(
        svc.separate(&src, Some(&cb)),
        Err(SeparationError::Cancelled)
    );
    assert!(rig.tmp_files_in_cache().is_empty());
}

#[test]
fn the_quit_guard_kills_the_in_flight_sidecar() {
    let rig = Rig::new();
    let svc = rig.service(&rig.write_stub(SLEEP_BODY));
    let sep = spawn_separate(&svc, &rig.write_source());
    wait_for(|| rig.starts() >= 1, "sidecar start");
    // The quit paths call this; the run winds down as cancelled (SIGKILL, no
    // escalation wait) instead of orphaning the subprocess.
    rig.guard.kill_all();
    assert_eq!(sep.join().unwrap(), Err(SeparationError::Cancelled));
}

/// A run counts as running with its quit guard from its registration until
/// it has wound down (what an update's relaunch waits for), and a ticket
/// dropped unused lists nothing.
#[test]
fn the_quit_guard_lists_a_run_until_it_winds_down() {
    let rig = Rig::new();
    let stub = rig.write_stub(SLEEP_BODY);
    assert!(!rig.guard.has_active());
    drop(rig.service(&stub).begin());
    assert!(!rig.guard.has_active(), "a ticket dropped unused");
    let svc = rig.service(&stub);
    let sep = spawn_separate(&svc, &rig.write_source());
    assert!(rig.guard.has_active(), "registered");
    wait_for(|| rig.starts() >= 1, "sidecar start");
    assert!(rig.guard.has_active(), "separating");
    svc.cancel();
    assert_eq!(sep.join().unwrap(), Err(SeparationError::Cancelled));
    assert!(!rig.guard.has_active(), "wound down");
}

fn pid_alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn the_quit_sweep_reaches_a_superseded_runs_still_alive_sidecar() {
    // begin() hands the registration to run B BEFORE waiting run A out, so at
    // quit the registered run has no process yet while A's SIGTERM-ignoring
    // sidecar is still alive — protected only by a 3 s escalation timer that
    // dies with the exiting app. The kill sweep must reach EVERY live sidecar.
    let rig = Rig::new();
    let svc = rig.service(&rig.write_stub(STUBBORN_BODY));
    let src = rig.write_source();
    let pids = rig.root().join("pids");
    let sep_a = spawn_separate(&svc, &src);
    wait_for(
        || {
            fs::read_to_string(&pids)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
        },
        "run A sidecar start",
    );
    let pid_a: i32 = fs::read_to_string(&pids)
        .unwrap()
        .lines()
        .last()
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    // Run B supersedes A: it takes the registration and SIGTERMs A — which A
    // ignores — then parks awaiting A's wind-down.
    let sep_b = spawn_separate(&svc, &src);
    rig.guard.kill_all();

    // A must die from the sweep's own SIGKILL, well before the 3 s
    // escalation timer (which the real quit never lives to run) fires.
    let deadline = Instant::now() + Duration::from_secs(2);
    while pid_alive(pid_a) {
        if Instant::now() > deadline {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid_a.to_string()])
                .status();
            panic!("superseded sidecar survived the quit kill sweep");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(sep_a.join().unwrap(), Err(SeparationError::Cancelled));
    assert_eq!(sep_b.join().unwrap(), Err(SeparationError::Cancelled));
    assert_eq!(rig.starts(), 1, "no sidecar may be spawned while quitting");
}

#[test]
fn a_quit_during_the_hashing_phase_never_spawns_a_sidecar() {
    // At quit there is no process left to run the SIGTERM->SIGKILL
    // escalation, so a sidecar spawned AFTER the kill sweep could ignore a
    // lone SIGTERM and orphan. The pre-spawn check must abort the run.
    let rig = Rig::new();
    let svc = rig.service(&rig.write_stub(SLEEP_BODY));
    let src = rig.write_source();
    let ticket = svc.begin();
    rig.guard.kill_all();
    assert_eq!(
        svc.separate_with(ticket, &src, None),
        Err(SeparationError::Cancelled)
    );
    assert_eq!(rig.starts(), 0, "no sidecar may be spawned while quitting");
}

#[test]
fn construction_sweeps_stale_tmp_orphans_and_keeps_fresh_ones_and_wavs() {
    let rig = Rig::new();
    let cache = rig.root().join("stems_cache");
    fs::create_dir_all(&cache).unwrap();
    let stale = cache.join("aa.instrumental.wav.1.tmp");
    let fresh = cache.join("bb.vocal.wav.2.tmp");
    let wav = cache.join("cc.instrumental.wav");
    for f in [&stale, &fresh, &wav] {
        fs::write(f, [1u8; 10]).unwrap();
    }
    fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(2 * 60 * 60))
        .unwrap();

    let svc = rig.service("/no/such/python");
    svc.wait_stale_tmp_sweep();

    assert!(!stale.exists()); // orphan of a dead run: swept
    assert!(fresh.exists()); // could be a live run: kept
    assert!(wav.exists()); // committed stems are never swept
}
