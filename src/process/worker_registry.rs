//! On-disk registry of detached structured view worker processes.
//!
//! Each running structured view worker has a JSON file at
//! `<app_dir>/acp-workers/<session_id>.json` describing how to dial it
//! and who owns the process. The directory is the source of truth across
//! `aoe serve` restarts: when serve starts, it scans the directory, dials
//! every live worker, and only spawns a fresh worker for sessions that
//! have no registry entry (or a dead one).
//!
//! The worker process itself (the `aoe __acp-runner` shim) writes the
//! file on startup and removes it on graceful exit; `Supervisor::shutdown`
//! and the stale-sweep on serve startup remove it for crashed runners.
//!
//! File mode is 0600 because `provider_env_keys` and `socket_path` may
//! leak metadata about which agents/providers a user runs.
//!
//! Layout note: the runner *and* the daemon both write to entries
//! (runner: `pid`/`started_at` on boot; daemon:
//! `last_attached_at`/`detached_at` on attach/detach). A per-session
//! generation lock serializes record replacement, authenticated deletion,
//! and the initial termination signal so stale cleanup cannot remove or
//! signal a concurrently published replacement.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::util::now_secs;

#[cfg(all(test, unix))]
pub(crate) mod test_support {
    use std::io::BufRead as _;
    use std::os::unix::process::CommandExt as _;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};

    #[derive(Clone, Copy)]
    pub(crate) enum TermBehavior {
        Terminate,
        Ignore,
        Notify,
    }

    pub(crate) struct SocketPeer {
        child: Child,
        term_seen: Option<tokio::sync::oneshot::Receiver<()>>,
    }

    impl SocketPeer {
        pub(crate) fn spawn(socket: &Path, term_behavior: TermBehavior) -> Self {
            let behavior = match term_behavior {
                TermBehavior::Terminate => "terminate",
                TermBehavior::Ignore => "ignore",
                TermBehavior::Notify => "notify",
            };
            let mut child = Command::new(std::env::current_exe().expect("current test executable"))
                .args([
                    "--ignored",
                    "--exact",
                    "process::worker_registry::tests::authenticated_unix_listener_fixture",
                    "--nocapture",
                ])
                .env("AOE_TEST_RUNNER_SOCKET", socket)
                .env("AOE_TEST_RUNNER_TERM", behavior)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .expect("spawn authenticated runner fixture");
            let output = child.stdout.take().expect("runner fixture stdout");
            let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
            let (term_tx, term_rx) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let mut term_tx = Some(term_tx);
                for line in std::io::BufReader::new(output)
                    .lines()
                    .map_while(Result::ok)
                {
                    if line.contains("runner-listener-ready") {
                        let _ = ready_tx.send(());
                    } else if line.contains("runner-listener-term") {
                        if let Some(tx) = term_tx.take() {
                            let _ = tx.send(());
                        }
                    }
                }
            });
            if ready_rx.recv().is_err() {
                let _ = child.kill();
                let _ = child.wait();
                panic!("authenticated runner fixture exited before readiness");
            }
            Self {
                child,
                term_seen: matches!(term_behavior, TermBehavior::Notify).then_some(term_rx),
            }
        }

        pub(crate) fn pid(&self) -> u32 {
            self.child.id()
        }

        pub(crate) async fn wait_for_term(&mut self) {
            self.term_seen
                .take()
                .expect("fixture does not report SIGTERM")
                .await
                .expect("runner fixture exited before SIGTERM");
        }
    }

    impl Drop for SocketPeer {
        fn drop(&mut self) {
            if matches!(self.child.try_wait(), Ok(None)) {
                crate::process::worker::kill_process_group(self.child.id());
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }

    extern "C" fn report_term(_: nix::libc::c_int) {
        const MESSAGE: &[u8] = b"runner-listener-term\n";
        // SAFETY: `write` is async-signal-safe and the buffer is static.
        unsafe {
            nix::libc::write(
                nix::libc::STDOUT_FILENO,
                MESSAGE.as_ptr().cast(),
                MESSAGE.len(),
            );
        }
    }

    pub(super) fn run_listener_fixture() {
        use std::io::Write as _;
        use std::os::unix::net::UnixListener;

        let signal = match std::env::var("AOE_TEST_RUNNER_TERM").as_deref() {
            Ok("terminate") => None,
            Ok("ignore") => Some(nix::sys::signal::SigHandler::SigIgn),
            Ok("notify") => Some(nix::sys::signal::SigHandler::Handler(report_term)),
            other => panic!("invalid runner TERM behavior: {other:?}"),
        };
        if let Some(handler) = signal {
            let action = nix::sys::signal::SigAction::new(
                handler,
                nix::sys::signal::SaFlags::SA_RESTART,
                nix::sys::signal::SigSet::empty(),
            );
            // SAFETY: the fixture installs its TERM behavior before starting
            // threads or announcing readiness.
            unsafe { nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGTERM, &action) }
                .unwrap();
        }

        let socket = std::env::var_os("AOE_TEST_RUNNER_SOCKET").expect("runner socket path");
        let listener = UnixListener::bind(socket).expect("bind runner socket");
        println!("runner-listener-ready");
        std::io::stdout().flush().unwrap();
        loop {
            match listener.accept() {
                Ok((stream, _)) => drop(stream),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("accept runner socket: {error}"),
            }
        }
    }
}

// Generic worker-subprocess plumbing now lives in `process::worker`; the
// registry is the ACP consumer of it. Re-exported so the names referenced
// across the ACP code (and its tests) keep resolving here.
pub(crate) use crate::process::process_start_identity_for;
pub use crate::process::worker::{is_pid_alive, validate_id as validate_session_id};

/// Bump when the on-disk schema changes incompatibly. Older entries with
/// a smaller `runner_version` are swept on startup instead of dialed.
pub const RUNNER_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRecord {
    pub runner_version: u32,
    /// Binary build identity (`build_info::BUILD_VERSION`) of the
    /// `aoe __acp-runner` process that wrote this record, e.g.
    /// `"1.9.5+g7f31a9c42e01"`. Distinct from `runner_version`, which is
    /// the on-disk SCHEMA version. The daemon compares this against its
    /// own `BUILD_VERSION` to detect a worker left running on an older
    /// binary after `aoe update` and respawn it (see #1754). Defaulted on
    /// load for legacy records that pre-date this field; the empty string
    /// compares unequal to any current build, forcing a one-time respawn.
    #[serde(default)]
    pub build_version: String,
    pub session_id: String,
    /// PID of the `aoe __acp-runner` process. Used by the stale-sweep
    /// to decide whether the registry entry corresponds to a live owner.
    pub pid: u32,
    /// Native process start time captured by the runner that owns this
    /// record. Binds a reused PID to the original process generation.
    #[serde(default)]
    pub process_start_identity: Option<u64>,
    pub socket_path: PathBuf,
    /// Binary command name that the runner was invoked with
    /// (e.g. `"claude-agent-acp"`, `"codex-acp"`). Surfaced in
    /// `aoe acp ps`, logs, and the doctor's install-hint lookup.
    /// NOT the registry key; use `agent_key` to resolve a profile.
    pub agent_name: String,
    /// Registry key for the agent (e.g. `"claude"`, `"codex"`,
    /// `"opencode"`). Drives `acp::agent_profiles::resolve` and any
    /// other per-agent gate keyed on the registry name. Defaulted on
    /// load for legacy records that pre-date this field; the empty
    /// string falls back to `DEFAULT_AGENT_PROFILE` at the call site,
    /// which is the safest behavior for an unknown agent.
    #[serde(default)]
    pub agent_key: String,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub additional_dirs: Vec<PathBuf>,
    /// Keys (not values) of provider_env passed through at spawn. Lets
    /// the reconciler observe which provider auth was configured for the
    /// session without re-reading every entry on every tick.
    pub provider_env_keys: Vec<String>,
    /// Cached ACP session id assigned by the agent on first `session/new`.
    /// On reattach, the daemon sends `session/load <stored_acp_session_id>`
    /// to resume the agent-side transcript.
    pub stored_acp_session_id: Option<String>,
    /// Profile the session was created under. Persisted so reattach can
    /// re-resolve sandbox env (`terminal/create` env entries) against the
    /// same profile the session originally used, instead of silently
    /// falling back to the global default profile. Defaulted on load for
    /// legacy records that pre-date this field; an absent value falls
    /// back to the default profile, matching pre-persistence behavior.
    #[serde(default)]
    pub source_profile: Option<String>,
    pub started_at: u64,
    pub last_attached_at: Option<u64>,
    pub detached_at: Option<u64>,
}

impl WorkerRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: String,
        pid: u32,
        socket_path: PathBuf,
        agent_name: String,
        agent_key: String,
        cwd: PathBuf,
        model: Option<String>,
        additional_dirs: Vec<PathBuf>,
        provider_env_keys: Vec<String>,
        stored_acp_session_id: Option<String>,
        source_profile: Option<String>,
    ) -> Self {
        Self {
            runner_version: RUNNER_VERSION,
            build_version: crate::build_info::BUILD_VERSION.to_string(),
            session_id,
            pid,
            process_start_identity: process_start_identity_for(pid),
            socket_path,
            agent_name,
            agent_key,
            cwd,
            model,
            additional_dirs,
            provider_env_keys,
            stored_acp_session_id,
            source_profile,
            started_at: now_secs(),
            last_attached_at: None,
            detached_at: None,
        }
    }
}

/// Directory holding worker JSON files, log files, and the per-session
/// unix sockets. Auto-created on first access.
pub fn workers_dir() -> Result<PathBuf> {
    let dir = crate::session::get_app_dir()?.join("acp-workers");
    crate::process::worker::ensure_dir(&dir)?;
    Ok(dir)
}

/// `<workers_dir>/<session_id>.json`.
pub fn record_path(session_id: &str) -> Result<PathBuf> {
    crate::process::worker::record_path(&workers_dir()?, session_id)
}

/// Cross-process fence for one session's runner-generation record.
pub(crate) struct GenerationLock {
    file: File,
}

impl Drop for GenerationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub(crate) fn lock_generation(session_id: &str) -> Result<GenerationLock> {
    validate_session_id(session_id)?;
    let path = workers_dir()?.join(format!(".{session_id}.generation.lock"));
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening generation lock at {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    file.lock_exclusive()
        .with_context(|| format!("locking generation at {}", path.display()))?;
    Ok(GenerationLock { file })
}

/// `<workers_dir>/<session_id>.sock`. Caller computes this once and threads
/// the same path into both the runner spawn and the daemon connect.
pub fn socket_path_for(session_id: &str) -> Result<PathBuf> {
    crate::process::worker::socket_path(&workers_dir()?, session_id)
}

/// `<workers_dir>/<session_id>.log` is the runner-side stderr drain
/// consumed by `aoe acp logs --session <id>`.
pub fn log_path_for(session_id: &str) -> Result<PathBuf> {
    crate::process::worker::log_path(&workers_dir()?, session_id)
}

/// Sentinel file `<workers_dir>/<session_id>.restart`. Written by
/// `aoe acp restart` BEFORE the registry delete + SIGTERM so the
/// daemon's reaper can distinguish a restart-driven teardown from
/// `aoe acp stop|kill` and:
///   - emit `Stopped { reason: "restart_pending" }` instead of
///     `user_stopped` so the UI shows a "Restarting…" banner without
///     the "Reconnect" button (the daemon will respawn shortly);
///   - signal the reconciler to clear the `attempted` set for this id
///     so the next 2s tick actually spawns a fresh worker.
pub fn restart_marker_path(session_id: &str) -> Result<PathBuf> {
    crate::process::worker::restart_marker_path(&workers_dir()?, session_id)
}

/// Best-effort write of an empty restart-pending marker. Called by the
/// CLI's `aoe acp restart` before deleting the registry entry. The
/// file's existence is the signal; its contents are irrelevant.
pub fn mark_restart_pending(session_id: &str) {
    let Ok(path) = restart_marker_path(session_id) else {
        return;
    };
    let _ = std::fs::write(&path, b"");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
}

/// Returns `true` if the marker existed (and was deleted). Caller uses
/// the boolean to pick the publish reason; defense-in-depth removes the
/// file so a leaked marker doesn't poison the next spawn.
pub fn take_restart_marker(session_id: &str) -> bool {
    let Ok(path) = restart_marker_path(session_id) else {
        return false;
    };
    match std::fs::remove_file(&path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => false,
    }
}

/// Atomic write (temp + rename) with 0600 perms. Avoids the half-written
/// JSON that a naive `fs::write` would leave if the runner is killed
/// mid-write — the dial path would then fail to parse and the entry
/// would be swept.
pub fn save(record: &WorkerRecord) -> Result<()> {
    let _generation_lock = lock_generation(&record.session_id)?;
    save_unlocked(record)
}

fn save_unlocked(record: &WorkerRecord) -> Result<()> {
    let dir = workers_dir()?;
    let final_path = dir.join(format!("{}.json", record.session_id));
    let tmp_path = dir.join(format!("{}.json.tmp", record.session_id));
    let bytes = serde_json::to_vec_pretty(record).context("serializing worker record")?;
    std::fs::write(&tmp_path, &bytes)
        .with_context(|| format!("writing tmp record at {}", tmp_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("renaming tmp record to {}", final_path.display()))?;
    Ok(())
}

/// Delete only the exact generation still named by the current record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GenerationDeleteOutcome {
    Deleted,
    Missing,
    Changed,
}

fn same_generation(current: &WorkerRecord, expected: &WorkerRecord) -> bool {
    current.session_id == expected.session_id
        && current.pid == expected.pid
        && current.process_start_identity == expected.process_start_identity
        && current.socket_path == expected.socket_path
        && current.started_at == expected.started_at
}

pub(crate) fn delete_generation_if_matches(
    session_id: &str,
    pid: u32,
    socket_path: &Path,
    started_at: Option<u64>,
    process_start_identity: Option<u64>,
) -> Result<GenerationDeleteOutcome> {
    let _generation_lock = lock_generation(session_id)?;
    let Some(current) = load_existing_strict(session_id)? else {
        return Ok(GenerationDeleteOutcome::Missing);
    };
    if current.session_id != session_id
        || current.pid != pid
        || current.process_start_identity != process_start_identity
        || current.socket_path != socket_path
        || started_at.is_some_and(|started| current.started_at != started)
    {
        return Ok(GenerationDeleteOutcome::Changed);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(current_identity) = process_start_identity_for(pid) {
        if Some(current_identity) != process_start_identity {
            return Ok(GenerationDeleteOutcome::Changed);
        }
    }
    delete_unlocked(session_id)?;
    Ok(GenerationDeleteOutcome::Deleted)
}

pub(crate) fn delete_record_if_current(
    record: &WorkerRecord,
    process_start_identity: Option<u64>,
) -> Result<GenerationDeleteOutcome> {
    delete_generation_if_matches(
        &record.session_id,
        record.pid,
        &record.socket_path,
        Some(record.started_at),
        process_start_identity,
    )
}

/// Delete an exact persisted generation only after proving its process is
/// absent. Unlike `delete_record_if_current`, this intentionally compares the
/// record's persisted process-start identity instead of a fresh `/proc` value:
/// a dead process has no current value to pass to that helper.
pub(crate) fn delete_dead_record_if_current(
    record: &WorkerRecord,
) -> Result<GenerationDeleteOutcome> {
    let _generation_lock = lock_generation(&record.session_id)?;
    let Some(current) = load_existing_strict(&record.session_id)? else {
        return Ok(GenerationDeleteOutcome::Missing);
    };
    if !same_generation(&current, record) {
        return Ok(GenerationDeleteOutcome::Changed);
    }
    if is_pid_alive(record.pid) || process_start_identity_for(record.pid).is_some() {
        return Ok(GenerationDeleteOutcome::Changed);
    }
    delete_unlocked(&record.session_id)?;
    Ok(GenerationDeleteOutcome::Deleted)
}

pub fn load(session_id: &str) -> Result<Option<WorkerRecord>> {
    let path = record_path(session_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    match serde_json::from_slice::<WorkerRecord>(&bytes) {
        Ok(record) => Ok(Some(record)),
        Err(e) => {
            warn!(
                target: "acp.registry",
                path = %path.display(),
                "failed to parse worker record: {e}; treating as missing"
            );
            Ok(None)
        }
    }
}

/// Load a generation record without converting malformed JSON into absence.
/// Authority-sensitive spawn and teardown paths must fail closed when an
/// existing record cannot be authenticated.
pub(crate) fn load_existing_strict(session_id: &str) -> Result<Option<WorkerRecord>> {
    let path = record_path(session_id)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let record = serde_json::from_slice::<WorkerRecord>(&bytes)
        .with_context(|| format!("parsing worker record at {}", path.display()))?;
    Ok(Some(record))
}

/// Re-read and authenticate a runner record under its cross-process fence.
/// Legacy live records without a persisted Linux start identity are upgraded
/// only when the canonical socket proves the same peer PID.
pub(crate) fn authenticate_generation_record(
    record: &WorkerRecord,
    session_id: &str,
    socket_path: &Path,
    connected_peer_pid: Option<u32>,
) -> Result<Option<WorkerRecord>> {
    let _generation_lock = lock_generation(session_id)?;
    let Some(mut current) = load_existing_strict(session_id)? else {
        return Ok(None);
    };
    if current.session_id != session_id
        || current.pid != record.pid
        || current.socket_path != socket_path
        || current.started_at != record.started_at
        || current.process_start_identity != record.process_start_identity
    {
        return Ok(None);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if connected_peer_pid.or_else(|| crate::process::worker::peer_pid_from_socket(socket_path))
            != Some(current.pid)
        {
            return Ok(None);
        }
        if current.process_start_identity.is_none() {
            let Some(identity) = process_start_identity_for(current.pid) else {
                return Ok(None);
            };
            current.process_start_identity = Some(identity);
            save_unlocked(&current)?;
        }
        if current.process_start_identity != process_start_identity_for(current.pid) {
            return Ok(None);
        }
    }
    Ok(Some(current))
}

/// Bind a just-spawned runner to the record it published after the daemon
/// captured its PID. This is the only path that may acquire `started_at`
/// rather than requiring it up front; every other generation operation uses
/// the fully persisted token returned here.
pub(crate) fn authenticate_spawned_generation(
    session_id: &str,
    pid: u32,
    socket_path: &Path,
    process_start_identity: Option<u64>,
    peer_pid: Option<u32>,
) -> Result<Option<WorkerRecord>> {
    let _generation_lock = lock_generation(session_id)?;
    let Some(current) = load_existing_strict(session_id)? else {
        return Ok(None);
    };
    if current.session_id != session_id
        || current.pid != pid
        || current.socket_path != socket_path
        || peer_pid != Some(current.pid)
        || (process_start_identity.is_some()
            && current.process_start_identity != process_start_identity)
    {
        return Ok(None);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if current.process_start_identity.is_none()
        || current.process_start_identity != process_start_identity_for(current.pid)
    {
        return Ok(None);
    }
    Ok(Some(current))
}

pub fn list() -> Result<Vec<WorkerRecord>> {
    let dir = workers_dir()?;
    let mut out = Vec::new();
    let read = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        match serde_json::from_slice::<WorkerRecord>(&bytes) {
            Ok(rec) => out.push(rec),
            Err(e) => {
                warn!(
                    target: "acp.registry",
                    path = %path.display(),
                    "skipping unparseable worker record: {e}"
                );
            }
        }
    }
    Ok(out)
}

/// Enumerate every persisted worker record without treating malformed state
/// as absence. Authority-sensitive bulk teardown must fail closed rather than
/// silently leave an undiscovered runner eligible for replacement.
pub(crate) fn list_existing_strict() -> Result<Vec<WorkerRecord>> {
    let dir = workers_dir()?;
    let read = match std::fs::read_dir(&dir) {
        Ok(read) => read,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in read {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        out.push(
            serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing worker record at {}", path.display()))?,
        );
    }
    Ok(out)
}

/// Remove the JSON entry and the unix socket file (if present). A
/// non-empty `.log` file is intentionally left behind so the user can read
/// it after the worker exits; an empty (0-byte) log carries no post-mortem
/// value and is swept so a crash loop doesn't litter the workers dir with
/// dead empty logs. See #1945.
pub fn delete(session_id: &str) -> Result<()> {
    let _generation_lock = lock_generation(session_id)?;
    delete_unlocked(session_id)
}

fn delete_unlocked(session_id: &str) -> Result<()> {
    if let Ok(p) = record_path(session_id) {
        let _ = std::fs::remove_file(&p);
    }
    if let Ok(p) = socket_path_for(session_id) {
        let _ = std::fs::remove_file(&p);
        // Sibling control socket (Phase A of #1054); best-effort, absent
        // for runners that predate the control channel.
        let _ = std::fs::remove_file(crate::process::worker::control_socket_sibling(&p));
    }
    if let Ok(p) = log_path_for(session_id) {
        if matches!(std::fs::metadata(&p), Ok(m) if m.len() == 0) {
            let _ = std::fs::remove_file(&p);
        }
    }
    Ok(())
}

/// Update the `last_attached_at` field in place. Best-effort: any I/O
/// error is logged and swallowed because attach itself has already
/// succeeded; the timestamp is purely for observability.
fn update_generation_if_current(
    expected: &WorkerRecord,
    update: impl FnOnce(&mut WorkerRecord),
) -> Result<bool> {
    let _generation_lock = lock_generation(&expected.session_id)?;
    let Some(mut current) = load_existing_strict(&expected.session_id)? else {
        return Ok(false);
    };
    if !same_generation(&current, expected) {
        return Ok(false);
    }
    update(&mut current);
    save_unlocked(&current)?;
    Ok(true)
}

pub fn mark_attached(record: &WorkerRecord) {
    if let Err(e) = update_generation_if_current(record, |current| {
        current.last_attached_at = Some(now_secs());
        current.detached_at = None;
    }) {
        debug!(
            target: "acp.registry",
            session = %record.session_id,
            "failed to update last_attached_at: {e}"
        );
    }
}

pub fn mark_detached(record: &WorkerRecord) {
    if let Err(e) = update_generation_if_current(record, |current| {
        current.detached_at = Some(now_secs());
    }) {
        debug!(
            target: "acp.registry",
            session = %record.session_id,
            "failed to update detached_at: {e}"
        );
    }
}

/// Update only `stored_acp_session_id` in place. Called by the
/// supervisor when the drain task observes an `AcpSessionAssigned`
/// event, so a fresh `aoe serve` knows to call `session/load` instead
/// of `session/new` on reattach.
pub fn update_stored_acp_session_id(record: &WorkerRecord, acp_id: Option<&str>) {
    match update_generation_if_current(record, |current| {
        current.stored_acp_session_id = acp_id.filter(|s| !s.is_empty()).map(|s| s.to_string());
    }) {
        Ok(true) => {}
        Ok(false) => debug!(
            target: "acp.registry",
            session = %record.session_id,
            "skipped stored_acp_session_id update because the runner generation changed"
        ),
        Err(e) => debug!(
            target: "acp.registry",
            session = %record.session_id,
            "failed to update stored_acp_session_id: {e}"
        ),
    }
}

/// Probe the recorded socket path. A worker registry entry is "live"
/// only if both the PID is alive AND the socket file still exists; a
/// stale entry where the runner died before deleting its files would
/// otherwise let attach hang on a missing socket.
///
/// Defense-in-depth for PID reuse: it's possible (though rare) for a
/// runner to die uncleanly, leave the socket file behind, and have its
/// PID immediately recycled by an unrelated process. The (pid_alive +
/// socket_exists) pair survives that case in almost all scenarios
/// because the unrelated process is exceedingly unlikely to be
/// listening on the same socket path. As a third layer, the daemon's
/// attach handshake (`AcpClient::attach` -> `initialize`) rejects any
/// peer that doesn't speak ACP within the 3s reconciler timeout, so a
/// truly unlucky PID/socket collision still falls back to a fresh
/// spawn rather than wedging the session.
pub fn is_record_live(rec: &WorkerRecord) -> bool {
    rec.runner_version == RUNNER_VERSION && is_pid_alive(rec.pid) && socket_exists(&rec.socket_path)
}

/// Whether the worker's recorded binary build matches the running
/// daemon's. A live-but-stale worker (this returns `false`) is still
/// "live" by `is_record_live`; the reconciler keeps it for any in-flight
/// turn and respawns it on the current binary at the next idle boundary,
/// rather than treating a version mismatch as death. See #1754.
///
/// Build identity is NOT folded into `is_record_live` on purpose: doing
/// so would make a busy stale worker look dead and push the reconciler
/// toward orphaning its in-flight turn.
pub fn is_build_current(rec: &WorkerRecord) -> bool {
    rec.build_version == crate::build_info::BUILD_VERSION
}

/// The ACP worker state ladder shared by `aoe ps --acp` and the deprecated
/// `aoe acp ps`: `dead` when the runner is not live; `detached` when it has
/// detached and has not re-attached since; `attached` otherwise. `live` is
/// the caller's [`is_record_live`] result, threaded in so a caller that
/// already computed it does not probe the socket twice.
pub(crate) fn worker_state_label(rec: &WorkerRecord, live: bool) -> &'static str {
    if !live {
        "dead"
    } else if rec
        .detached_at
        .is_some_and(|detached| rec.last_attached_at.unwrap_or(0) <= detached)
    {
        "detached"
    } else {
        "attached"
    }
}

fn socket_exists(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// Resolve the runner's PID from whichever source is still legible: the
/// on-disk record first, then (only when `load` returns `Err`, not when
/// it returns `Ok(None)`) `SO_PEERCRED` on the live socket. The
/// distinction matters: `Ok(None)` means the runner is already gone, so
/// falling through to the socket would just probe a stale inode; `Err`
/// means we lost the primary channel and must reach for the secondary
/// before the runner escapes both SIGTERM and the shutdown wait. `load`
/// returns `Err` only for true I/O failures (permissions, wrong file
/// type, transient); JSON parse errors are coerced to `Ok(None)`
/// upstream. See #2102.
pub fn pid_source_for(session_id: &str) -> Option<u32> {
    match load(session_id) {
        Ok(Some(record)) => (record.pid > 0).then_some(record.pid),
        Ok(None) => None,
        Err(e) => {
            let sock = socket_path_for(session_id).ok()?; // same validator as load(); degrades to None on invalid id
            let pid = crate::process::worker::peer_pid_from_socket(&sock);
            match pid {
                Some(peer_pid) => warn!(
                    target: "acp.registry",
                    session = %session_id,
                    pid = peer_pid,
                    "worker registry unreadable; recovered runner PID via SO_PEERCRED on socket: {e}"
                ),
                None => warn!(
                    target: "acp.registry",
                    session = %session_id,
                    "worker registry unreadable and no peer PID on socket; \
                     runner may be orphaned under PID 1: {e}"
                ),
            }
            pid
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn authenticated_unix_listener_fixture() {
        super::test_support::run_listener_fixture();
    }

    fn with_temp_home<F: FnOnce()>(f: F) {
        // Root under /tmp instead of the default $TMPDIR (which on
        // macOS points into /var/folders/... and blows past the
        // ~104-char sun_path limit once we tack on <app_dir>/acp-workers/
        // <session_id>.sock inside a peer_pid test).
        let tmp = TempDir::with_prefix_in("aoe-registry-", "/tmp").unwrap();
        let original = std::env::var_os("HOME");
        let original_xdg = std::env::var_os("XDG_CONFIG_HOME");
        // SAFETY: tests are serialized via `#[serial]`; the env mutation
        // window is bounded to this closure and restored on exit.
        unsafe {
            std::env::set_var("HOME", tmp.path());
            std::env::set_var("XDG_CONFIG_HOME", tmp.path().join(".config"));
        }
        f();
        unsafe {
            if let Some(v) = original {
                std::env::set_var("HOME", v);
            } else {
                std::env::remove_var("HOME");
            }
            if let Some(v) = original_xdg {
                std::env::set_var("XDG_CONFIG_HOME", v);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
        }
    }

    #[test]
    #[serial]
    fn roundtrip_save_load() {
        with_temp_home(|| {
            let rec = WorkerRecord::new(
                "sess-abc".into(),
                42,
                PathBuf::from("/tmp/sock"),
                "claude-agent-acp".into(),
                "claude".into(),
                PathBuf::from("/repo"),
                Some("claude-opus-4-7".into()),
                vec![],
                vec!["ANTHROPIC_API_KEY".into()],
                None,
                Some("personal".into()),
            );
            save(&rec).unwrap();
            let loaded = load("sess-abc").unwrap().unwrap();
            assert_eq!(loaded.session_id, "sess-abc");
            assert_eq!(loaded.pid, 42);
            assert_eq!(loaded.runner_version, RUNNER_VERSION);
            assert_eq!(loaded.agent_name, "claude-agent-acp");
            assert_eq!(loaded.agent_key, "claude");
        });
    }

    /// A fresh record is stamped with this binary's build identity and
    /// reports as current; the empty-string legacy default reports stale.
    /// This is the gate the reconciler uses to respawn workers left on an
    /// old binary after `aoe update`. See #1754.
    #[test]
    #[serial]
    fn build_version_stamped_and_current() {
        with_temp_home(|| {
            let rec = WorkerRecord::new(
                "sess-bv".into(),
                1,
                PathBuf::from("/tmp/sess-bv.sock"),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            assert_eq!(rec.build_version, crate::build_info::BUILD_VERSION);
            assert!(is_build_current(&rec));

            let mut stale = rec.clone();
            stale.build_version = String::new();
            assert!(
                !is_build_current(&stale),
                "empty (legacy) build_version must read as stale"
            );

            stale.build_version = "0.0.0+gdeadbeef".into();
            assert!(!is_build_current(&stale));
        });
    }

    /// Legacy records written before `build_version` existed must load
    /// with the empty-string default (and thus read as build-stale), not
    /// fail to deserialize.
    #[test]
    #[serial]
    fn load_legacy_record_without_build_version() {
        with_temp_home(|| {
            let dir = workers_dir().unwrap();
            let legacy = serde_json::json!({
                "runner_version": RUNNER_VERSION,
                "session_id": "legacy-bv-1",
                "pid": 7,
                "socket_path": "/tmp/legacy-bv.sock",
                "agent_name": "claude-agent-acp",
                "agent_key": "claude",
                "cwd": "/repo",
                "model": null,
                "additional_dirs": [],
                "provider_env_keys": [],
                "stored_acp_session_id": null,
                "source_profile": null,
                "started_at": 0,
                "last_attached_at": null,
                "detached_at": null
            });
            std::fs::write(
                dir.join("legacy-bv-1.json"),
                serde_json::to_string(&legacy).unwrap(),
            )
            .unwrap();
            let loaded = load("legacy-bv-1").unwrap().unwrap();
            assert_eq!(loaded.build_version, "");
            assert!(!is_build_current(&loaded));
        });
    }

    /// Legacy records written before the `agent_key` field existed
    /// must still load without surfacing a deserialization error;
    /// `serde(default)` fills in the empty string and call sites are
    /// responsible for falling back to `agent_name` or a default
    /// profile. See `Supervisor::agent_key_for_session`.
    #[test]
    #[serial]
    fn load_legacy_record_without_agent_key() {
        with_temp_home(|| {
            let dir = workers_dir().unwrap();
            // Hand-craft a record missing `agent_key` to simulate a
            // file written by an older daemon.
            let legacy = serde_json::json!({
                "runner_version": RUNNER_VERSION,
                "session_id": "legacy-1",
                "pid": 99,
                "socket_path": "/tmp/legacy.sock",
                "agent_name": "claude-agent-acp",
                "cwd": "/repo",
                "model": null,
                "additional_dirs": [],
                "provider_env_keys": [],
                "stored_acp_session_id": null,
                "started_at": 0,
                "last_attached_at": null,
                "detached_at": null
            });
            std::fs::write(
                dir.join("legacy-1.json"),
                serde_json::to_string(&legacy).unwrap(),
            )
            .unwrap();
            let loaded = load("legacy-1").unwrap().unwrap();
            assert_eq!(loaded.agent_name, "claude-agent-acp");
            assert_eq!(loaded.agent_key, "");
        });
    }

    /// Same legacy-record guarantee for `source_profile`: records written
    /// before the field existed must load with `None` (the documented
    /// fallback), not surface a deserialization error.
    #[test]
    #[serial]
    fn load_legacy_record_without_source_profile() {
        with_temp_home(|| {
            let dir = workers_dir().unwrap();
            let legacy = serde_json::json!({
                "runner_version": RUNNER_VERSION,
                "session_id": "legacy-sp-1",
                "pid": 7,
                "socket_path": "/tmp/legacy-sp.sock",
                "agent_name": "claude-agent-acp",
                "agent_key": "claude",
                "cwd": "/repo",
                "model": null,
                "additional_dirs": [],
                "provider_env_keys": [],
                "stored_acp_session_id": null,
                "started_at": 0,
                "last_attached_at": null,
                "detached_at": null
            });
            std::fs::write(
                dir.join("legacy-sp-1.json"),
                serde_json::to_string(&legacy).unwrap(),
            )
            .unwrap();
            let loaded = load("legacy-sp-1").unwrap().unwrap();
            assert_eq!(loaded.source_profile, None);
        });
    }

    /// Fresh records carry `source_profile` end-to-end (write + read).
    /// The roundtrip case is covered above; this asserts the field
    /// specifically because the reattach path depends on it.
    #[test]
    #[serial]
    fn source_profile_roundtrips() {
        with_temp_home(|| {
            let rec = WorkerRecord::new(
                "sess-sp".into(),
                1,
                PathBuf::from("/tmp/sess-sp.sock"),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                Some("personal".into()),
            );
            save(&rec).unwrap();
            let loaded = load("sess-sp").unwrap().unwrap();
            assert_eq!(loaded.source_profile.as_deref(), Some("personal"));
        });
    }

    #[test]
    #[serial]
    fn empty_stored_acp_session_id_normalizes_to_none() {
        with_temp_home(|| {
            let rec = WorkerRecord::new(
                "sess-empty-acp".into(),
                1,
                PathBuf::from("/tmp/sess-empty-acp.sock"),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                Some("initial-acp".into()),
                None,
            );
            save(&rec).unwrap();
            update_stored_acp_session_id(&rec, Some(""));
            let loaded = load("sess-empty-acp").unwrap().unwrap();
            assert_eq!(loaded.stored_acp_session_id, None);
        });
    }

    #[test]
    #[serial]
    fn list_filters_non_json_and_unparseable() {
        with_temp_home(|| {
            let dir = workers_dir().unwrap();
            std::fs::write(dir.join("not-json.json"), b"this isn't json").unwrap();
            std::fs::write(dir.join("ignored.txt"), b"{}").unwrap();
            let rec = WorkerRecord::new(
                "live".into(),
                1,
                PathBuf::from("/tmp/sock-live"),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            save(&rec).unwrap();
            let all = list().unwrap();
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].session_id, "live");
        });
    }

    #[test]
    #[serial]
    fn delete_removes_json_and_socket() {
        with_temp_home(|| {
            let dir = workers_dir().unwrap();
            let sock = dir.join("sess.sock");
            std::fs::write(&sock, b"").unwrap();
            let rec = WorkerRecord::new(
                "sess".into(),
                1,
                sock.clone(),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            save(&rec).unwrap();
            assert!(record_path("sess").unwrap().exists());
            assert!(sock.exists());
            delete("sess").unwrap();
            assert!(!record_path("sess").unwrap().exists());
        });
    }

    #[test]
    #[serial]
    fn compare_delete_preserves_a_published_replacement_generation() {
        with_temp_home(|| {
            let socket = socket_path_for("replace-race").unwrap();
            let old = WorkerRecord::new(
                "replace-race".into(),
                41,
                socket.clone(),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            save(&old).unwrap();
            let replacement = WorkerRecord {
                pid: 42,
                started_at: old.started_at.saturating_add(1),
                ..old.clone()
            };
            save(&replacement).unwrap();

            assert_eq!(
                delete_record_if_current(&old, None).unwrap(),
                GenerationDeleteOutcome::Changed
            );
            assert_eq!(load("replace-race").unwrap().unwrap().pid, 42);

            #[cfg(target_os = "linux")]
            {
                let mut live = std::process::Command::new("sleep")
                    .arg("30")
                    .spawn()
                    .expect("spawn live replacement");
                let live_record = WorkerRecord {
                    pid: live.id(),
                    ..replacement
                };
                save(&live_record).unwrap();
                let wrong_identity = process_start_identity_for(live.id()).map(|value| value + 1);
                assert_eq!(
                    delete_record_if_current(&live_record, wrong_identity).unwrap(),
                    GenerationDeleteOutcome::Changed
                );
                assert_eq!(load("replace-race").unwrap().unwrap().pid, live.id());
                let _ = live.kill();
                let _ = live.wait();
            }
        });
    }

    #[test]
    #[serial]
    fn dead_record_with_persisted_process_identity_is_deleted() {
        with_temp_home(|| {
            let mut record = WorkerRecord::new(
                "dead-generation".into(),
                2_000_000_000,
                PathBuf::from("/tmp/dead-generation.sock"),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            record.process_start_identity = Some(1234);
            save(&record).unwrap();

            assert_eq!(
                delete_dead_record_if_current(&record).unwrap(),
                GenerationDeleteOutcome::Deleted
            );
            assert!(load("dead-generation").unwrap().is_none());
        });
    }

    #[test]
    #[serial]
    fn delete_sweeps_empty_log_but_keeps_nonempty() {
        with_temp_home(|| {
            // Empty log (worker died before writing anything): swept.
            let empty_log = log_path_for("empty").unwrap();
            std::fs::create_dir_all(empty_log.parent().unwrap()).unwrap();
            std::fs::write(&empty_log, b"").unwrap();
            delete("empty").unwrap();
            assert!(
                !empty_log.exists(),
                "0-byte worker log should be swept on delete"
            );

            // Non-empty log (has post-mortem content): kept.
            let kept_log = log_path_for("kept").unwrap();
            std::fs::write(&kept_log, b"agent stderr line\n").unwrap();
            delete("kept").unwrap();
            assert!(
                kept_log.exists(),
                "non-empty worker log should survive delete for post-mortem"
            );
        });
    }

    #[test]
    #[serial]
    fn mark_attached_clears_detached() {
        with_temp_home(|| {
            let mut rec = WorkerRecord::new(
                "x".into(),
                1,
                PathBuf::from("/tmp/x.sock"),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            rec.detached_at = Some(100);
            save(&rec).unwrap();
            mark_attached(&rec);
            let after = load("x").unwrap().unwrap();
            assert!(after.last_attached_at.is_some());
            assert!(after.detached_at.is_none());
        });
    }

    #[test]
    #[serial]
    fn stale_generation_metadata_update_preserves_replacement_bytes() {
        with_temp_home(|| {
            let old = WorkerRecord::new(
                "metadata-race".into(),
                41,
                PathBuf::from("/tmp/metadata-race.sock"),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            save(&old).unwrap();
            let replacement = WorkerRecord {
                pid: 42,
                process_start_identity: Some(4242),
                started_at: old.started_at.saturating_add(1),
                ..old.clone()
            };
            save(&replacement).unwrap();
            let path = record_path("metadata-race").unwrap();
            let before = std::fs::read(&path).unwrap();

            mark_attached(&old);
            mark_detached(&old);
            update_stored_acp_session_id(&old, Some("stale-acp-id"));

            assert_eq!(std::fs::read(path).unwrap(), before);
        });
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[serial]
    fn legacy_identity_backfill_returns_metadata_authority() {
        use std::os::unix::net::UnixListener;

        with_temp_home(|| {
            let socket = socket_path_for("legacy-backfill").unwrap();
            let _listener = UnixListener::bind(&socket).unwrap();
            let mut legacy = WorkerRecord::new(
                "legacy-backfill".into(),
                std::process::id(),
                socket.clone(),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            legacy.process_start_identity = None;
            save(&legacy).unwrap();

            let authenticated =
                authenticate_generation_record(&legacy, "legacy-backfill", &socket, None)
                    .unwrap()
                    .expect("legacy runner authenticated by canonical socket peer");
            assert!(authenticated.process_start_identity.is_some());

            update_stored_acp_session_id(&authenticated, Some("session-from-legacy"));
            assert_eq!(
                load("legacy-backfill")
                    .unwrap()
                    .unwrap()
                    .stored_acp_session_id
                    .as_deref(),
                Some("session-from-legacy")
            );

            let explicit_socket = socket_path_for("legacy-explicit-peer").unwrap();
            let mut explicit = WorkerRecord::new(
                "legacy-explicit-peer".into(),
                std::process::id(),
                explicit_socket.clone(),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            explicit.process_start_identity = None;
            save(&explicit).unwrap();
            assert!(authenticate_generation_record(
                &explicit,
                "legacy-explicit-peer",
                &explicit_socket,
                Some(std::process::id()),
            )
            .unwrap()
            .is_some());
        });
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[serial]
    fn legacy_identity_without_reachable_or_matching_peer_fails_closed() {
        use std::os::unix::net::UnixListener;

        with_temp_home(|| {
            let missing_socket = socket_path_for("legacy-missing-peer").unwrap();
            let mut missing = WorkerRecord::new(
                "legacy-missing-peer".into(),
                std::process::id(),
                missing_socket.clone(),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            missing.process_start_identity = None;
            save(&missing).unwrap();
            assert!(authenticate_generation_record(
                &missing,
                "legacy-missing-peer",
                &missing_socket,
                None,
            )
            .unwrap()
            .is_none());

            let mismatched_socket = socket_path_for("legacy-mismatched-peer").unwrap();
            let _listener = UnixListener::bind(&mismatched_socket).unwrap();
            let mut mismatched = WorkerRecord::new(
                "legacy-mismatched-peer".into(),
                2_000_000_000,
                mismatched_socket.clone(),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            mismatched.process_start_identity = None;
            save(&mismatched).unwrap();
            assert!(authenticate_generation_record(
                &mismatched,
                "legacy-mismatched-peer",
                &mismatched_socket,
                None,
            )
            .unwrap()
            .is_none());
        });
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[serial]
    fn modern_identity_rejects_mismatched_peer_pid() {
        with_temp_home(|| {
            let socket = socket_path_for("modern-mismatched-peer").unwrap();
            let current_pid = std::process::id();
            let modern = WorkerRecord::new(
                "modern-mismatched-peer".into(),
                current_pid,
                socket.clone(),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            assert!(modern.process_start_identity.is_some());
            save(&modern).unwrap();
            let mismatched_pid = if current_pid == u32::MAX {
                current_pid - 1
            } else {
                current_pid + 1
            };

            assert!(authenticate_generation_record(
                &modern,
                "modern-mismatched-peer",
                &socket,
                Some(mismatched_pid),
            )
            .unwrap()
            .is_none());
        });
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[serial]
    fn stale_legacy_identity_cannot_bind_or_update_current_generation() {
        with_temp_home(|| {
            let socket = socket_path_for("legacy-stale").unwrap();
            let current = WorkerRecord::new(
                "legacy-stale".into(),
                std::process::id(),
                socket.clone(),
                "aoe-agent".into(),
                "aoe-agent".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            assert!(current.process_start_identity.is_some());
            save(&current).unwrap();
            let mut stale_legacy = current.clone();
            stale_legacy.process_start_identity = None;
            let before = std::fs::read(record_path("legacy-stale").unwrap()).unwrap();

            assert!(
                authenticate_generation_record(&stale_legacy, "legacy-stale", &socket, None,)
                    .unwrap()
                    .is_none()
            );
            update_stored_acp_session_id(&stale_legacy, Some("stale-session"));
            assert_eq!(
                std::fs::read(record_path("legacy-stale").unwrap()).unwrap(),
                before
            );
        });
    }

    #[test]
    fn worker_state_ladder() {
        let mut rec = WorkerRecord::new(
            "s".into(),
            1,
            PathBuf::from("/tmp/s.sock"),
            "claude-agent-acp".into(),
            "claude".into(),
            PathBuf::from("/repo"),
            None,
            vec![],
            vec![],
            None,
            None,
        );
        assert_eq!(worker_state_label(&rec, false), "dead");
        assert_eq!(worker_state_label(&rec, true), "attached");
        rec.detached_at = Some(100);
        rec.last_attached_at = Some(50);
        assert_eq!(worker_state_label(&rec, true), "detached");
        rec.last_attached_at = Some(150);
        assert_eq!(worker_state_label(&rec, true), "attached");
        // detached with no prior attach: last_attached_at None is treated as 0,
        // so 0 <= detached_at keeps it detached.
        rec.last_attached_at = None;
        assert_eq!(worker_state_label(&rec, true), "detached");
    }

    #[test]
    fn is_pid_alive_self() {
        let pid = std::process::id();
        assert!(is_pid_alive(pid));
    }

    #[test]
    fn is_pid_alive_unlikely_pid() {
        // PID 0 is the kernel scheduler / swapper; kill(0, 0) targets the
        // *process group*, not a real process. Use a very high value that
        // won't realistically be allocated.
        assert!(!is_pid_alive(2_000_000_000));
    }

    #[test]
    fn validate_session_id_accepts_uuids_and_test_ids() {
        // Production format: UUID v4 with hyphens.
        assert!(
            validate_session_id("550e8400-e29b-41d4-a716-446655440000").is_ok(),
            "must accept UUID v4 (the production session_id shape)"
        );
        // Test-prefixed ids with underscores and digits.
        assert!(validate_session_id("test_session_42").is_ok());
        assert!(validate_session_id("a").is_ok());
        assert!(validate_session_id("Z-0").is_ok());
    }

    #[test]
    fn validate_session_id_rejects_path_traversal_and_separators() {
        // The whole point of this check: don't let a CLI invocation of
        // `aoe __acp-runner --session-id "<evil>"` write files
        // outside the workers dir.
        for bad in [
            "",
            "..",
            "../../etc/passwd",
            "foo/bar",
            "foo\\bar",
            ".hidden",
            "with space",
            "with\0null",
            "trailing.",
            "good-then/../bad",
        ] {
            assert!(
                validate_session_id(bad).is_err(),
                "expected rejection for {bad:?}"
            );
        }
    }

    #[test]
    fn validate_session_id_rejects_overlong() {
        let long = "a".repeat(129);
        assert!(validate_session_id(&long).is_err());
        let ok = "a".repeat(128);
        assert!(validate_session_id(&ok).is_ok());
    }

    #[test]
    fn path_builders_propagate_validation_error() {
        // Defense-in-depth: even if some future caller forgets to
        // validate at the trust boundary, the path builders themselves
        // catch a bad id.
        assert!(record_path("../escape").is_err());
        assert!(socket_path_for("foo/bar").is_err());
        assert!(log_path_for("").is_err());
        assert!(restart_marker_path(".hidden").is_err());
    }

    #[test]
    #[serial]
    fn pid_source_for_prefers_record_pid_when_load_ok_some() {
        with_temp_home(|| {
            let rec = WorkerRecord::new(
                "sess-ok-some".into(),
                4242,
                PathBuf::from("/tmp/unused"),
                "claude-agent-acp".into(),
                "claude".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            save(&rec).unwrap();
            assert_eq!(pid_source_for("sess-ok-some"), Some(4242));
        });
    }

    #[test]
    #[serial]
    fn pid_source_for_returns_none_when_load_ok_none() {
        with_temp_home(|| {
            // No record on disk AND no socket: the runner is already
            // gone, so we must NOT fall through to the peer probe (the
            // socket, if any, would be a stale inode from a prior spawn).
            assert_eq!(pid_source_for("sess-missing"), None);
        });
    }

    /// #2102: the load-Err branch consults `peer_pid_from_socket` so an
    /// unreadable registry file no longer means the runner
    /// escapes SIGTERM and the shutdown_and_wait poll. Binding a
    /// listener in the test process makes own PID the peer, which is
    /// what the helper must surface.
    #[cfg(unix)]
    #[test]
    #[serial]
    fn pid_source_for_falls_back_to_peer_pid_on_load_err() {
        with_temp_home(|| {
            let session_id = "sess-load-err";
            let rec = WorkerRecord::new(
                session_id.into(),
                4242,
                socket_path_for(session_id).unwrap(),
                "claude-agent-acp".into(),
                "claude".into(),
                PathBuf::from("/repo"),
                None,
                vec![],
                vec![],
                None,
                None,
            );
            save(&rec).unwrap();
            // A directory is unreadable as a record even when tests run as root.
            let rec_path = record_path(session_id).unwrap();
            std::fs::remove_file(&rec_path).unwrap();
            std::fs::create_dir(&rec_path).unwrap();
            assert!(
                load(session_id).is_err(),
                "fixture must force load() to return Err"
            );
            // Bind a real UDS listener in the test process so peer_pid
            // resolves to own PID.
            let sock_path = socket_path_for(session_id).unwrap();
            let _listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();
            assert_eq!(pid_source_for(session_id), Some(std::process::id()));
        });
    }
}
