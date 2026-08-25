//! Managed loopback Foreseerr child lifecycle.
//!
//! This module deliberately has no knowledge of CEF.  It owns only the bundled
//! Node process and exposes a validated, exact loopback origin to the caller.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, STILL_ACTIVE,
};

use crate::config::AppConfig;

pub const READY_PREFIX: &str = "FORESEERR_DESKTOP_READY ";
pub const READY_PROTOCOL_VERSION: u32 = 1;
const READY_TIMEOUT: Duration = Duration::from_secs(90);
const BUNDLED_FORESEERR_VERSION_FILE: &str = include_str!("../foreseerr.version");
const BUNDLED_FORESEERR_REVISION_FILE: &str = include_str!("../foreseerr.rev");
const CLEARED_CHILD_ENV: &[&str] = &[
    "PORT",
    "HOST",
    "CONFIG_DIRECTORY",
    "CACHE_DIRECTORY",
    "LOG_DIRECTORY",
    "NODE_OPTIONS",
    "NODE_PATH",
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
    child: Child,
    stdin: Option<ChildStdin>,
    #[cfg(windows)]
    job: WindowsJob,
    pub origin: String,
    pub diagnostics: Vec<String>,
    config: AppConfig,
    config_dir: PathBuf,
    ready: ReadyRecord,
    upgrade_backup: Option<PathBuf>,
    successful_status_recorded: bool,
}

/// Owns a Windows Job Object with kill-on-close semantics. Node may spawn
/// helpers for native modules, so terminating only the immediate Node process
/// is insufficient on Windows.
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

impl StandaloneSupervisor {
    pub fn start(config: &AppConfig) -> Result<Self, SupervisorError> {
        Self::start_on_port(config, 0)
    }

    fn start_on_port(config: &AppConfig, port: u16) -> Result<Self, SupervisorError> {
        let resource_root = resource_root()?;
        let node = resource_root
            .join("node")
            .join(if cfg!(windows) { "node.exe" } else { "node" });
        let launcher = resource_root.join("foreseerr").join("launcher.js");
        if !node.is_file() || !launcher.is_file() {
            return Err(SupervisorError::ResourcesNotFound(resource_root));
        }
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
        let mut command = Command::new(node);
        command
            .arg(launcher)
            .current_dir(resource_root.join("foreseerr"))
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
            // The staged bundle contains a production Next.js build and must
            // initialize its SQLite schema before loading migrated settings.
            .env("NODE_ENV", "production")
            .env("CONFIG_DIRECTORY", &config_dir)
            .env("CACHE_DIRECTORY", &cache_dir)
            .env("LOG_DIRECTORY", &log_dir)
            .env("HOST", "127.0.0.1")
            .env("PORT", port.to_string())
            .env("FORESEERR_COMMIT", bundled_foreseerr_revision())
            .env(
                "FORESEER_CACHE_LIMIT_BYTES",
                config.standalone.cache_limit_bytes.to_string(),
            );
        let mut child = command.spawn().map_err(SupervisorError::Spawn)?;
        #[cfg(windows)]
        let job = match WindowsJob::assign(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SupervisorError::Spawn(error));
            }
        };
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let (tx, rx) = mpsc::channel();
        let tx_stdout = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx_stdout.send((false, line.unwrap_or_default())).is_err() {
                    break;
                }
            }
        });
        let tx_stderr = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if tx_stderr.send((true, line.unwrap_or_default())).is_err() {
                    break;
                }
            }
        });
        let deadline = Instant::now() + READY_TIMEOUT;
        let mut diagnostics = Vec::new();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    cleanup_startup_child(
                        &mut child,
                        #[cfg(windows)]
                        &job,
                    );
                    return Err(SupervisorError::Startup(format!(
                        "Bundled Foreseerr exited before readiness ({status}); {}",
                        diagnostics.join("\n")
                    )));
                }
                Ok(None) => {}
                Err(error) => {
                    cleanup_startup_child(
                        &mut child,
                        #[cfg(windows)]
                        &job,
                    );
                    return Err(SupervisorError::Spawn(error));
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                cleanup_startup_child(
                    &mut child,
                    #[cfg(windows)]
                    &job,
                );
                return Err(SupervisorError::Startup(format!(
                    "Timed out waiting for bundled Foreseerr readiness; {}",
                    diagnostics.join("\n")
                )));
            }
            if let Ok((is_stderr, line)) =
                rx.recv_timeout(remaining.min(Duration::from_millis(250)))
            {
                if diagnostics.len() == 200 {
                    diagnostics.remove(0);
                }
                diagnostics.push(redact(&line));
                if !is_stderr && line.starts_with(READY_PREFIX) {
                    let ready: ReadyRecord = match serde_json::from_str(&line[READY_PREFIX.len()..])
                    {
                        Ok(ready) => ready,
                        Err(_) => {
                            cleanup_startup_child(
                                &mut child,
                                #[cfg(windows)]
                                &job,
                            );
                            return Err(SupervisorError::InvalidReady(
                                "Bundled Foreseerr emitted malformed readiness data".into(),
                            ));
                        }
                    };
                    if let Err(error) = validate_ready(&ready) {
                        cleanup_startup_child(
                            &mut child,
                            #[cfg(windows)]
                            &job,
                        );
                        return Err(error);
                    }
                    if ready.foreseerr_version != bundled_foreseerr_version() {
                        cleanup_startup_child(
                            &mut child,
                            #[cfg(windows)]
                            &job,
                        );
                        return Err(SupervisorError::InvalidReady(format!(
                            "Bundled Foreseerr version mismatch (expected {}, got {})",
                            bundled_foreseerr_version(),
                            ready.foreseerr_version
                        )));
                    }
                    let supervisor = Self {
                        stdin: child.stdin.take(),
                        child,
                        #[cfg(windows)]
                        job,
                        origin: ready.origin.clone(),
                        diagnostics,
                        config: config.clone(),
                        config_dir,
                        ready,
                        upgrade_backup,
                        successful_status_recorded: false,
                    };
                    // Keep draining stdout/stderr for the child's lifetime.
                    // Dropping `rx` here used to make the reader threads exit and
                    // close the pipes; the next Node write then raised EPIPE.
                    // Next.js source-maps that as uncaughtException, logs it to
                    // stderr, and livelocks the event loop so /login never
                    // renders.
                    drop(tx);
                    std::thread::spawn(move || {
                        while rx.recv().is_ok() {}
                    });
                    return Ok(supervisor);
                }
            }
        }
    }
    pub fn set_playback_active(&mut self, active: bool) {
        self.send_control(&format!(
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
        match self.child.try_wait() {
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
    pub fn shutdown(&mut self) {
        self.send_control(r#"{"type":"shutdown","deadlineMs":2000}"#);
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(self.child.id()) {
            // The child is the leader of a dedicated group, so this also
            // reaches Node helpers spawned for native modules.
            unsafe { libc::kill(-pid, libc::SIGTERM) };
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(self.child.id()) {
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
        #[cfg(windows)]
        self.job.terminate();
        #[cfg(all(not(unix), not(windows)))]
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
    fn send_control(&mut self, message: &str) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = writeln!(stdin, "{message}");
            let _ = stdin.flush();
        }
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

/// Reap a child that failed before it could become a `StandaloneSupervisor`.
/// On Unix the Node process is a process-group leader; signaling the group
/// avoids leaving native-module helpers behind after malformed readiness or a
/// timeout. Windows uses the kill-on-close job object for the same property.
fn cleanup_startup_child(child: &mut Child, #[cfg(windows)] job: &WindowsJob) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(child.id()) {
        unsafe { libc::kill(-pid, libc::SIGTERM) };
    }
    #[cfg(windows)]
    job.terminate();
    #[cfg(all(not(unix), not(windows)))]
    let _ = child.kill();

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(child.id()) {
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    #[cfg(windows)]
    job.terminate();
    #[cfg(all(not(unix), not(windows)))]
    let _ = child.kill();
    let _ = child.wait();
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
    queried && exit_code == STILL_ACTIVE
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
    fn managed_child_scrubs_hosted_database_and_node_overrides() {
        for variable in [
            "DB_TYPE",
            "DB_HOST",
            "DB_PORT",
            "DB_USER",
            "DB_PASS",
            "DB_NAME",
            "NODE_OPTIONS",
            "NODE_PATH",
        ] {
            assert!(CLEARED_CHILD_ENV.contains(&variable));
        }
    }
}
