//! Managed loopback Foreseerr child lifecycle.
//!
//! This module deliberately has no knowledge of CEF.  It owns only the bundled
//! Foreseerr runtime process and exposes a validated, exact loopback origin to
//! the caller.
//!
//! The bundled runtime is a Foreseerr runtime directory (Foreseerr's
//! `scripts/build-runtime.sh`): the `foreseerr` binary next to
//! `seerr-api.yml`, the built frontend in `dist/public/` and
//! `committag.json`. The child protocol (environment, readiness record,
//! control channel, exit codes) is Foreseerr's "Desktop child" contract in
//! `backend-rs/README.md`.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, STILL_ACTIVE};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::config::AppConfig;

pub const READY_PREFIX: &str = "FORESEERR_DESKTOP_READY ";
pub const READY_PROTOCOL_VERSION: u32 = 1;
const READY_TIMEOUT: Duration = Duration::from_secs(90);
const BUNDLED_FORESEERR_VERSION_FILE: &str = include_str!("../foreseerr.version");
const BUNDLED_FORESEERR_REVISION_FILE: &str = include_str!("../foreseerr.rev");
/// The runtime directory below `resources/`.
const RUNTIME_DIRECTORY: &str = "foreseerr";
const RUNTIME_BINARY: &str = if cfg!(windows) {
    "foreseerr.exe"
} else {
    "foreseerr"
};
/// Files the binary reads from its app root besides itself. Foreseerr exits
/// at startup without the OpenAPI spec; without the frontend it would start
/// but serve no UI.
const RUNTIME_FILES: &[&str] = &["seerr-api.yml", "dist/public/index.html"];
/// Exit codes of a failed desktop start (`DESKTOP_LOCK_EXIT_CODE`,
/// `DESKTOP_SCHEMA_EXIT_CODE`); any other failed start exits with 1.
const CHILD_LOCK_EXIT_CODE: i32 = 73;
const CHILD_SCHEMA_EXIT_CODE: i32 = 74;
/// What the child gets to finish open requests and jobs when asked to stop.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(2);
/// After the deadline the child still checkpoints and closes SQLite (at
/// least one more second) and releases its instance lock.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// Variables the child reads that must come from the supervisor (set below)
/// or must not leak in from the desktop's own environment: a hosted
/// database, hybrid/plugin modes, dev overrides of bundled paths, or a
/// `RUST_LOG` meant for the desktop that would filter the child's console.
const CLEARED_CHILD_ENV: &[&str] = &[
    "PORT",
    "HOST",
    "CONFIG_DIRECTORY",
    "CACHE_DIRECTORY",
    "LOG_DIRECTORY",
    "FORESEERR_APP_ROOT",
    "FORESEERR_NODE_URL",
    "FORESEERR_STANDALONE",
    "FORESEERR_RUST_JOBS",
    "FORESEERR_PLUGIN",
    "FORESEERR_PLUGIN_SECRET",
    "FORESEERR_PLUGIN_INSTANCE",
    "FORESEERR_BASE_PATH",
    "FORESEERR_PUBLIC_BASE_PATH",
    "FORESEERR_PUBLIC_PATH",
    "FORESEERR_SWAGGER_UI_PATH",
    "FORESEERR_OPENAPI_VALIDATOR",
    "FORESEERR_MAPPING_TEST_DB",
    "OPENAPI_SPEC",
    "OPENAPI_CORPUS",
    "SHADOW_CA_FILE",
    "API_KEY",
    "RUST_LOG",
    "DB_TYPE",
    "DB_HOST",
    "DB_PORT",
    "DB_USER",
    "DB_PASS",
    "DB_NAME",
    "DB_SOCKET_PATH",
    "DB_USE_SSL",
    "DB_SSL_REJECT_UNAUTHORIZED",
    "DB_SSL_CA",
    "DB_SSL_CA_FILE",
    "DB_SSL_KEY",
    "DB_SSL_KEY_FILE",
    "DB_SSL_CERT",
    "DB_SSL_CERT_FILE",
];

fn bundled_foreseerr_version() -> &'static str {
    BUNDLED_FORESEERR_VERSION_FILE.trim()
}

fn bundled_foreseerr_revision() -> &'static str {
    BUNDLED_FORESEERR_REVISION_FILE.trim()
}

#[derive(Debug, Deserialize, Serialize)]
struct RuntimeVersion {
    foreseerr_version: String,
    #[serde(default)]
    schema_version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyRecord {
    pub protocol_version: u32,
    pub pid: u32,
    pub origin: String,
    pub foreseerr_version: String,
    pub commit: String,
    pub schema_version: u64,
}

#[derive(Debug)]
pub enum SupervisorError {
    ResourcesNotFound(PathBuf),
    Spawn(std::io::Error),
    Startup(String),
    InvalidReady(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeHealth {
    Healthy,
    Unhealthy,
    Exited,
}

/// Tracks the recovery threshold without conflating a transient health probe
/// failure with a confirmed child-runtime failure.
#[derive(Debug, Default)]
pub struct RuntimeHealthTracker {
    consecutive_unhealthy: u8,
}

impl RuntimeHealthTracker {
    pub fn observe(&mut self, health: RuntimeHealth) -> bool {
        match health {
            RuntimeHealth::Healthy => {
                self.consecutive_unhealthy = 0;
                false
            }
            RuntimeHealth::Exited => true,
            RuntimeHealth::Unhealthy => {
                self.consecutive_unhealthy = self.consecutive_unhealthy.saturating_add(1);
                self.consecutive_unhealthy >= 3
            }
        }
    }
}
impl std::fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ResourcesNotFound(path) => write!(
                f,
                "Bundled Foreseerr resources were not found at {}",
                path.display()
            ),
            Self::Spawn(err) => write!(f, "Could not start bundled Foreseerr: {err}"),
            Self::Startup(message) | Self::InvalidReady(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for SupervisorError {}

pub struct StandaloneSupervisor {
    child: ManagedChild,
    pub origin: String,
    pub diagnostics: Vec<String>,
    config: AppConfig,
    config_dir: PathBuf,
    ready: ReadyRecord,
    upgrade_backup: Option<PathBuf>,
    successful_status_recorded: bool,
}

/// Owns a Windows Job Object with kill-on-close semantics, so nothing the
/// runtime starts can outlive the desktop on Windows.
#[cfg(windows)]
struct WindowsJob {
    handle: HANDLE,
}

#[cfg(windows)]
impl WindowsJob {
    fn assign(child: &Child) -> std::io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            unsafe { CloseHandle(handle) };
            return Err(std::io::Error::last_os_error());
        }
        let assigned = unsafe { AssignProcessToJobObject(handle, child.as_raw_handle()) };
        if assigned == 0 {
            unsafe { CloseHandle(handle) };
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    fn terminate(&self) {
        unsafe { TerminateJobObject(self.handle, 1) };
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

/// Where one runtime child runs and what it owns.
struct ChildLaunch<'a> {
    runtime_dir: &'a Path,
    config_dir: &'a Path,
    cache_dir: &'a Path,
    log_dir: &'a Path,
    port: u16,
    cache_limit_bytes: u64,
}

/// A child that announced a valid readiness record.
struct LaunchedChild {
    child: ManagedChild,
    ready: ReadyRecord,
    diagnostics: Vec<String>,
}

/// The runtime process and its stdin control channel.
struct ManagedChild {
    process: Child,
    stdin: Option<ChildStdin>,
    #[cfg(windows)]
    job: WindowsJob,
}

impl ManagedChild {
    fn spawn(mut command: Command) -> Result<Self, SupervisorError> {
        let mut process = command.spawn().map_err(SupervisorError::Spawn)?;
        #[cfg(windows)]
        let job = match WindowsJob::assign(&process) {
            Ok(job) => job,
            Err(error) => {
                let _ = process.kill();
                let _ = process.wait();
                return Err(SupervisorError::Spawn(error));
            }
        };
        let stdin = process.stdin.take();
        Ok(Self {
            process,
            stdin,
            #[cfg(windows)]
            job,
        })
    }

    fn send_control(&mut self, message: &str) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = writeln!(stdin, "{message}");
            let _ = stdin.flush();
        }
    }

    fn has_exited(&mut self) -> bool {
        self.process.try_wait().ok().flatten().is_some()
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.has_exited() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Ask the child to stop through its control channel and wait for it:
    /// it drains open requests and jobs until the deadline, checkpoints and
    /// closes SQLite, releases `state/instance.lock` and exits with 0. Only a
    /// child that misses that window is signalled. Foreseerr takes any
    /// second stop request (a signal, or stdin closing) as "exit now", so
    /// signalling right after the control message would skip the checkpoint
    /// and leave the lock behind.
    fn shutdown(&mut self) {
        if self.has_exited() {
            return;
        }
        self.send_control(&format!(
            r#"{{"type":"shutdown","deadlineMs":{}}}"#,
            SHUTDOWN_DEADLINE.as_millis()
        ));
        if self.wait_for_exit(SHUTDOWN_DEADLINE + SHUTDOWN_GRACE) {
            return;
        }
        self.terminate(Duration::from_secs(1));
    }

    /// Signal the child's process group (Windows: terminate its job), give
    /// it `grace` to exit, then kill it.
    fn terminate(&mut self, grace: Duration) {
        if self.has_exited() {
            return;
        }
        #[cfg(unix)]
        self.signal_group(libc::SIGTERM);
        #[cfg(windows)]
        self.job.terminate();
        #[cfg(all(not(unix), not(windows)))]
        let _ = self.process.kill();
        if self.wait_for_exit(grace) {
            return;
        }
        #[cfg(unix)]
        self.signal_group(libc::SIGKILL);
        #[cfg(windows)]
        self.job.terminate();
        #[cfg(all(not(unix), not(windows)))]
        let _ = self.process.kill();
        let _ = self.process.wait();
    }

    #[cfg(unix)]
    fn signal_group(&self, signal: libc::c_int) {
        if let Ok(pid) = i32::try_from(self.process.id()) {
            // The child leads a dedicated process group (`child_command`).
            unsafe { libc::kill(-pid, signal) };
        }
    }
}

/// The desktop child invocation: Foreseerr's "Desktop child" environment,
/// with the runtime directory as working directory and app root.
fn child_command(launch: &ChildLaunch<'_>) -> Command {
    let mut command = Command::new(launch.runtime_dir.join(RUNTIME_BINARY));
    command
        .current_dir(launch.runtime_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    for key in CLEARED_CHILD_ENV {
        command.env_remove(key);
    }
    command
        .env("FORESEERR_RUNTIME", "desktop")
        // The child is the whole backend; it must never forward routes to a
        // Node server named in the desktop's environment.
        .env("FORESEERR_STANDALONE", "1")
        // seerr-api.yml, dist/public and committag.json.
        .env("FORESEERR_APP_ROOT", launch.runtime_dir)
        // Production runs the database migrations before the settings load
        // and serves the bundled frontend.
        .env("NODE_ENV", "production")
        .env("CONFIG_DIRECTORY", launch.config_dir)
        .env("CACHE_DIRECTORY", launch.cache_dir)
        .env("LOG_DIRECTORY", launch.log_dir)
        // The desktop child always binds 127.0.0.1; HOST is informational.
        .env("HOST", "127.0.0.1")
        .env("PORT", launch.port.to_string())
        .env("FORESEERR_COMMIT", bundled_foreseerr_revision())
        .env(
            "FORESEER_CACHE_LIMIT_BYTES",
            launch.cache_limit_bytes.to_string(),
        );
    command
}

/// Start the runtime and wait for its readiness record. Any failure reaps
/// the child before returning.
fn launch(spec: &ChildLaunch<'_>) -> Result<LaunchedChild, SupervisorError> {
    let mut child = ManagedChild::spawn(child_command(spec))?;
    let stdout = child.process.stdout.take().expect("stdout piped");
    let stderr = child.process.stderr.take().expect("stderr piped");
    let (tx, rx) = mpsc::channel();
    let tx_stdout = tx.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx_stdout.send((false, line.unwrap_or_default())).is_err() {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if tx.send((true, line.unwrap_or_default())).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut diagnostics = Vec::new();
    loop {
        match child.process.try_wait() {
            Ok(Some(status)) => {
                // The child logs why it failed right before exiting; collect
                // what is still in the pipes.
                let drain_until = Instant::now() + Duration::from_secs(1);
                while let Ok((_, line)) =
                    rx.recv_timeout(drain_until.saturating_duration_since(Instant::now()))
                {
                    push_diagnostic(&mut diagnostics, &line);
                }
                child.terminate(Duration::from_secs(10));
                return Err(SupervisorError::Startup(exited_before_readiness(
                    status,
                    &diagnostics,
                )));
            }
            Ok(None) => {}
            Err(error) => {
                child.terminate(Duration::from_secs(10));
                return Err(SupervisorError::Spawn(error));
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            child.terminate(Duration::from_secs(10));
            return Err(SupervisorError::Startup(format!(
                "Timed out waiting for bundled Foreseerr readiness; {}",
                diagnostics.join("\n")
            )));
        }
        let (is_stderr, line) = match rx.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(entry) => entry,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Both pipes closed; keep polling for the exit status.
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        push_diagnostic(&mut diagnostics, &line);
        if is_stderr || !line.starts_with(READY_PREFIX) {
            continue;
        }
        let ready = serde_json::from_str::<ReadyRecord>(&line[READY_PREFIX.len()..])
            .map_err(|_| {
                SupervisorError::InvalidReady(
                    "Bundled Foreseerr emitted malformed readiness data".into(),
                )
            })
            .and_then(|ready| validate_ready(&ready).map(|()| ready))
            .and_then(|ready| {
                if ready.foreseerr_version == bundled_foreseerr_version() {
                    Ok(ready)
                } else {
                    Err(SupervisorError::InvalidReady(format!(
                        "Bundled Foreseerr version mismatch (expected {}, got {})",
                        bundled_foreseerr_version(),
                        ready.foreseerr_version
                    )))
                }
            });
        let ready = match ready {
            Ok(ready) => ready,
            Err(error) => {
                child.terminate(Duration::from_secs(10));
                return Err(error);
            }
        };
        // Keep draining stdout/stderr for the child's lifetime: dropping
        // `rx` would end the reader threads and close the pipes under a
        // child that still logs every request.
        std::thread::spawn(move || while rx.recv().is_ok() {});
        return Ok(LaunchedChild {
            child,
            ready,
            diagnostics,
        });
    }
}

fn exited_before_readiness(status: ExitStatus, diagnostics: &[String]) -> String {
    let reason = match status.code() {
        Some(CHILD_LOCK_EXIT_CODE) => "Another Foreseer Desktop instance owns this data directory",
        Some(CHILD_SCHEMA_EXIT_CODE) => {
            "The standalone database was migrated by a newer Foreseerr than the bundled one"
        }
        _ => "Bundled Foreseerr exited before readiness",
    };
    format!("{reason} ({status}); {}", diagnostics.join("\n"))
}

impl StandaloneSupervisor {
    pub fn start(config: &AppConfig) -> Result<Self, SupervisorError> {
        Self::start_on_port(config, 0)
    }

    fn start_on_port(config: &AppConfig, port: u16) -> Result<Self, SupervisorError> {
        let runtime_dir = runtime_directory()?;
        let config_dir = AppConfig::standalone_data_directory().ok_or_else(|| {
            SupervisorError::Startup("No platform configuration directory is available".into())
        })?;
        let cache_dir = AppConfig::standalone_cache_directory().ok_or_else(|| {
            SupervisorError::Startup("No platform cache directory is available".into())
        })?;
        let log_dir = AppConfig::standalone_log_directory().ok_or_else(|| {
            SupervisorError::Startup("No platform log directory is available".into())
        })?;
        for directory in [
            &config_dir,
            &cache_dir,
            &log_dir,
            &config_dir.join("state"),
            &config_dir.join("backups"),
        ] {
            std::fs::create_dir_all(directory).map_err(SupervisorError::Spawn)?;
        }
        ensure_no_active_instance_lock(&config_dir)?;
        let upgrade_backup = backup_before_upgrade(&config_dir)?;
        let launched = launch(&ChildLaunch {
            runtime_dir: &runtime_dir,
            config_dir: &config_dir,
            cache_dir: &cache_dir,
            log_dir: &log_dir,
            port,
            cache_limit_bytes: config.standalone.cache_limit_bytes,
        })?;
        Ok(Self {
            child: launched.child,
            origin: launched.ready.origin.clone(),
            diagnostics: launched.diagnostics,
            config: config.clone(),
            config_dir,
            ready: launched.ready,
            upgrade_backup,
            successful_status_recorded: false,
        })
    }
    pub fn set_playback_active(&mut self, active: bool) {
        self.child.send_control(&format!(
            r#"{{"type":"runtime-state","playbackActive":{active}}}"#
        ));
    }

    /// Restart on the original loopback port. The CEF extension descriptor is
    /// bound to that exact origin, so a different port is a recoverable error
    /// that requires a full application relaunch rather than a weakened allow
    /// list.
    pub fn retry_on_original_port(&mut self) -> Result<(), SupervisorError> {
        let original_origin = self.origin.clone();
        let port = url::Url::parse(&original_origin)
            .ok()
            .and_then(|url| url.port())
            .ok_or_else(|| {
                SupervisorError::Startup("Could not recover the previous loopback port".into())
            })?;
        let config = self.config.clone();
        self.shutdown();
        let mut replacement = Self::start_on_port(&config, port)?;
        if replacement.origin != original_origin {
            replacement.shutdown();
            return Err(SupervisorError::Startup(
                "Bundled Foreseerr did not rebind the previous loopback port".into(),
            ));
        }
        *self = replacement;
        Ok(())
    }
    /// Poll only after readiness. Callers can require three consecutive
    /// unhealthy results before presenting recovery UI.
    pub fn health(&mut self) -> RuntimeHealth {
        match self.child.process.try_wait() {
            Ok(Some(_)) => RuntimeHealth::Exited,
            Ok(None) if self.status_is_healthy() => match self.record_successful_status() {
                Ok(()) => RuntimeHealth::Healthy,
                Err(error) => {
                    self.diagnostics.push(redact(&error.to_string()));
                    RuntimeHealth::Unhealthy
                }
            },
            Ok(None) | Err(_) => RuntimeHealth::Unhealthy,
        }
    }
    /// Stop the child gracefully (see [`ManagedChild::shutdown`]). Safe to
    /// call again once the child has exited.
    pub fn shutdown(&mut self) {
        self.child.shutdown();
    }

    fn status_is_healthy(&self) -> bool {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .build()
            .into();
        agent
            .get(&format!("{}/api/v1/status", self.origin))
            .call()
            .ok()
            .is_some_and(|response| response.status().is_success())
    }

    /// Readiness proves that the server owns the exact loopback listener. The
    /// first status response is deliberately recorded only after CEF is live:
    /// it is both the supervisor's runtime health baseline and the point at
    /// which an upgrade may be considered successful.
    fn record_successful_status(&mut self) -> Result<(), SupervisorError> {
        if self.successful_status_recorded {
            return Ok(());
        }
        write_runtime_version(
            &self.config_dir,
            &self.ready.foreseerr_version,
            self.ready.schema_version,
        )?;
        if let Some(backup) = self.upgrade_backup.as_deref() {
            complete_upgrade_backup(backup, &self.ready)?;
        }
        self.upgrade_backup = None;
        self.successful_status_recorded = true;
        Ok(())
    }
}
impl Drop for StandaloneSupervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn resource_root() -> Result<PathBuf, SupervisorError> {
    let executable = std::env::current_exe().map_err(SupervisorError::Spawn)?;
    let base = executable
        .parent()
        .unwrap_or(Path::new("."))
        .join("resources");
    if base.is_dir() {
        Ok(base)
    } else {
        Err(SupervisorError::ResourcesNotFound(base))
    }
}

/// The staged runtime directory, `resources/foreseerr/`.
fn runtime_directory() -> Result<PathBuf, SupervisorError> {
    let runtime = resource_root()?.join(RUNTIME_DIRECTORY);
    if is_runtime_directory(&runtime) {
        Ok(runtime)
    } else {
        Err(SupervisorError::ResourcesNotFound(runtime))
    }
}

fn is_runtime_directory(runtime: &Path) -> bool {
    runtime.join(RUNTIME_BINARY).is_file()
        && RUNTIME_FILES
            .iter()
            .all(|relative| runtime.join(relative).is_file())
}
fn validate_ready(ready: &ReadyRecord) -> Result<(), SupervisorError> {
    if ready.protocol_version != READY_PROTOCOL_VERSION {
        return Err(SupervisorError::InvalidReady(
            "Bundled Foreseerr uses an unsupported desktop protocol".into(),
        ));
    }
    if ready.commit != bundled_foreseerr_revision() {
        return Err(SupervisorError::InvalidReady(
            "Bundled Foreseerr supplied an unexpected revision".into(),
        ));
    }
    let url = url::Url::parse(&ready.origin).map_err(|_| {
        SupervisorError::InvalidReady("Bundled Foreseerr supplied an invalid origin".into())
    })?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().unwrap_or(0) == 0
        || url.username() != ""
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SupervisorError::InvalidReady(
            "Bundled Foreseerr supplied a non-loopback origin".into(),
        ));
    }
    Ok(())
}
fn redact(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    if ["authorization", "cookie", "token", "ticket"]
        .iter()
        .any(|secret| lower.contains(secret))
    {
        "[redacted child diagnostic]".into()
    } else {
        line.into()
    }
}

/// Keep the last 200 child console lines, redacted and without the ANSI
/// colours of the child's winston-format console.
fn push_diagnostic(diagnostics: &mut Vec<String>, line: &str) {
    if diagnostics.len() == 200 {
        diagnostics.remove(0);
    }
    diagnostics.push(redact(&strip_ansi(line)));
}

fn strip_ansi(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // A CSI sequence ends with a byte in 0x40..=0x7e.
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    plain
}

fn ensure_no_active_instance_lock(config_dir: &Path) -> Result<(), SupervisorError> {
    let lock = config_dir.join("state/instance.lock");
    if !lock.is_file() {
        return Ok(());
    }
    let pid = fs::read_to_string(&lock)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    if pid.is_some_and(process_is_alive) {
        return Err(SupervisorError::Startup(
            "Another Foreseer Desktop instance owns this data directory".into(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        // An inaccessible process must be treated as live: proceeding would
        // risk modifying a database an existing desktop child owns.
        return true;
    }
    let mut exit_code = 0;
    let queried = unsafe { GetExitCodeProcess(handle, &mut exit_code) } != 0;
    unsafe { CloseHandle(handle) };
    // `STILL_ACTIVE` is an NTSTATUS (i32); the exit code is a DWORD.
    queried && exit_code == STILL_ACTIVE as u32
}

#[cfg(all(not(unix), not(windows)))]
fn process_is_alive(_pid: u32) -> bool {
    // Unsupported platforms fail closed whenever a parseable lock is found.
    true
}

fn backup_before_upgrade(config_dir: &Path) -> Result<Option<PathBuf>, SupervisorError> {
    let state_file = config_dir.join("state/runtime-version.json");
    let previous = fs::read_to_string(&state_file)
        .ok()
        .and_then(|text| serde_json::from_str::<RuntimeVersion>(&text).ok());
    let Some(previous) = previous else {
        return Ok(None);
    };
    if previous.foreseerr_version == bundled_foreseerr_version() {
        return Ok(None);
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup = config_dir
        .join("backups")
        .join(format!("{}-{timestamp}", previous.foreseerr_version));
    fs::create_dir_all(&backup).map_err(SupervisorError::Spawn)?;
    for relative in [
        "settings.json",
        "db/db.sqlite3",
        "db/db.sqlite3-wal",
        "db/db.sqlite3-shm",
    ] {
        let source = config_dir.join(relative);
        if source.is_file() {
            let target = backup.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(SupervisorError::Spawn)?;
            }
            fs::copy(source, target).map_err(SupervisorError::Spawn)?;
        }
    }
    let metadata = serde_json::json!({
        "previousVersion": previous.foreseerr_version,
        "previousSchemaVersion": previous.schema_version,
        "newVersion": bundled_foreseerr_version(),
        "newSchemaVersion": null,
        "createdAt": timestamp,
    });
    fs::write(
        backup.join("metadata.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .map_err(SupervisorError::Spawn)?;
    let backups = config_dir.join("backups");
    let mut entries: Vec<_> = fs::read_dir(&backups)
        .map_err(SupervisorError::Spawn)?
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    while entries.len() > 3 {
        let oldest = entries.remove(0);
        fs::remove_dir_all(oldest.path()).map_err(SupervisorError::Spawn)?;
    }
    Ok(Some(backup))
}

fn complete_upgrade_backup(backup: &Path, ready: &ReadyRecord) -> Result<(), SupervisorError> {
    let metadata_path = backup.join("metadata.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&metadata_path).map_err(SupervisorError::Spawn)?)
            .map_err(|error| SupervisorError::Startup(error.to_string()))?;
    metadata["newVersion"] = serde_json::Value::String(ready.foreseerr_version.clone());
    metadata["newSchemaVersion"] = serde_json::Value::from(ready.schema_version);
    fs::write(
        metadata_path,
        serde_json::to_vec_pretty(&metadata)
            .map_err(|error| SupervisorError::Startup(error.to_string()))?,
    )
    .map_err(SupervisorError::Spawn)
}

fn write_runtime_version(
    config_dir: &Path,
    version: &str,
    schema_version: u64,
) -> Result<(), SupervisorError> {
    let state = config_dir.join("state/runtime-version.json");
    let payload = serde_json::to_vec_pretty(&RuntimeVersion {
        foreseerr_version: version.into(),
        schema_version,
    })
    .map_err(|error| SupervisorError::Startup(error.to_string()))?;
    fs::write(state, payload).map_err(SupervisorError::Spawn)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_requires_exact_loopback_origin() {
        let ready = ReadyRecord {
            protocol_version: 1,
            pid: 1,
            origin: "http://127.0.0.1:43127".into(),
            foreseerr_version: "0.6.2".into(),
            commit: bundled_foreseerr_revision().into(),
            schema_version: 1,
        };
        assert!(validate_ready(&ready).is_ok());
        let mut bad = ready;
        bad.origin = "http://localhost:43127".into();
        assert!(validate_ready(&bad).is_err());

        let mut wrong_revision = ReadyRecord {
            protocol_version: 1,
            pid: 1,
            origin: "http://127.0.0.1:43127".into(),
            foreseerr_version: "0.6.2".into(),
            commit: "unexpected".into(),
            schema_version: 1,
        };
        assert!(validate_ready(&wrong_revision).is_err());
        wrong_revision.commit = bundled_foreseerr_revision().into();
        assert!(validate_ready(&wrong_revision).is_ok());
    }

    #[test]
    fn health_tracker_requires_three_probe_failures_but_exits_immediately() {
        let mut tracker = RuntimeHealthTracker::default();
        assert!(!tracker.observe(RuntimeHealth::Unhealthy));
        assert!(!tracker.observe(RuntimeHealth::Unhealthy));
        assert!(tracker.observe(RuntimeHealth::Unhealthy));
        assert!(!tracker.observe(RuntimeHealth::Healthy));
        assert!(tracker.observe(RuntimeHealth::Exited));
    }

    #[test]
    fn upgrade_backup_records_schema_only_after_verified_readiness() {
        let temporary = tempfile::tempdir().unwrap();
        let config_dir = temporary.path();
        fs::create_dir_all(config_dir.join("state")).unwrap();
        fs::write(
            config_dir.join("state/runtime-version.json"),
            r#"{"foreseerr_version":"0.0.1","schema_version":123}"#,
        )
        .unwrap();

        let backup = backup_before_upgrade(config_dir).unwrap().unwrap();
        let pending: serde_json::Value =
            serde_json::from_slice(&fs::read(backup.join("metadata.json")).unwrap()).unwrap();
        assert_eq!(pending["previousSchemaVersion"], 123);
        assert!(pending["newSchemaVersion"].is_null());

        complete_upgrade_backup(
            &backup,
            &ReadyRecord {
                protocol_version: READY_PROTOCOL_VERSION,
                pid: 1,
                origin: "http://127.0.0.1:43127".into(),
                foreseerr_version: bundled_foreseerr_version().into(),
                commit: "test".into(),
                schema_version: 456,
            },
        )
        .unwrap();
        let complete: serde_json::Value =
            serde_json::from_slice(&fs::read(backup.join("metadata.json")).unwrap()).unwrap();
        assert_eq!(complete["newSchemaVersion"], 456);
    }

    #[test]
    fn legacy_runtime_version_defaults_schema_to_zero() {
        let state: RuntimeVersion =
            serde_json::from_str(r#"{"foreseerr_version":"0.6.0"}"#).unwrap();
        assert_eq!(state.schema_version, 0);
    }

    #[test]
    fn active_instance_lock_prevents_upgrade_backup_work() {
        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join("instance.lock"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();

        assert!(ensure_no_active_instance_lock(temporary.path()).is_err());
    }

    #[test]
    fn managed_child_scrubs_hosted_database_and_mode_overrides() {
        for variable in [
            "DB_TYPE",
            "DB_HOST",
            "DB_PORT",
            "DB_USER",
            "DB_PASS",
            "DB_NAME",
            "FORESEERR_NODE_URL",
            "FORESEERR_PLUGIN",
            "FORESEERR_PUBLIC_PATH",
            "API_KEY",
            "RUST_LOG",
        ] {
            assert!(CLEARED_CHILD_ENV.contains(&variable), "{variable}");
        }
    }

    fn child_env(command: &Command) -> std::collections::HashMap<String, Option<String>> {
        command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    #[test]
    fn child_command_follows_the_desktop_child_contract() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime = temporary.path().join("resources/foreseerr");
        let config = temporary.path().join("config");
        let cache = temporary.path().join("cache");
        let logs = temporary.path().join("config/logs");
        let command = child_command(&ChildLaunch {
            runtime_dir: &runtime,
            config_dir: &config,
            cache_dir: &cache,
            log_dir: &logs,
            port: 43127,
            cache_limit_bytes: 1024,
        });

        assert_eq!(command.get_program(), runtime.join(RUNTIME_BINARY));
        assert_eq!(command.get_args().count(), 0);
        assert_eq!(command.get_current_dir(), Some(runtime.as_path()));
        let env = child_env(&command);
        let set = |key: &str| env.get(key).cloned().flatten();
        let path = |dir: &Path| Some(dir.to_string_lossy().into_owned());
        assert_eq!(set("FORESEERR_RUNTIME").as_deref(), Some("desktop"));
        assert_eq!(set("FORESEERR_STANDALONE").as_deref(), Some("1"));
        assert_eq!(set("NODE_ENV").as_deref(), Some("production"));
        assert_eq!(set("FORESEERR_APP_ROOT"), path(&runtime));
        assert_eq!(set("CONFIG_DIRECTORY"), path(&config));
        assert_eq!(set("CACHE_DIRECTORY"), path(&cache));
        assert_eq!(set("LOG_DIRECTORY"), path(&logs));
        assert_eq!(set("PORT").as_deref(), Some("43127"));
        assert_eq!(
            set("FORESEERR_COMMIT").as_deref(),
            Some(bundled_foreseerr_revision())
        );
        assert_eq!(set("FORESEER_CACHE_LIMIT_BYTES").as_deref(), Some("1024"));
        for key in [
            "FORESEERR_NODE_URL",
            "FORESEERR_PLUGIN",
            "DB_TYPE",
            "RUST_LOG",
        ] {
            assert_eq!(env.get(key), Some(&None), "{key} is removed");
        }
    }

    #[test]
    fn runtime_directory_needs_binary_spec_and_frontend() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime = temporary.path();
        fs::write(runtime.join(RUNTIME_BINARY), b"").unwrap();
        fs::write(runtime.join("seerr-api.yml"), b"").unwrap();
        assert!(!is_runtime_directory(runtime));
        fs::create_dir_all(runtime.join("dist/public")).unwrap();
        fs::write(runtime.join("dist/public/index.html"), b"").unwrap();
        assert!(is_runtime_directory(runtime));
        fs::remove_file(runtime.join("seerr-api.yml")).unwrap();
        assert!(!is_runtime_directory(runtime));
    }

    #[cfg(unix)]
    #[test]
    fn early_exit_names_lock_and_schema_failures() {
        use std::os::unix::process::ExitStatusExt;
        let lines = vec!["Error: boom".to_string()];
        let message = exited_before_readiness(ExitStatus::from_raw(73 << 8), &lines);
        assert!(message.starts_with("Another Foreseer Desktop instance owns this data directory"));
        assert!(message.ends_with("Error: boom"));
        let message = exited_before_readiness(ExitStatus::from_raw(74 << 8), &lines);
        assert!(message.contains("migrated by a newer Foreseerr"));
        let message = exited_before_readiness(ExitStatus::from_raw(1 << 8), &lines);
        assert!(message.starts_with("Bundled Foreseerr exited before readiness"));
    }

    #[test]
    fn diagnostics_are_plain_redacted_and_bounded() {
        let mut diagnostics = Vec::new();
        push_diagnostic(
            &mut diagnostics,
            "2026-10-30T22:09:42.040Z [\u{1b}[32minfo\u{1b}[39m][Server]: Server ready",
        );
        assert_eq!(
            diagnostics,
            ["2026-10-30T22:09:42.040Z [info][Server]: Server ready"]
        );
        push_diagnostic(&mut diagnostics, "Cookie: connect.sid=secret");
        assert_eq!(diagnostics[1], "[redacted child diagnostic]");
        for line in 0..300 {
            push_diagnostic(&mut diagnostics, &line.to_string());
        }
        assert_eq!(diagnostics.len(), 200);
        assert_eq!(diagnostics[199], "299");
    }

    /// Starts a real runtime directory through the supervisor's own launch
    /// and shutdown code: `FORESEERR_RUNTIME_DIR=<dir> cargo test --
    /// --ignored bundled_runtime`. The runtime must be built from
    /// `foreseerr.rev` (`scripts/stage-foreseerr.sh` stages one into
    /// `resources/foreseerr`).
    #[test]
    #[ignore = "needs FORESEERR_RUNTIME_DIR pointing at a staged Foreseerr runtime"]
    fn bundled_runtime_starts_serves_and_stops_cleanly() {
        let runtime = PathBuf::from(
            std::env::var_os("FORESEERR_RUNTIME_DIR").expect("set FORESEERR_RUNTIME_DIR"),
        );
        assert!(is_runtime_directory(&runtime), "{}", runtime.display());
        let temporary = tempfile::tempdir().unwrap();
        let config = temporary.path().join("standalone");
        let cache = temporary.path().join("cache");
        let logs = config.join("logs");
        for directory in [&config.join("state"), &cache, &logs] {
            fs::create_dir_all(directory).unwrap();
        }
        let mut launched = launch(&ChildLaunch {
            runtime_dir: &runtime,
            config_dir: &config,
            cache_dir: &cache,
            log_dir: &logs,
            port: 0,
            cache_limit_bytes: 64 * 1024 * 1024,
        })
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            launched.ready.foreseerr_version,
            bundled_foreseerr_version()
        );
        assert_eq!(launched.ready.commit, bundled_foreseerr_revision());
        assert_eq!(launched.ready.pid, launched.child.process.id());
        let lock = config.join("state/instance.lock");
        assert_eq!(
            fs::read_to_string(&lock).unwrap().trim(),
            launched.child.process.id().to_string()
        );

        let status: serde_json::Value =
            ureq::get(&format!("{}/api/v1/status", launched.ready.origin))
                .call()
                .unwrap()
                .body_mut()
                .read_json()
                .unwrap();
        assert_eq!(status["runtime"], "desktop");
        assert_eq!(status["managed"], true);
        assert_eq!(status["version"], bundled_foreseerr_version());
        let index = ureq::get(&launched.ready.origin)
            .call()
            .unwrap()
            .body_mut()
            .read_to_string()
            .unwrap();
        assert!(index.contains("<html"), "the bundled frontend is served");
        launched
            .child
            .send_control(r#"{"type":"runtime-state","playbackActive":false}"#);

        let started = Instant::now();
        launched.child.shutdown();
        let exit = launched.child.process.try_wait().unwrap().expect("exited");
        assert!(exit.success(), "{exit}");
        assert!(
            started.elapsed() < SHUTDOWN_DEADLINE + SHUTDOWN_GRACE,
            "stopped through the control channel, not a signal"
        );
        assert!(!lock.exists(), "a graceful stop releases the instance lock");
        assert!(
            fs::metadata(config.join("db/db.sqlite3-wal")).map_or(true, |wal| wal.len() == 0),
            "a graceful stop checkpoints the WAL"
        );
        // Shutdown is idempotent once the child is gone.
        launched.child.shutdown();
    }
}
