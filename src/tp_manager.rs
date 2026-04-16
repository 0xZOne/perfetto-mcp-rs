// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use lru::LruCache;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{watch, Mutex, OnceCell};

use crate::download::DownloadConfig;
use crate::proto::StatusResult;
use crate::tp_client::TraceProcessorClient;

const STDERR_TAIL_CAPACITY: usize = 50;
const READY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const STATUS_FALLBACK_DELAY: Duration = Duration::from_millis(500);
const STATUS_FALLBACK_STABILITY: Duration = Duration::from_millis(300);

type SharedStderrTail = Arc<StdMutex<std::collections::VecDeque<String>>>;

#[derive(Debug, Clone, Copy)]
pub struct TraceProcessorConfig {
    pub startup_timeout: Duration,
    pub request_timeout: Duration,
}

impl Default for TraceProcessorConfig {
    fn default() -> Self {
        Self {
            startup_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum StartupState {
    #[default]
    Waiting,
    Ready,
    Ipv4BindFailed(String),
}

#[derive(Debug, Default)]
struct StartupLogState {
    saw_ipv4_start: bool,
    saw_ipv4_bind_failure: bool,
}

#[derive(Debug)]
enum WaitPhase {
    StderrGated,
    StatusFallback { ok_since: Option<Instant> },
}

/// A running trace_processor_shell instance bound to a specific trace file.
struct TraceProcessorInstance {
    process: Child,
    port: u16,
    client: TraceProcessorClient,
    stderr_tail: SharedStderrTail,
}

impl TraceProcessorInstance {
    /// Spawn trace_processor_shell in HTTP-RPC mode on the given port.
    async fn spawn(
        binary: &Path,
        trace_path: &Path,
        port: u16,
        config: TraceProcessorConfig,
    ) -> Result<Self> {
        let mut process = Command::new(binary)
            .arg("-D")
            .arg("--http-port")
            .arg(port.to_string())
            .arg(trace_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!(
                    "failed to spawn {} for {}",
                    binary.display(),
                    trace_path.display(),
                )
            })?;

        let stderr = process
            .stderr
            .take()
            .context("failed to capture trace_processor_shell stderr")?;
        let (mut startup_rx, stderr_tail) = spawn_stderr_drain(stderr, port);
        let client = TraceProcessorClient::new(port, config.request_timeout);

        let mut instance = Self {
            process,
            port,
            client,
            stderr_tail,
        };
        instance
            .wait_ready(trace_path, &mut startup_rx, config.startup_timeout)
            .await?;
        Ok(instance)
    }

    /// Poll the /status endpoint until the instance is ready.
    async fn wait_ready(
        &mut self,
        expected_trace: &Path,
        startup_rx: &mut watch::Receiver<StartupState>,
        startup_timeout: Duration,
    ) -> Result<()> {
        let client = self.client.clone();
        self.wait_ready_with_status(expected_trace, startup_rx, startup_timeout, || async {
            client.status().await
        })
        .await
    }

    async fn wait_ready_with_status<F, Fut>(
        &mut self,
        expected_trace: &Path,
        startup_rx: &mut watch::Receiver<StartupState>,
        startup_timeout: Duration,
        mut check_status: F,
    ) -> Result<()>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<StatusResult, crate::error::PerfettoError>>,
    {
        let start = Instant::now();
        let deadline = start + startup_timeout;
        let mut phase = WaitPhase::StderrGated;
        let mut emitted_wait_log = false;

        loop {
            if let Some(status) = self.process.try_wait()? {
                bail!(
                    "trace_processor_shell exited with {status} on port {}{}",
                    self.port,
                    format_stderr_tail(&self.stderr_tail),
                );
            }

            let startup_state = startup_rx.borrow().clone();
            match startup_state {
                StartupState::Waiting => {
                    if matches!(phase, WaitPhase::StderrGated)
                        && start.elapsed() >= STATUS_FALLBACK_DELAY
                    {
                        tracing::warn!(
                            "no recognized stderr readiness marker for trace_processor_shell on port {} after {:?}; falling back to /status + loaded_trace_name verification{}",
                            self.port,
                            STATUS_FALLBACK_DELAY,
                            format_stderr_tail(&self.stderr_tail),
                        );
                        phase = WaitPhase::StatusFallback { ok_since: None };
                    }

                    if let WaitPhase::StatusFallback { ok_since } = &mut phase {
                        match check_status().await {
                            Ok(status)
                                if status_matches_expected_trace(&status, expected_trace) =>
                            {
                                let first_ok = ok_since.get_or_insert_with(Instant::now);
                                if first_ok.elapsed() >= STATUS_FALLBACK_STABILITY {
                                    return Ok(());
                                }
                            }
                            Ok(_) | Err(_) => {
                                *ok_since = None;
                            }
                        }
                    }
                }
                StartupState::Ready => {
                    if check_status().await.is_ok() {
                        return Ok(());
                    }
                }
                StartupState::Ipv4BindFailed(line) => {
                    bail!(
                        "trace_processor_shell failed to bind 127.0.0.1:{}: {line}{}",
                        self.port,
                        format_stderr_tail(&self.stderr_tail),
                    );
                }
            }

            if Instant::now() >= deadline {
                bail!(
                    "trace_processor_shell on port {} did not become ready within {:?}{}",
                    self.port,
                    startup_timeout,
                    format_stderr_tail(&self.stderr_tail),
                );
            }

            if !emitted_wait_log && start.elapsed() >= startup_timeout / 2 {
                tracing::debug!(
                    "still waiting for trace_processor_shell on port {}",
                    self.port,
                );
                emitted_wait_log = true;
            }

            tokio::select! {
                changed = startup_rx.changed() => {
                    if changed.is_err() {
                        tokio::time::sleep(READY_POLL_INTERVAL).await;
                    }
                }
                _ = tokio::time::sleep(READY_POLL_INTERVAL) => {}
            }
        }
    }

    /// Check if the underlying process is still alive.
    fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        self.process
            .try_wait()
            .context("failed to poll child process")
    }
}

impl Drop for TraceProcessorInstance {
    fn drop(&mut self) {
        // kill_on_drop handles cleanup, but log for observability.
        tracing::debug!("dropping trace_processor_shell on port {}", self.port);
    }
}

/// Manages a pool of trace_processor_shell instances, one per trace file,
/// with LRU eviction when the pool exceeds `max_instances`.
pub struct TraceProcessorManager {
    inner: Mutex<ManagerInner>,
    spawn_locks: Mutex<std::collections::HashMap<PathBuf, Arc<Mutex<()>>>>,
    binary_path: OnceCell<PathBuf>,
    config: TraceProcessorConfig,
    download_config: DownloadConfig,
}

impl std::fmt::Debug for TraceProcessorManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraceProcessorManager")
            .field("binary_path", &self.binary_path.get())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

struct ManagerInner {
    instances: LruCache<PathBuf, TraceProcessorInstance>,
    next_port: u16,
    starting_port: u16,
}

impl TraceProcessorManager {
    pub const DEFAULT_STARTING_PORT: u16 = 9001;

    fn new_inner(
        max_instances: usize,
        starting_port: u16,
        config: TraceProcessorConfig,
        download_config: DownloadConfig,
    ) -> Self {
        let cap = NonZeroUsize::new(max_instances).unwrap_or(NonZeroUsize::MIN);
        Self {
            inner: Mutex::new(ManagerInner {
                instances: LruCache::new(cap),
                next_port: starting_port,
                starting_port,
            }),
            spawn_locks: Mutex::new(std::collections::HashMap::new()),
            binary_path: OnceCell::new(),
            config,
            download_config,
        }
    }

    pub fn new(max_instances: usize) -> Self {
        Self::new_inner(
            max_instances,
            Self::DEFAULT_STARTING_PORT,
            TraceProcessorConfig::default(),
            DownloadConfig::default(),
        )
    }

    pub fn new_with_configs(
        max_instances: usize,
        config: TraceProcessorConfig,
        download_config: DownloadConfig,
    ) -> Self {
        Self::new_inner(
            max_instances,
            Self::DEFAULT_STARTING_PORT,
            config,
            download_config,
        )
    }

    pub fn new_with_starting_port(max_instances: usize, starting_port: u16) -> Self {
        Self::new_inner(
            max_instances,
            starting_port,
            TraceProcessorConfig::default(),
            DownloadConfig::default(),
        )
    }

    pub fn new_with_starting_port_and_configs(
        max_instances: usize,
        starting_port: u16,
        config: TraceProcessorConfig,
        download_config: DownloadConfig,
    ) -> Self {
        Self::new_inner(max_instances, starting_port, config, download_config)
    }

    /// Create a manager with a pre-resolved binary path (tests only, avoids
    /// any download or PATH lookup).
    #[cfg(test)]
    pub fn new_with_binary(binary_path: PathBuf, max_instances: usize) -> Self {
        let this = Self::new(max_instances);
        this.binary_path
            .set(binary_path)
            .expect("binary_path not yet initialized");
        this
    }

    /// Resolve `trace_processor_shell`, downloading it on first call if needed.
    /// Errors are not cached, so a transient download failure can be retried.
    async fn ensure_binary(&self) -> Result<&Path> {
        let path = self
            .binary_path
            .get_or_try_init(|| async {
                let p = crate::download::ensure_binary(&self.download_config).await?;
                tracing::info!("using trace_processor_shell: {}", p.display());
                Ok::<PathBuf, anyhow::Error>(p)
            })
            .await?;
        Ok(path.as_path())
    }

    /// Get or create a `TraceProcessorClient` for the given trace file.
    ///
    /// If the instance already exists in the cache, it is returned (and
    /// promoted in LRU order). If the instance's process has died, it is
    /// respawned. If the cache is full, the least recently used instance
    /// is evicted (its process is killed via `kill_on_drop`).
    pub async fn get_client(&self, trace_path: &Path) -> Result<TraceProcessorClient> {
        let canonical = trace_path
            .canonicalize()
            .with_context(|| format!("trace file not found: {}", trace_path.display()))?;
        let binary = self.ensure_binary().await?.to_path_buf();
        self.get_or_spawn_instance(canonical, move |port, canonical| async move {
            TraceProcessorInstance::spawn(&binary, &canonical, port, self.config).await
        })
        .await
    }

    async fn get_or_spawn_instance<F, Fut>(
        &self,
        canonical: PathBuf,
        spawn: F,
    ) -> Result<TraceProcessorClient>
    where
        F: FnOnce(u16, PathBuf) -> Fut,
        Fut: std::future::Future<Output = Result<TraceProcessorInstance>>,
    {
        let path_lock = self.spawn_lock(canonical.clone()).await;
        let _path_guard = path_lock.lock().await;
        let result = self
            .get_or_spawn_instance_locked(canonical.clone(), spawn)
            .await;
        self.cleanup_spawn_lock(&canonical, &path_lock).await;
        result
    }

    async fn get_or_spawn_instance_locked<F, Fut>(
        &self,
        canonical: PathBuf,
        spawn: F,
    ) -> Result<TraceProcessorClient>
    where
        F: FnOnce(u16, PathBuf) -> Fut,
        Fut: std::future::Future<Output = Result<TraceProcessorInstance>>,
    {
        if let Some(client) = self.cached_client(&canonical).await? {
            return Ok(client);
        }

        let port = {
            let mut inner = self.inner.lock().await;
            allocate_next_port(&mut inner)?
        };

        let instance = spawn(port, canonical.clone()).await?;
        let client = instance.client.clone();

        let mut inner = self.inner.lock().await;
        if let Some(existing) = inner.instances.get(&canonical) {
            return Ok(existing.client.clone());
        }
        inner.instances.put(canonical, instance);
        Ok(client)
    }

    async fn cached_client(&self, canonical: &Path) -> Result<Option<TraceProcessorClient>> {
        let mut inner = self.inner.lock().await;
        if let Some(inst) = inner.instances.get_mut(canonical) {
            match inst.try_wait()? {
                None => return Ok(Some(inst.client.clone())),
                Some(status) => {
                    tracing::warn!(
                        "trace_processor_shell on port {} exited with {status}; respawning{}",
                        inst.port,
                        format_stderr_tail(&inst.stderr_tail),
                    );
                    inner.instances.pop(canonical);
                }
            }
        }
        Ok(None)
    }

    async fn spawn_lock(&self, canonical: PathBuf) -> Arc<Mutex<()>> {
        let mut locks = self.spawn_locks.lock().await;
        locks
            .entry(canonical)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn cleanup_spawn_lock(&self, canonical: &Path, path_lock: &Arc<Mutex<()>>) {
        let mut locks = self.spawn_locks.lock().await;
        if locks.get(canonical).is_some_and(|existing| {
            Arc::ptr_eq(existing, path_lock) && Arc::strong_count(existing) == 2
        }) {
            locks.remove(canonical);
        }
    }
}

fn allocate_next_port(inner: &mut ManagerInner) -> Result<u16> {
    allocate_next_port_with_probe(inner, preflight_port_free)
}

/// Sweep the full port range starting from `inner.next_port`, returning the
/// first port where `probe` succeeds. Bails if the entire range is occupied
/// — the alternative (returning a port we just rejected) would defer the
/// same failure into `wait_ready` and report it as a confusing bind error.
fn allocate_next_port_with_probe<F>(inner: &mut ManagerInner, mut probe: F) -> Result<u16>
where
    F: FnMut(u16) -> bool,
{
    let sweep = u16::MAX as u32 - inner.starting_port as u32 + 1;
    for _ in 0..sweep {
        let port = advance_next_port(inner);
        if probe(port) {
            return Ok(port);
        }
    }
    bail!("no free port found in {}..=65535", inner.starting_port)
}

fn advance_next_port(inner: &mut ManagerInner) -> u16 {
    let port = inner.next_port;
    inner.next_port = inner.next_port.wrapping_add(1);
    if inner.next_port < inner.starting_port {
        inner.next_port = inner.starting_port;
    }
    port
}

/// Best-effort probe: if we can bind the port right now, it is (probably)
/// free. The listener is dropped immediately, leaving a microsecond-wide
/// TOCTOU window before the child spawns — small enough for our purposes.
fn preflight_port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn spawn_stderr_drain(
    stderr: tokio::process::ChildStderr,
    port: u16,
) -> (watch::Receiver<StartupState>, SharedStderrTail) {
    let (startup_tx, startup_rx) = watch::channel(StartupState::Waiting);
    let stderr_tail = Arc::new(StdMutex::new(std::collections::VecDeque::with_capacity(
        STDERR_TAIL_CAPACITY,
    )));
    let stderr_tail_task = Arc::clone(&stderr_tail);

    tokio::spawn(async move {
        let needles = StartupNeedles::for_port(port);
        let mut startup_state = StartupLogState::default();
        // Stops allocating on every stderr line once startup resolves.
        let mut parse_startup = true;
        let mut lines = BufReader::new(stderr).lines();

        loop {
            match lines.next_line().await {
                Ok(Some(raw_line)) => {
                    let line = raw_line.trim_end_matches('\r').to_owned();
                    tracing::info!("trace_processor_shell[{port}] stderr: {line}");
                    push_stderr_line(&stderr_tail_task, &line);

                    if parse_startup {
                        if let Some(next_state) =
                            update_startup_state(&mut startup_state, &needles, &line)
                        {
                            let _ = startup_tx.send(next_state);
                            parse_startup = false;
                        }
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    tracing::warn!("failed reading trace_processor_shell[{port}] stderr: {err}");
                    push_stderr_line(&stderr_tail_task, &format!("stderr read error: {err}"));
                    break;
                }
            }
        }
    });

    (startup_rx, stderr_tail)
}

struct StartupNeedles {
    ipv4_start: String,
    ipv4_bound: String,
}

impl StartupNeedles {
    fn for_port(port: u16) -> Self {
        Self {
            ipv4_start: format!("[HTTP] Starting HTTP server on 127.0.0.1:{port}"),
            ipv4_bound: format!("127.0.0.1:{port}"),
        }
    }
}

fn update_startup_state(
    state: &mut StartupLogState,
    needles: &StartupNeedles,
    line: &str,
) -> Option<StartupState> {
    if line.contains(&needles.ipv4_start) {
        state.saw_ipv4_start = true;
    }

    if line.contains("Failed to listen on IPv4 socket") && line.contains(&needles.ipv4_bound) {
        state.saw_ipv4_bind_failure = true;
        return Some(StartupState::Ipv4BindFailed(line.to_owned()));
    }

    if state.saw_ipv4_bind_failure {
        return None;
    }

    if state.saw_ipv4_start && line.contains("[HTTP] This server can be used") {
        return Some(StartupState::Ready);
    }

    None
}

fn status_matches_expected_trace(status: &StatusResult, expected_trace: &Path) -> bool {
    let Some(loaded_trace_name) = status.loaded_trace_name.as_deref() else {
        // Some trace_processor_shell builds leave `loaded_trace_name` unset.
        // The allocator preflights every port right before spawn, so a
        // successful /status response on a port that was free microseconds
        // ago is almost certainly from our child. Trust it.
        return true;
    };
    loaded_name_matches(loaded_trace_name, expected_trace)
}

/// Does `/status`'s `loaded_trace_name` refer to the trace at `expected`?
/// Matches after stripping Perfetto's `" (NN MB)"` annotation and
/// normalizing `\` → `/`; also accepts a bare filename match.
pub(crate) fn loaded_name_matches(loaded: &str, expected: &Path) -> bool {
    let loaded_norm = normalize_status_path(strip_size_suffix(loaded));
    let expected_norm = normalize_status_path(&expected.to_string_lossy());
    if loaded_norm == expected_norm {
        return true;
    }
    expected
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| loaded_norm == name)
}

fn normalize_status_path(path: &str) -> String {
    path.replace('\\', "/")
}

/// Strip Perfetto's trailing `" (NN MB)"` size annotation so exact
/// equality works against our canonical trace path.
pub(crate) fn strip_size_suffix(loaded: &str) -> &str {
    if !loaded.ends_with(')') {
        return loaded;
    }
    match loaded.rfind(" (") {
        Some(idx) => &loaded[..idx],
        None => loaded,
    }
}

fn push_stderr_line(stderr_tail: &SharedStderrTail, line: &str) {
    let mut tail = stderr_tail.lock().expect("stderr tail poisoned");
    if tail.len() == STDERR_TAIL_CAPACITY {
        tail.pop_front();
    }
    tail.push_back(line.to_owned());
}

fn format_stderr_tail(stderr_tail: &SharedStderrTail) -> String {
    let tail = stderr_tail.lock().expect("stderr tail poisoned");
    if tail.is_empty() {
        String::new()
    } else {
        format!(
            "\nstderr tail:\n{}",
            tail.iter().cloned().collect::<Vec<_>>().join("\n")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::process::Command as TokioCommand;

    #[test]
    fn lru_evicts_oldest_when_full() {
        let mut cache: LruCache<String, u16> = LruCache::new(NonZeroUsize::new(2).unwrap());
        cache.put("a".into(), 1);
        cache.put("b".into(), 2);
        cache.put("c".into(), 3);

        assert!(cache.get(&"a".to_string()).is_none(), "a should be evicted");
        assert!(cache.get(&"b".to_string()).is_some());
        assert!(cache.get(&"c".to_string()).is_some());
    }

    #[test]
    fn lru_access_refreshes_entry() {
        let mut cache: LruCache<String, u16> = LruCache::new(NonZeroUsize::new(2).unwrap());
        cache.put("a".into(), 1);
        cache.put("b".into(), 2);
        // Access "a" to refresh it.
        let _ = cache.get(&"a".to_string());
        // Insert "c" — should evict "b" (oldest unreferenced).
        cache.put("c".into(), 3);

        assert!(cache.get(&"a".to_string()).is_some(), "a was refreshed");
        assert!(cache.get(&"b".to_string()).is_none(), "b should be evicted");
        assert!(cache.get(&"c".to_string()).is_some());
    }

    #[test]
    fn allocate_next_port_wraps_back_to_starting_port() {
        let mut inner = ManagerInner {
            instances: LruCache::new(NonZeroUsize::new(1).unwrap()),
            next_port: u16::MAX,
            starting_port: 19_001,
        };

        assert_eq!(
            allocate_next_port_with_probe(&mut inner, |_| true).unwrap(),
            u16::MAX,
        );
        assert_eq!(inner.next_port, 19_001);
    }

    #[test]
    fn allocate_next_port_skips_occupied_ports_via_probe() {
        let mut inner = ManagerInner {
            instances: LruCache::new(NonZeroUsize::new(1).unwrap()),
            next_port: 20_000,
            starting_port: 20_000,
        };

        let occupied = [20_000u16, 20_001u16];
        let probe = |port: u16| !occupied.contains(&port);
        assert_eq!(
            allocate_next_port_with_probe(&mut inner, probe).unwrap(),
            20_002
        );
        assert_eq!(inner.next_port, 20_003);
    }

    #[test]
    fn allocate_next_port_bails_when_all_probes_fail() {
        // starting_port near u16::MAX keeps the full-range sweep cheap in test.
        let mut inner = ManagerInner {
            instances: LruCache::new(NonZeroUsize::new(1).unwrap()),
            next_port: 65_530,
            starting_port: 65_530,
        };

        let err = allocate_next_port_with_probe(&mut inner, |_| false)
            .expect_err("exhausted sweep must surface a clear error");
        assert!(
            err.to_string().contains("no free port"),
            "error message should explain the exhaustion, got: {err}",
        );
    }

    #[test]
    fn preflight_port_free_rejects_real_bound_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral listener");
        let port = listener.local_addr().expect("local addr").port();
        assert!(
            !preflight_port_free(port),
            "actively bound port {port} must probe as occupied",
        );
    }

    #[test]
    fn allocate_next_port_skips_real_bound_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral listener");
        let bound_port = listener.local_addr().expect("local addr").port();

        let mut inner = ManagerInner {
            instances: LruCache::new(NonZeroUsize::new(1).unwrap()),
            next_port: bound_port,
            starting_port: bound_port,
        };
        let allocated = allocate_next_port_with_probe(&mut inner, preflight_port_free)
            .expect("allocator should sweep past a real bound port");
        assert_ne!(
            allocated, bound_port,
            "allocator must not hand out a port that is actively bound",
        );
    }

    #[test]
    fn status_matches_rejects_suffix_collision() {
        let status = status_result(Some("/tmp/foo.perfetto-trace.1 (0 MB)"));
        let expected = PathBuf::from("/tmp/foo.perfetto-trace");
        assert!(
            !status_matches_expected_trace(&status, &expected),
            "a longer trace name must not match a shorter expected path",
        );
    }

    #[test]
    fn status_matches_accepts_missing_name() {
        let status = status_result(None);
        let expected = PathBuf::from("/tmp/foo.perfetto-trace");
        assert!(status_matches_expected_trace(&status, &expected));
    }

    #[test]
    fn status_matches_accepts_bare_basename() {
        let status = status_result(Some("foo.perfetto-trace (0 MB)"));
        let expected = PathBuf::from("/abs/path/to/foo.perfetto-trace");
        assert!(status_matches_expected_trace(&status, &expected));
    }

    #[test]
    fn strip_size_suffix_removes_trailing_annotation() {
        assert_eq!(
            strip_size_suffix("/tmp/trace.perfetto-trace (0 MB)"),
            "/tmp/trace.perfetto-trace",
        );
        assert_eq!(
            strip_size_suffix("/tmp/trace.perfetto-trace (123 MB)"),
            "/tmp/trace.perfetto-trace",
        );
    }

    #[test]
    fn strip_size_suffix_passes_through_when_no_annotation() {
        assert_eq!(
            strip_size_suffix("/tmp/trace.perfetto-trace"),
            "/tmp/trace.perfetto-trace",
        );
    }

    #[test]
    fn strip_size_suffix_uses_rightmost_boundary() {
        // A user's trace path can legitimately contain " (" — the rightmost
        // occurrence is the one Perfetto appended, so only strip that.
        assert_eq!(
            strip_size_suffix("/tmp/weird (old).perfetto-trace (0 MB)"),
            "/tmp/weird (old).perfetto-trace",
        );
    }

    #[test]
    fn strip_size_suffix_requires_trailing_paren() {
        // Has " (" but does not end in ")" — treat as no suffix.
        assert_eq!(strip_size_suffix("foo (oops"), "foo (oops");
    }

    #[test]
    fn new_with_starting_port_and_configs_wires_all_fields() {
        let tp_config = TraceProcessorConfig {
            startup_timeout: Duration::from_millis(4_321),
            request_timeout: Duration::from_millis(7_654),
        };
        let download_config =
            DownloadConfig::from_override(Some("https://mirror.example/tp".to_string()));

        let manager = TraceProcessorManager::new_with_starting_port_and_configs(
            5,
            19_500,
            tp_config,
            download_config,
        );

        assert_eq!(manager.config.startup_timeout, Duration::from_millis(4_321));
        assert_eq!(manager.config.request_timeout, Duration::from_millis(7_654));
        assert_eq!(
            manager.download_config.redacted_base_url(),
            "https://mirror.example/tp"
        );

        let inner = manager
            .inner
            .try_lock()
            .expect("freshly built manager is uncontended");
        assert_eq!(inner.starting_port, 19_500);
        assert_eq!(inner.next_port, 19_500);
        assert_eq!(inner.instances.cap().get(), 5);
    }

    #[tokio::test]
    async fn wait_ready_blocks_status_until_stderr_gate_opens() {
        let mut instance = fake_instance(19_111);
        let expected_trace = expected_trace_path(19_111);
        let expected_trace_for_status = expected_trace.clone();
        let (startup_tx, mut startup_rx) = watch::channel(StartupState::Waiting);
        let status_calls = Arc::new(AtomicUsize::new(0));
        let status_calls_task = Arc::clone(&status_calls);

        let wait = tokio::spawn(async move {
            instance
                .wait_ready_with_status(
                    &expected_trace,
                    &mut startup_rx,
                    Duration::from_millis(400),
                    move || {
                        let status_calls = Arc::clone(&status_calls_task);
                        let expected_trace = expected_trace_for_status.clone();
                        async move {
                            status_calls.fetch_add(1, Ordering::SeqCst);
                            Ok(status_for_trace(&expected_trace))
                        }
                    },
                )
                .await
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            status_calls.load(Ordering::SeqCst),
            0,
            "status polling must not start before our stderr readiness marker",
        );

        startup_tx
            .send(StartupState::Ready)
            .expect("send startup ready");

        wait.await.expect("join wait_ready").expect("wait_ready ok");
        assert!(
            status_calls.load(Ordering::SeqCst) >= 1,
            "status should be polled once the readiness gate opens",
        );
    }

    #[tokio::test]
    async fn wait_ready_fails_on_ipv4_bind_error_without_polling_status() {
        let mut instance = fake_instance(19_112);
        let expected_trace = expected_trace_path(19_112);
        let expected_trace_for_status = expected_trace.clone();
        push_stderr_line(
            &instance.stderr_tail,
            "[HTTP] Starting HTTP server on 127.0.0.1:19112",
        );
        push_stderr_line(
            &instance.stderr_tail,
            "[HTTP] Failed to listen on IPv4 socket: \"127.0.0.1:19112\"",
        );
        let (_startup_tx, mut startup_rx) = watch::channel(StartupState::Ipv4BindFailed(
            "Failed to listen on IPv4 socket: \"127.0.0.1:19112\"".to_owned(),
        ));
        let status_calls = Arc::new(AtomicUsize::new(0));
        let status_calls_task = Arc::clone(&status_calls);

        let err = instance
            .wait_ready_with_status(
                &expected_trace,
                &mut startup_rx,
                Duration::from_millis(200),
                move || {
                    let status_calls = Arc::clone(&status_calls_task);
                    let expected_trace = expected_trace_for_status.clone();
                    async move {
                        status_calls.fetch_add(1, Ordering::SeqCst);
                        Ok(status_for_trace(&expected_trace))
                    }
                },
            )
            .await
            .expect_err("bind failure must abort startup");

        assert_eq!(
            status_calls.load(Ordering::SeqCst),
            0,
            "foreign /status must not be consulted after our own bind failure",
        );
        assert!(
            err.to_string().contains("failed to bind 127.0.0.1:19112"),
            "error should surface the bind failure, got: {err}",
        );
    }

    #[tokio::test]
    async fn wait_ready_falls_back_to_status_for_unrecognized_external_binary_output() {
        let mut instance = fake_instance(19_114);
        let expected_trace = expected_trace_path(19_114);
        let expected_trace_for_status = expected_trace.clone();
        let (_startup_tx, mut startup_rx) = watch::channel(StartupState::Waiting);
        let status_calls = Arc::new(AtomicUsize::new(0));
        let status_calls_task = Arc::clone(&status_calls);

        instance
            .wait_ready_with_status(
                &expected_trace,
                &mut startup_rx,
                Duration::from_millis(1_500),
                move || {
                    let status_calls = Arc::clone(&status_calls_task);
                    let expected_trace = expected_trace_for_status.clone();
                    async move {
                        status_calls.fetch_add(1, Ordering::SeqCst);
                        Ok(status_for_trace(&expected_trace))
                    }
                },
            )
            .await
            .expect("fallback /status should keep external binaries working");

        assert!(
            status_calls.load(Ordering::SeqCst) >= 2,
            "fallback path should require sustained /status health before succeeding",
        );
    }

    #[tokio::test]
    async fn wait_ready_fallback_stability_window_resets_on_failure() {
        let mut instance = fake_instance(19_116);
        let expected_trace = expected_trace_path(19_116);
        let expected_trace_for_status = expected_trace.clone();
        let (_startup_tx, mut startup_rx) = watch::channel(StartupState::Waiting);

        // Sequence of /status results once the fallback path opens.
        // The first Ok starts the stability timer; the Err that follows
        // must clear it so the timer cannot "carry over" pre-failure time.
        let results = Arc::new(StdMutex::new(std::collections::VecDeque::from([
            Ok(status_for_trace(&expected_trace)),
            Err(crate::error::PerfettoError::QueryError {
                kind: crate::error::QueryErrorKind::Other,
                message: "simulated transient /status failure".to_owned(),
            }),
            Ok(status_for_trace(&expected_trace)),
            Ok(status_for_trace(&expected_trace)),
            Ok(status_for_trace(&expected_trace)),
            Ok(status_for_trace(&expected_trace)),
            Ok(status_for_trace(&expected_trace)),
            Ok(status_for_trace(&expected_trace)),
        ])));
        let results_task = Arc::clone(&results);
        let ok_timestamps = Arc::new(StdMutex::new(Vec::<Instant>::new()));
        let ok_timestamps_task = Arc::clone(&ok_timestamps);

        let ready_at = Instant::now();
        instance
            .wait_ready_with_status(
                &expected_trace,
                &mut startup_rx,
                Duration::from_millis(2_500),
                move || {
                    let results = Arc::clone(&results_task);
                    let ok_timestamps = Arc::clone(&ok_timestamps_task);
                    let expected_trace = expected_trace_for_status.clone();
                    async move {
                        let next = results
                            .lock()
                            .unwrap()
                            .pop_front()
                            .unwrap_or_else(|| Ok(status_for_trace(&expected_trace)));
                        if next.is_ok() {
                            ok_timestamps.lock().unwrap().push(Instant::now());
                        }
                        next
                    }
                },
            )
            .await
            .expect("fallback must eventually succeed after stability re-accumulates");
        let total_elapsed = ready_at.elapsed();

        // The stability timer must not have latched onto the pre-failure
        // Ok: success requires STATUS_FALLBACK_STABILITY (300ms) of
        // *sustained* Ok after the failure, on top of the 500ms fallback
        // delay. Allow slack for the poll interval and scheduler jitter.
        assert!(
            total_elapsed
                >= STATUS_FALLBACK_DELAY + STATUS_FALLBACK_STABILITY - Duration::from_millis(50),
            "stability window must re-accumulate after a failure; elapsed={total_elapsed:?}",
        );

        // And the final Ok run must itself span the stability window,
        // meaning at least two post-failure Ok samples that are
        // STATUS_FALLBACK_STABILITY apart.
        let timestamps = ok_timestamps.lock().unwrap().clone();
        assert!(
            timestamps.len() >= 3,
            "expected at least pre-fail Ok + two post-fail Oks, got {}",
            timestamps.len(),
        );
        let post_fail = &timestamps[1..];
        let spanned = post_fail
            .last()
            .unwrap()
            .duration_since(*post_fail.first().unwrap());
        assert!(
            spanned >= STATUS_FALLBACK_STABILITY - Duration::from_millis(50),
            "post-failure Ok streak must span the stability window; spanned={spanned:?}",
        );
    }

    #[tokio::test]
    async fn wait_ready_fallback_rejects_foreign_trace_status() {
        let mut instance = fake_instance(19_117);
        let expected_trace = expected_trace_path(19_117);
        let foreign_trace = PathBuf::from("/tmp/foreign-trace.perfetto-trace");
        let (_startup_tx, mut startup_rx) = watch::channel(StartupState::Waiting);
        let status_calls = Arc::new(AtomicUsize::new(0));
        let status_calls_task = Arc::clone(&status_calls);

        let err = instance
            .wait_ready_with_status(
                &expected_trace,
                &mut startup_rx,
                Duration::from_millis(1_200),
                move || {
                    let status_calls = Arc::clone(&status_calls_task);
                    let foreign_trace = foreign_trace.clone();
                    async move {
                        status_calls.fetch_add(1, Ordering::SeqCst);
                        Ok(status_for_trace(&foreign_trace))
                    }
                },
            )
            .await
            .expect_err("fallback must not accept a foreign trace identity");

        assert!(
            err.to_string().contains("did not become ready"),
            "expected timeout for foreign trace identity, got: {err}",
        );
        assert!(
            status_calls.load(Ordering::SeqCst) >= 2,
            "fallback should have polled status repeatedly before timing out",
        );
    }

    #[tokio::test]
    async fn wait_ready_fallback_rejects_suffix_collision() {
        let mut instance = fake_instance(19_118);
        let expected_trace = PathBuf::from("/tmp/perfetto-test-19118.perfetto-trace");
        let collision = status_result(Some("/tmp/perfetto-test-19118.perfetto-trace.1 (0 MB)"));
        let (_startup_tx, mut startup_rx) = watch::channel(StartupState::Waiting);

        let err = instance
            .wait_ready_with_status(
                &expected_trace,
                &mut startup_rx,
                Duration::from_millis(1_200),
                move || {
                    let collision = collision.clone();
                    async move { Ok(collision) }
                },
            )
            .await
            .expect_err("suffix-extended foreign path must not satisfy identity");
        assert!(
            err.to_string().contains("did not become ready"),
            "expected timeout for suffix-collision status, got: {err}",
        );
    }

    #[tokio::test]
    async fn wait_ready_fallback_accepts_missing_trace_name() {
        let mut instance = fake_instance(19_119);
        let expected_trace = expected_trace_path(19_119);
        let (_startup_tx, mut startup_rx) = watch::channel(StartupState::Waiting);

        instance
            .wait_ready_with_status(
                &expected_trace,
                &mut startup_rx,
                Duration::from_millis(2_500),
                || async { Ok(status_result(None)) },
            )
            .await
            .expect("external binary with missing loaded_trace_name must pass");
    }

    #[tokio::test]
    async fn concurrent_same_trace_requests_only_spawn_once() {
        let manager = Arc::new(TraceProcessorManager::new(2));
        let canonical = PathBuf::from("/tmp/fake-trace.perfetto-trace");
        let spawn_count = Arc::new(AtomicUsize::new(0));

        let task1 = {
            let manager = Arc::clone(&manager);
            let canonical = canonical.clone();
            let spawn_count = Arc::clone(&spawn_count);
            tokio::spawn(async move {
                manager
                    .get_or_spawn_instance(canonical, move |port, _| async move {
                        spawn_count.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        Ok(fake_instance(port))
                    })
                    .await
            })
        };

        let task2 = {
            let manager = Arc::clone(&manager);
            let canonical = canonical.clone();
            let spawn_count = Arc::clone(&spawn_count);
            tokio::spawn(async move {
                manager
                    .get_or_spawn_instance(canonical, move |port, _| async move {
                        spawn_count.fetch_add(1, Ordering::SeqCst);
                        Ok(fake_instance(port))
                    })
                    .await
            })
        };

        task1.await.expect("join task1").expect("client1");
        task2.await.expect("join task2").expect("client2");

        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            1,
            "same trace should use a single in-flight spawn",
        );
    }

    #[tokio::test]
    async fn concurrent_different_trace_requests_spawn_independently() {
        let manager = Arc::new(TraceProcessorManager::new(4));
        let spawn_count = Arc::new(AtomicUsize::new(0));

        let task_a = {
            let manager = Arc::clone(&manager);
            let spawn_count = Arc::clone(&spawn_count);
            tokio::spawn(async move {
                manager
                    .get_or_spawn_instance(
                        PathBuf::from("/tmp/concurrent-a.perfetto-trace"),
                        move |port, _| async move {
                            spawn_count.fetch_add(1, Ordering::SeqCst);
                            Ok(fake_instance(port))
                        },
                    )
                    .await
            })
        };

        let task_b = {
            let manager = Arc::clone(&manager);
            let spawn_count = Arc::clone(&spawn_count);
            tokio::spawn(async move {
                manager
                    .get_or_spawn_instance(
                        PathBuf::from("/tmp/concurrent-b.perfetto-trace"),
                        move |port, _| async move {
                            spawn_count.fetch_add(1, Ordering::SeqCst);
                            Ok(fake_instance(port))
                        },
                    )
                    .await
            })
        };

        task_a.await.expect("join task_a").expect("client a");
        task_b.await.expect("join task_b").expect("client b");

        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            2,
            "different traces must spawn independently and not share locks",
        );
    }

    #[tokio::test]
    async fn manager_evicts_oldest_instance_when_capacity_exceeded() {
        let manager = Arc::new(TraceProcessorManager::new(2));
        let trace_a = PathBuf::from("/tmp/lru-a.perfetto-trace");
        let trace_b = PathBuf::from("/tmp/lru-b.perfetto-trace");
        let trace_c = PathBuf::from("/tmp/lru-c.perfetto-trace");
        let spawn_count = Arc::new(AtomicUsize::new(0));

        for path in [&trace_a, &trace_b, &trace_c] {
            let counter = Arc::clone(&spawn_count);
            manager
                .get_or_spawn_instance(path.clone(), move |port, _| async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(fake_instance(port))
                })
                .await
                .expect("initial spawn");
        }
        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            3,
            "three distinct traces must trigger three spawns",
        );

        // trace_a was the LRU entry; re-fetching it must respawn.
        {
            let counter = Arc::clone(&spawn_count);
            manager
                .get_or_spawn_instance(trace_a, move |port, _| async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(fake_instance(port))
                })
                .await
                .expect("respawn evicted trace");
        }
        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            4,
            "evicted instance must respawn on next request",
        );

        // trace_c was inserted most recently and is still cached.
        {
            let counter = Arc::clone(&spawn_count);
            manager
                .get_or_spawn_instance(trace_c, move |port, _| async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(fake_instance(port))
                })
                .await
                .expect("cached client");
        }
        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            4,
            "cached instance must reuse, not respawn",
        );
    }

    #[tokio::test]
    async fn get_or_spawn_instance_recovers_after_process_death() {
        let manager = Arc::new(TraceProcessorManager::new(2));
        let canonical = PathBuf::from("/tmp/auto-recovery-trace.perfetto-trace");
        let spawn_count = Arc::new(AtomicUsize::new(0));

        // First spawn: insert a process that exits immediately.
        {
            let counter = Arc::clone(&spawn_count);
            manager
                .get_or_spawn_instance(canonical.clone(), move |port, _| async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(fake_instance_with_process(port, spawn_quick_exit_process()))
                })
                .await
                .expect("initial spawn");
        }

        // Give the kernel a moment to reap the exited child so try_wait observes it.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Second call: cached_client must detect the dead process and respawn.
        {
            let counter = Arc::clone(&spawn_count);
            manager
                .get_or_spawn_instance(canonical, move |port, _| async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(fake_instance(port))
                })
                .await
                .expect("auto-recovery respawn");
        }

        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            2,
            "dead cached instance must trigger a respawn on next request",
        );
    }

    #[tokio::test]
    async fn get_client_returns_clear_error_for_missing_trace() {
        let manager = TraceProcessorManager::new_with_binary(
            PathBuf::from("/nonexistent/trace_processor_shell"),
            1,
        );
        let missing = PathBuf::from("/nonexistent/this-trace-does-not-exist.perfetto-trace");

        let err = manager
            .get_client(&missing)
            .await
            .expect_err("missing trace must error");
        let msg = err.to_string();
        assert!(
            msg.contains("trace file not found"),
            "error should call out the missing trace, got: {msg}",
        );
        assert!(
            msg.contains("this-trace-does-not-exist.perfetto-trace"),
            "error should include the trace path, got: {msg}",
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn get_client_surfaces_spawn_error_for_non_executable_binary() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let non_exec = tmp.path().join("fake_tp_shell");
        std::fs::write(&non_exec, b"fake").expect("write fake binary");
        std::fs::set_permissions(&non_exec, std::fs::Permissions::from_mode(0o644))
            .expect("strip execute bit");

        let trace = tmp.path().join("fake.perfetto-trace");
        std::fs::write(&trace, b"not a real trace").expect("write trace placeholder");

        let manager = TraceProcessorManager::new_with_binary(non_exec.clone(), 1);
        let err = manager
            .get_client(&trace)
            .await
            .expect_err("non-executable binary must fail at spawn");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("failed to spawn"),
            "error should surface the spawn-failure context, got: {msg}",
        );
        assert!(
            msg.contains("fake_tp_shell"),
            "error should include the binary name, got: {msg}",
        );
        assert!(
            msg.to_lowercase().contains("permission denied"),
            "error chain should surface the OS-level permission denial, got: {msg}",
        );
    }

    #[test]
    fn startup_parser_never_overrides_ipv4_bind_failure_with_ready() {
        let mut state = StartupLogState::default();
        let needles = StartupNeedles::for_port(19_113);

        assert_eq!(
            update_startup_state(
                &mut state,
                &needles,
                "[HTTP] Starting HTTP server on 127.0.0.1:19113",
            ),
            None,
        );
        assert_eq!(
            update_startup_state(
                &mut state,
                &needles,
                "Failed to listen on IPv4 socket: \"127.0.0.1:19113\" (errno: 98, Address already in use)",
            ),
            Some(StartupState::Ipv4BindFailed(
                "Failed to listen on IPv4 socket: \"127.0.0.1:19113\" (errno: 98, Address already in use)"
                    .to_owned(),
            )),
        );
        assert_eq!(
            update_startup_state(
                &mut state,
                &needles,
                "[HTTP] This server can be used by reloading https://ui.perfetto.dev",
            ),
            None,
            "ready banner must not erase a prior bind failure",
        );
    }

    #[tokio::test]
    async fn wait_ready_fails_if_process_exits_before_ready() {
        let mut instance = fake_instance_with_process(19_115, spawn_quick_exit_process());
        let expected_trace = expected_trace_path(19_115);
        let expected_trace_for_status = expected_trace.clone();
        let (_startup_tx, mut startup_rx) = watch::channel(StartupState::Waiting);

        let err = instance
            .wait_ready_with_status(
                &expected_trace,
                &mut startup_rx,
                Duration::from_millis(500),
                move || {
                    let expected_trace = expected_trace_for_status.clone();
                    async move { Ok(status_for_trace(&expected_trace)) }
                },
            )
            .await
            .expect_err("exited child must fail startup");

        assert!(
            err.to_string().contains("exited with"),
            "expected early-exit failure, got: {err}",
        );
    }

    fn spawn_hold_process() -> Child {
        // Windows `timeout` refuses redirected stdin; use `ping` as a tty-less sleep.
        #[cfg(windows)]
        let mut cmd = {
            let mut cmd = TokioCommand::new("ping");
            cmd.args(["-n", "31", "127.0.0.1"]);
            cmd
        };

        #[cfg(not(windows))]
        let mut cmd = {
            let mut cmd = TokioCommand::new("sleep");
            cmd.arg("30");
            cmd
        };

        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn hold process")
    }

    fn spawn_quick_exit_process() -> Child {
        #[cfg(windows)]
        let mut cmd = {
            let mut cmd = TokioCommand::new("cmd");
            cmd.args(["/C", "exit", "0"]);
            cmd
        };

        #[cfg(not(windows))]
        let mut cmd = TokioCommand::new("true");

        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn quick-exit process")
    }

    fn fake_instance(port: u16) -> TraceProcessorInstance {
        fake_instance_with_process(port, spawn_hold_process())
    }

    fn fake_instance_with_process(port: u16, process: Child) -> TraceProcessorInstance {
        TraceProcessorInstance {
            process,
            port,
            client: TraceProcessorClient::new(port, Duration::from_secs(1)),
            stderr_tail: Arc::new(StdMutex::new(std::collections::VecDeque::new())),
        }
    }

    fn expected_trace_path(port: u16) -> PathBuf {
        PathBuf::from(format!("/tmp/test-trace-{port}.perfetto-trace"))
    }

    fn status_result(loaded_trace_name: Option<&str>) -> StatusResult {
        StatusResult {
            loaded_trace_name: loaded_trace_name.map(str::to_owned),
            human_readable_version: None,
            api_version: None,
            version_code: None,
        }
    }

    fn status_for_trace(trace_path: &Path) -> StatusResult {
        status_result(Some(&format!("{} (0 MB)", trace_path.display())))
    }
}
