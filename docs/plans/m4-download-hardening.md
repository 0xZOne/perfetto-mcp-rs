# Milestone 4 — Download And Distribution Hardening

## Context

The roadmap (`docs/roadmap.zh-CN.md` lines 100–115) defines M4 as the last gating
item for the v0.2 release: making `trace_processor_shell` acquisition robust
against interrupted downloads, local corruption, restricted networks, and
untested platforms. Current code at `src/download.rs` has three soft spots:

1. **Non-atomic writes** — `tokio::fs::write(dest, &bytes)` writes directly to
   the final path (line 103). A crash or SIGKILL mid-write leaves a truncated
   binary in the cache that passes the `.exists()` check next run.
2. **No integrity check** beyond a Content-Length `>1MB` guard. Local bit rot
   or a partial copy is silently accepted.
3. **Hardcoded upstream** (`ARTIFACTS_BASE` constant line 12). Users behind a
   firewall or a corporate proxy cannot point at a mirror.

Additionally, CI runs **only on `ubuntu-latest`**, so none of the download,
rename, or spawn paths are exercised on macOS or Windows — the platforms the
release workflow actually publishes for. M4 closes all four gaps. Completing
M4 unblocks the last unchecked item on the v0.2 release gate ("完成下载原子化",
roadmap line 182).

## Approach

Rewrite `download.rs` around a **streaming download into a `tempfile::NamedTempFile`
co-located with the cache directory**, with a rolling **SHA-256 hasher** that
produces a `.sha256` sidecar written via the same atomic-persist mechanism. On
cache hit, re-verify the sidecar — mismatch means corruption and triggers a
re-download. Introduce a small `DownloadConfig { base_url: String }` struct
threaded through `TraceProcessorManager` so the upstream URL is an explicit
(not ambient) input, driven by a new `--artifacts-base-url` CLI flag with env
var `PERFETTO_ARTIFACTS_BASE_URL`. Finally, split `ci.yml` into a lint job and
a 3-platform test matrix so every change exercises the download path on all
supported runners.

Keep the error model as-is (`anyhow`-through-`PerfettoError::Other`). No
consumer currently branches on download-error kind, so adding variants would be
speculative API shaping.

## File-by-file changes

### 1. `Cargo.toml` — add deps

```toml
tempfile = "3"
sha2 = "0.10"
hex = "0.4"
futures-util = "0.3"
```

`futures-util` must be a direct dependency: reqwest's transitive copy is not
visible to our crate, and `stream.next().await` requires importing
`futures_util::StreamExt`.

### 2. `src/download.rs` — full rewrite (~250 lines)

**Public surface**

```rust
pub struct DownloadConfig { pub base_url: String }
impl DownloadConfig {
    pub fn from_override(override_url: Option<String>) -> Self { /* trim trailing '/' */ }
}
impl Default for DownloadConfig { /* DEFAULT_ARTIFACTS_BASE_URL */ }

pub async fn ensure_binary(config: &DownloadConfig) -> Result<PathBuf>;
```

**Control flow inside `ensure_binary`**

1. `PERFETTO_TP_PATH` env override — unchanged.
2. `which::which("trace_processor_shell")` — unchanged.
3. Cached path check:
   - Compute `cache_dir` and `binary_path` as today.
   - `sweep_stale_temp_files(&cache_dir)` — best-effort cleanup of leftover
     `.tmp*` files older than 1 hour (belt-and-braces for SIGKILL mid-download).
   - If `binary_path.exists() && !is_stale(&binary_path)`:
     - Call `verify_sidecar(&binary_path)?` returning `{Verified,
       LegacyNoSidecar, Mismatch}`.
     - `Verified` → return cached path.
     - `LegacyNoSidecar` → log `warn!` ("upgrading pre-M4 cache"), fall
       through to re-download. Pre-M4 caches may already be corrupted, so
       we **invalidate** rather than bless them — self-healing by hashing
       the existing bytes would lock in any pre-existing corruption and
       defeat the milestone goal.
     - `Mismatch` → log `warn!`, fall through to re-download.
4. `download_binary(config, &binary_path).await?` → return.

**`download_binary` streaming rewrite**

```rust
let url = format!("{}/{TP_VERSION}/{arch}/{BINARY_NAME}", config.base_url);
let client = reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(10))
    .read_timeout(Duration::from_secs(120))
    .build()?;
let resp = client.get(&url).send().await?.error_for_status()?;

if let Some(len) = resp.content_length() {
    if len < MIN_EXPECTED_SIZE { bail!(...) }
}

let parent = dest.parent().context("cache dir missing")?;
tokio::fs::create_dir_all(parent).await?;

let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
let mut hasher = sha2::Sha256::new();
let mut total: u64 = 0;

// IMPORTANT: write through the NamedTempFile's own handle via
// `tmp.as_file_mut()`. Do NOT reopen `tmp.path()` with
// `std::fs::File::create` — on Windows the second open fails with a
// sharing violation against the handle NamedTempFile already holds.
use std::io::Write;
use futures_util::StreamExt;
let mut stream = resp.bytes_stream();
while let Some(chunk) = stream.next().await {
    let chunk = chunk?;
    hasher.update(&chunk);
    total += chunk.len() as u64;
    tmp.as_file_mut().write_all(&chunk)?;
}
tmp.as_file().sync_all()?;
// Do not drop `tmp` here — `persist_with_retry` needs to consume it.

if total < MIN_EXPECTED_SIZE { bail!("download too small: {total}") }

#[cfg(unix)]
{
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755))?;
}

persist_with_retry(tmp, dest)?;     // 5x, 100ms backoff on PermissionDenied (Windows AV)
let hex = hex::encode(hasher.finalize());
write_sidecar_atomically(dest, &hex)?;
```

**Private helpers (pure, unit-testable without HTTP)**

- `fn binary_url(config: &DownloadConfig, arch: &str) -> String` — returns
  `format!("{}/{TP_VERSION}/{arch}/{BINARY_NAME}", config.base_url)`.
  Pure, sync, no network, no OS-specific branches. Extracted specifically
  so the mirror-override threading can be tested deterministically:
  `download_binary` itself is async + networked and its failure-mode
  strings vary by platform, so we do not test URL threading through it.
- `fn sidecar_path(binary: &Path) -> PathBuf` — `{binary}.sha256`.
- `fn write_sidecar_atomically(binary: &Path, hex: &str) -> Result<()>` —
  `NamedTempFile::new_in(parent).write_all().persist()`.
- `fn read_sidecar(binary: &Path) -> Result<Option<String>>` — `None` on absent.
- `enum VerifyOutcome { Verified, LegacyNoSidecar, Mismatch }`.
- `fn verify_sidecar(binary: &Path) -> Result<VerifyOutcome>` — re-reads binary,
  hashes, compares.
- `fn persist_with_retry(tmp: NamedTempFile, dest: &Path) -> Result<()>` —
  Windows AV retry loop.
- `fn sweep_stale_temp_files(cache_dir: &Path, max_age: Duration)` —
  `read_dir` filter for entries whose name starts with `.tmp` and whose
  `metadata.modified().elapsed() > max_age`; log-and-continue on I/O error.
  Taking `max_age` as an explicit parameter avoids needing a time-mock or a
  `filetime` dev-dependency to test the "old file gets removed" path — the
  test passes a very small `max_age` (e.g. `Duration::from_millis(1)` after
  a brief `thread::sleep`) rather than mutating the file's mtime. Production
  callers pass a module-level `const STALE_TEMP_MAX_AGE: Duration =
  Duration::from_secs(3600)`.

**Tests to add (in the existing `mod tests`)**

- `sidecar_roundtrip_verifies_clean_file` — write fake bytes, persist sidecar,
  verify → `Verified`.
- `sidecar_detects_tampered_binary` — persist sidecar, mutate one byte in the
  binary, verify → `Mismatch`.
- `sidecar_missing_returns_legacy` — binary without sidecar → `LegacyNoSidecar`
  (classification only; the caller in `ensure_binary` then treats it as
  "invalidate cache, re-download").
- `write_sidecar_atomically_creates_file` — happy path + file contents.
- `download_config_default_uses_upstream` — base URL is the upstream constant.
- `download_config_override_trims_trailing_slash` — `"https://x/"` →
  `"https://x"`.
- `binary_url_uses_configured_base` — construct
  `DownloadConfig { base_url: "https://mirror.example".into() }`, call
  `binary_url(&cfg, "linux-amd64")`, assert result contains
  `"https://mirror.example"`, `TP_VERSION`, `"linux-amd64"`, and
  `BINARY_NAME`. Pure function, no network. This is the deterministic
  guard against the "override threaded through clap but not read by
  download code" regression.
- `sweep_stale_temp_files_removes_old_tmp` — create a `.tmp*` file in a
  tempdir, sleep a few ms, call
  `sweep_stale_temp_files(dir, Duration::from_millis(1))`, assert the file
  is gone. Avoids the need to mutate mtime (which `std::fs` cannot do
  cross-platform without pulling in `filetime` as a dev-dep).
- Keep existing: `cache_path_includes_version`, `stale_check_on_nonexistent`,
  `platform_arch_returns_known`.

Env-var-dependent tests must serialize via a module-local
`static ENV_LOCK: Mutex<()>` to avoid flakes under parallel `cargo test`.

### 3. `src/tp_manager.rs` — plumb `DownloadConfig`

- Add field `download_config: DownloadConfig` to `TraceProcessorManager`
  (after line 258).
- Introduce a **private common constructor** that owns the single struct
  literal and accepts every configurable parameter. All public constructors
  delegate to it.

  ```rust
  fn new_inner(
      max_instances: usize,
      starting_port: u16,
      tp_config: TraceProcessorConfig,
      download_config: DownloadConfig,
  ) -> Self {
      let cap = NonZeroUsize::new(max_instances).unwrap_or(NonZeroUsize::MIN);
      Self {
          inner: Mutex::new(ManagerInner {
              instances: LruCache::new(cap),
              next_port: starting_port,
              starting_port,
          }),
          spawn_locks: Mutex::new(HashMap::new()),
          binary_path: OnceCell::new(),
          config: tp_config,
          download_config,
      }
  }
  ```

- Public constructors — each becomes a one-liner delegating to `new_inner`:
  - `new(max_instances)` → `new_inner(max_instances, DEFAULT_STARTING_PORT,
    TraceProcessorConfig::default(), DownloadConfig::default())`.
  - `new_with_config(max_instances, tp_config)` → `new_inner(...,
    DEFAULT_STARTING_PORT, tp_config, DownloadConfig::default())`.
  - `new_with_starting_port(max_instances, starting_port)` — used by
    `tests/e2e_smoke.rs` line 28 — → `new_inner(..., starting_port,
    TraceProcessorConfig::default(), DownloadConfig::default())`.
  - `new_with_starting_port_and_config(max_instances, starting_port,
    tp_config)` → `new_inner(..., starting_port, tp_config,
    DownloadConfig::default())`.
  - **New**: `new_with_configs(max_instances, tp_config, download_config)`
    → `new_inner(max_instances, DEFAULT_STARTING_PORT, tp_config,
    download_config)`. This is the entrypoint used by `main.rs` for the
    CLI-driven mirror override.
  - `new_with_binary` (test-only, line 309) unchanged — delegates to `new`
    and then sets `binary_path` directly, skipping download.
- Removing the duplicated struct literals eliminates the "N constructors
  must all grow a new field" maintenance hazard Codex flagged: `new_inner`
  is the single place that touches `download_config` (and any future
  fields).
- Do **not** preemptively add a `new_with_starting_port_and_configs` variant.
  No caller currently combines a non-default starting port with a non-default
  download config. Add that constructor — also as a one-liner delegating to
  `new_inner` — when the first caller appears.
- Update `ensure_binary` at line 323:
  ```rust
  crate::download::ensure_binary(&self.download_config).await?;
  ```

### 4. `src/main.rs` — CLI flag

Add to `Args`:

```rust
/// Override the base URL for downloading trace_processor_shell.
/// Leave unset to use the default Perfetto LUCI artifacts bucket.
#[arg(long, env = "PERFETTO_ARTIFACTS_BASE_URL")]
artifacts_base_url: Option<String>,
```

Construct `DownloadConfig::from_override(args.artifacts_base_url.clone())`,
pass into `TraceProcessorManager::new_with_configs`. Extend the startup
`tracing::info!` (line 51) to log the base URL host so users can confirm a
mirror override took effect.

### 5. `.github/workflows/ci.yml` — split lint, add matrix

```yaml
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - run: sudo apt-get update && sudo apt-get install -y protobuf-compiler
      - uses: dtolnay/rust-toolchain@stable
        with: { components: clippy, rustfmt }
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all --check
      - run: cargo clippy --all-targets -- -D warnings

  test:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v6
      - name: Install protoc
        shell: bash
        run: |
          case "$RUNNER_OS" in
            Linux)   sudo apt-get update && sudo apt-get install -y protobuf-compiler ;;
            macOS)   brew install protobuf ;;
            Windows) choco install protoc --no-progress ;;
          esac
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      # NO trace_processor_shell cache: every matrix run must exercise the
      # real download path end-to-end (atomic temp-file persist, sidecar
      # write, Windows rename-retry). A hash-keyed cache would only bust on
      # src/download.rs changes, silently skipping download-path validation
      # on PRs that touch main.rs / tp_manager.rs / workflow / tests — the
      # exact PRs most likely to regress M4. The ~35 MB per-run download
      # cost is acceptable for the coverage gain.
      - run: cargo test --all-targets
```

`fail-fast: false` so a Windows-only regression does not mask Linux/macOS
signal. Dropping the `actions/cache@v5` step for `trace_processor_shell` is
deliberate: with no cache the smoke test at `tests/e2e_smoke.rs` runs the
full `tp_manager::ensure_binary → download::ensure_binary` path cold on
every PR × every OS, validating atomic temp-file persist, sidecar write,
and Windows rename-retry on each matrix runner.

**What the matrix actually validates, and what it does not:**

- Cold-download path (cross-platform, every run): atomic persist, sidecar
  write, rename retry. Exercised by the smoke test.
- Verify-sidecar path (cross-platform, every run): exercised by the unit
  tests `sidecar_roundtrip_verifies_clean_file`,
  `sidecar_detects_tampered_binary`, `sidecar_missing_returns_legacy`,
  which `cargo test --all-targets` runs on every matrix OS.
- Warm-cache end-to-end (same process calling `ensure_binary` twice): NOT
  exercised. `OnceCell` in `TraceProcessorManager` makes `ensure_binary`
  run exactly once per process, and the smoke test only triggers one
  `get_client` call. If a future milestone needs in-process warm-cache
  coverage, it should add a dedicated integration test that spawns a
  subprocess (fresh `OnceCell`) against an already-populated cache dir.

## Critical files

- `src/download.rs` — complete rewrite around streaming + temp-file persist + sidecar.
- `src/tp_manager.rs` lines 258, 273–303, 319–329 — add `download_config` field and constructor variant.
- `src/main.rs` lines 16–40, 59–66 — new CLI flag and plumbing.
- `.github/workflows/ci.yml` — split into `lint` and matrix `test` jobs.
- `Cargo.toml` — add `tempfile`, `sha2`, `hex`, `futures-util` (all required
  direct dependencies; see section 1).

## Existing utilities to reuse

- `dirs::data_local_dir()` — already resolves platform cache root (`cache_dir()` fn).
- `which::which` — already used for PATH lookup.
- Existing `is_stale` 7-day check — keep as a secondary trigger.
- Existing `platform_arch` mapping — unchanged.
- `tracing::info!` / `warn!` / `debug!` — used throughout the crate.

## Deliberate non-goals

- **Pinned per-version SHA-256 constants in source.** The sidecar-of-observed-
  hash approach catches the local bit-rot case the roadmap actually names
  ("损坏 binary 可被识别并重新下载"); HTTPS already covers authenticity. Pinning
  would add per-TP-version maintenance on five platforms for a threat model
  HTTPS already closes. Revisit if a future milestone adds supply-chain
  attestation.
- **Cross-process file lock** around the cache directory. Two MCP servers
  starting simultaneously is a real but rare race; on POSIX the atomic rename
  makes both downloads succeed with last-writer-wins; on Windows the loser
  retries. Adding `fs2` lockfile hardening is a separate follow-up.
- **Streaming result decoding in `execute_sql`.** Belongs to M6, not M4.
- **New `PerfettoError` variants** for download. No caller branches on them.

## Open items to confirm during implementation

1. **Windows antivirus retry tuning.** `persist_with_retry` uses 5 × 100 ms; if
   Windows CI flakes on the first run, bump to 10 × 200 ms before giving up.
2. **Upstream `.sha256`.** Before writing PR #3, `curl -I` the URL
   `{ARTIFACTS_BASE}/v54.0/linux-amd64/trace_processor_shell.sha256`. If
   Perfetto publishes it, the sidecar becomes "copy of upstream hash" — a free
   upgrade to authoritative verification. If not, keep the
   locally-computed-hash design. Do not block landing on this probe.

## Suggested landing order

1. **Deps + `DownloadConfig` plumbing + CLI flag.** No behavior change: default
   URL is preserved. `cargo test` green.
2. **Atomic write + streaming download + temp-file sweep.** `download_binary`
   rewrite. New unit tests for sidecar helpers compile but the verify path is
   not yet called.
3. **Sidecar verification on cache hit + legacy self-heal.** Wire
   `verify_sidecar` into `ensure_binary`. Tamper test added.
4. **CI matrix split.** Land last so any Windows-specific spawn fallout
   surfaces in isolation and can be reverted without touching correctness code.

Each step is independently reviewable and leaves the tree green.

## Verification plan

End-to-end validation before declaring M4 done:

- **Local** (Linux dev box):
  - `cargo fmt --all --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test --all-targets` (unit + e2e smoke). Unit tests cover:
    `verify_sidecar` classification, `write_sidecar_atomically` round-trip,
    `DownloadConfig::from_override` trailing-slash trim, and
    `sweep_stale_temp_files` with a tiny `max_age`.
  - Manual interrupt test: delete cached binary, run `cargo test e2e_smoke`,
    Ctrl-C mid-download, re-run. **Expected**: the second run completes
    successfully. A `.tmp*` file may or may not be present depending on
    whether `NamedTempFile::drop` fired before termination; either way is
    acceptable because (a) the next run's `NamedTempFile::new_in` picks a
    fresh random name and does not conflict, and (b) `sweep_stale_temp_files`
    will GC any genuine leftover on a later run once it crosses
    `STALE_TEMP_MAX_AGE`. The verification is: no corruption in the final
    cached binary, and the tree does not accumulate temp files over multiple
    interrupt cycles (spot-check by running the loop a few times and
    listing the cache dir).
  - Manual tamper test: overwrite one byte of cached binary, re-run smoke test
    — logs should show a `warn!` about checksum mismatch and the binary should
    be re-downloaded.
  - Manual mirror test: `cargo run -- --artifacts-base-url https://example.invalid`
    then send an MCP `load_trace` request (e.g. via `tests/fixtures/basic.perfetto-trace`
    over stdin JSON-RPC, or via an MCP client). The server should fail the
    lazy `ensure_binary` step with an error that mentions `example.invalid`
    in the URL, confirming the CLI flag is threaded through clap →
    `DownloadConfig::from_override` → `download_binary`. **This path
    deliberately goes through `main.rs`; setting `PERFETTO_ARTIFACTS_BASE_URL`
    on `cargo test` has no effect because the test harness constructs
    `TraceProcessorManager` directly with `DownloadConfig::default()`.**
    Automated coverage: the `binary_url_uses_configured_base` unit test
    (see tests list above) asserts that the private `binary_url` helper
    honors `DownloadConfig.base_url`. This is pure, sync, network-free,
    and runs on every matrix OS; it locks the "override threaded through
    clap into the URL" contract deterministically. We deliberately do
    **not** test URL threading by calling `download_binary` against an
    invalid URL — that path depends on reqwest's platform-specific DNS/
    TCP error strings and would flake.
- **MCP tools** (after local tests pass):
  - `load_trace` on `tests/fixtures/basic.perfetto-trace`, then `list_tables`,
    then a simple `execute_sql` — confirms the download + spawn + query path
    still works end to end.
- **CI** (after pushing the matrix split):
  - All three OS jobs green on a clean PR. Because the
    `actions/cache@v5` step is intentionally removed, every matrix run is a
    cold download on a fresh runner — this validates atomic persist,
    sidecar write, and Windows rename-retry per OS on every PR.
  - Sidecar-read / verify path is **not** covered by the smoke test within
    a single CI run (single `get_client` call + `OnceCell` ⇒ one
    `ensure_binary`). That path is covered by the unit tests
    (`sidecar_roundtrip_verifies_clean_file`, `sidecar_detects_tampered_binary`,
    `sidecar_missing_returns_legacy`), which `cargo test --all-targets`
    runs on every matrix OS. Do not rely on "re-run with warm cache" —
    Actions rerun starts from a fresh runner under the no-cache design.
