// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use lru::LruCache;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::tp_client::TraceProcessorClient;

/// A running trace_processor_shell instance bound to a specific trace file.
struct TraceProcessorInstance {
    process: Child,
    port: u16,
    client: TraceProcessorClient,
}

impl TraceProcessorInstance {
    /// Spawn trace_processor_shell in HTTP-RPC mode on the given port.
    async fn spawn(
        binary: &Path,
        trace_path: &Path,
        port: u16,
    ) -> Result<Self> {
        let process = Command::new(binary)
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

        let client = TraceProcessorClient::new(port);

        let mut instance = Self {
            process,
            port,
            client,
        };
        instance.wait_ready().await?;
        Ok(instance)
    }

    /// Poll the /status endpoint until the instance is ready.
    async fn wait_ready(&mut self) -> Result<()> {
        for i in 0..50 {
            // Check if process exited early.
            if let Some(status) = self.process.try_wait()? {
                bail!(
                    "trace_processor_shell exited with {status} on port {}",
                    self.port,
                );
            }

            if self.client.status().await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            if i == 9 {
                tracing::debug!(
                    "still waiting for trace_processor_shell on port {}",
                    self.port,
                );
            }
        }
        bail!(
            "trace_processor_shell on port {} did not become ready within 5s",
            self.port,
        );
    }

    /// Check if the underlying process is still alive.
    fn is_alive(&mut self) -> bool {
        matches!(self.process.try_wait(), Ok(None))
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
    binary_path: PathBuf,
}

impl std::fmt::Debug for TraceProcessorManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraceProcessorManager")
            .field("binary_path", &self.binary_path)
            .finish_non_exhaustive()
    }
}

struct ManagerInner {
    instances: LruCache<PathBuf, TraceProcessorInstance>,
    next_port: u16,
}

impl TraceProcessorManager {
    /// Create a new manager.
    ///
    /// `binary_path` should point to `trace_processor_shell`.
    /// `max_instances` controls LRU capacity.
    pub fn new(binary_path: PathBuf, max_instances: usize) -> Self {
        let cap = NonZeroUsize::new(max_instances).unwrap_or(NonZeroUsize::MIN);
        Self {
            inner: Mutex::new(ManagerInner {
                instances: LruCache::new(cap),
                next_port: 9001,
            }),
            binary_path,
        }
    }

    /// Get or create a `TraceProcessorClient` for the given trace file.
    ///
    /// If the instance already exists in the cache, it is returned (and
    /// promoted in LRU order). If the instance's process has died, it is
    /// respawned. If the cache is full, the least recently used instance
    /// is evicted (its process is killed via `kill_on_drop`).
    pub async fn get_client(
        &self,
        trace_path: &Path,
    ) -> Result<TraceProcessorClient> {
        let canonical = trace_path
            .canonicalize()
            .with_context(|| format!("trace file not found: {}", trace_path.display()))?;

        // Fast path: check if already cached and alive.
        {
            let mut inner = self.inner.lock().await;
            if let Some(inst) = inner.instances.get_mut(&canonical) {
                if inst.is_alive() {
                    return Ok(inst.client.clone());
                }
                // Dead process — remove and respawn below.
                tracing::warn!(
                    "trace_processor_shell on port {} died, respawning",
                    inst.port,
                );
                inner.instances.pop(&canonical);
            }
        }

        // Slow path: spawn a new instance (without holding the lock).
        let port;
        {
            let mut inner = self.inner.lock().await;
            port = inner.next_port;
            inner.next_port = inner.next_port.wrapping_add(1);
            if inner.next_port < 9001 {
                inner.next_port = 9001;
            }
        }

        let instance =
            TraceProcessorInstance::spawn(&self.binary_path, &canonical, port)
                .await?;
        let client = instance.client.clone();

        // Insert into cache (may evict LRU entry, killing its process).
        {
            let mut inner = self.inner.lock().await;
            // Double-check: another task may have inserted while we spawned.
            if let Some(existing) = inner.instances.get(&canonical) {
                if existing.client.port() == client.port() {
                    return Ok(client);
                }
                // Use the existing one, drop our new spawn.
                return Ok(existing.client.clone());
            }
            inner.instances.put(canonical, instance);
        }

        Ok(client)
    }

    /// Shut down all managed instances.
    pub async fn shutdown(&self) {
        let mut inner = self.inner.lock().await;
        inner.instances.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_evicts_oldest_when_full() {
        let mut cache: LruCache<String, u16> =
            LruCache::new(NonZeroUsize::new(2).unwrap());
        cache.put("a".into(), 1);
        cache.put("b".into(), 2);
        cache.put("c".into(), 3);

        assert!(cache.get(&"a".to_string()).is_none(), "a should be evicted");
        assert!(cache.get(&"b".to_string()).is_some());
        assert!(cache.get(&"c".to_string()).is_some());
    }

    #[test]
    fn lru_access_refreshes_entry() {
        let mut cache: LruCache<String, u16> =
            LruCache::new(NonZeroUsize::new(2).unwrap());
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
}
