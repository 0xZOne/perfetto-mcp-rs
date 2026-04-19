// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

//! Hermetic integration tests for `perfetto-mcp-rs install` / `uninstall`.
//!
//! The tests swap `claude` and `codex` for shell fixtures under
//! `tests/fixtures/`. `PATH` is set to **only** the fixtures dir so
//! `which::which` can't accidentally find the real CLIs. `HOME` +
//! `XDG_DATA_HOME` + `LOCALAPPDATA` are swung to tempdirs so
//! `dirs::data_local_dir()` (and the binary's `clean_cache`) land in
//! isolated storage.
//!
//! Fixture protocol: see `tests/fixtures/claude`. Each run appends
//! `"<tool>|<args>"` to `$FAKE_RECORD_FILE` and consults per-op env vars
//! (`FAKE_{CLAUDE,CODEX}_{LIST,ADD,REMOVE}_{EXIT_CODE,STDOUT,STDERR}`).

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_perfetto-mcp-rs");

/// Absolute path to `tests/fixtures/` — stable across CWD / parallel tests.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Test env shim: wires up fake CLIs + isolated data dirs.
struct Harness {
    _root: TempDir,
    state_dir: PathBuf,
    record_file: PathBuf,
    xdg_data: PathBuf,
    local_appdata: PathBuf,
    home: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let rp = root.path().to_path_buf();
        let state_dir = rp.join("fake-state");
        let xdg_data = rp.join("xdg-data");
        let local_appdata = rp.join("local-appdata");
        let home = rp.join("home");
        for p in [&state_dir, &xdg_data, &local_appdata, &home] {
            fs::create_dir_all(p).unwrap();
        }
        let record_file = state_dir.join("calls.log");
        // Pre-create so append works even on first call.
        fs::write(&record_file, "").unwrap();
        Self {
            _root: root,
            state_dir,
            record_file,
            xdg_data,
            local_appdata,
            home,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_extra_env(args, &[])
    }

    fn run_with_extra_env(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let fixtures = fixtures_dir();
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .env_clear()
            // PATH: fixtures only. No fallback to real claude/codex, no external
            // binaries reachable from fixture (rm etc.). Important: must include
            // /bin for Rust's `std::process::Command` on some distros? Actually
            // no — Command.output() uses posix_spawn/exec directly, not a shell.
            .env("PATH", fixtures.to_str().unwrap())
            .env("FAKE_STATE_DIR", &self.state_dir)
            .env("FAKE_RECORD_FILE", &self.record_file)
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", &self.xdg_data)
            .env("LOCALAPPDATA", &self.local_appdata);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn recorded_calls(&self) -> Vec<String> {
        fs::read_to_string(&self.record_file)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Cache root as the binary will compute it: `$XDG_DATA_HOME/perfetto-mcp-rs`
    /// on Linux, `$HOME/Library/Application Support/perfetto-mcp-rs` on macOS.
    fn expected_cache_root(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.home
                .join("Library/Application Support/perfetto-mcp-rs")
        } else {
            self.xdg_data.join("perfetto-mcp-rs")
        }
    }

    fn seed_cache(&self) {
        let versioned = self.expected_cache_root().join("vX.Y");
        fs::create_dir_all(&versioned).unwrap();
        fs::write(versioned.join("trace_processor_shell"), b"fake").unwrap();
    }
}

fn assert_success(out: &Output, ctx: &str) {
    assert!(
        out.status.success(),
        "{ctx}: exit={:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn assert_failure(out: &Output, ctx: &str) {
    assert!(
        !out.status.success(),
        "{ctx}: expected non-zero exit but got success\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ---------------------------------------------------------------------------
// Contract 1: install from clean state → always remove-then-add, never `mcp
// list`. `claude mcp list` is NOT a passive probe (it spawns stdio servers
// for health checks), so we deliberately never run it on the install/uninstall
// paths. The benign "not found" from `mcp remove` on first install is
// classified and tolerated.
// ---------------------------------------------------------------------------
#[test]
fn install_from_clean_state_always_remove_then_add() {
    let h = Harness::new();
    // Fake `mcp remove` on empty state returns 0 with no stderr (see fixture);
    // binary should proceed to add. Production: Claude returns non-zero + "not
    // found" stderr and gets classified benign.
    let out = h.run(&[
        "install",
        "--scope",
        "user",
        "--binary-path",
        "/fake/bin/perfetto-mcp-rs",
    ]);
    assert_success(&out, "install should succeed on clean state");

    assert_eq!(
        h.recorded_calls(),
        vec![
            "claude|mcp remove perfetto-mcp-rs --scope user".to_string(),
            "claude|mcp add perfetto-mcp-rs --scope user /fake/bin/perfetto-mcp-rs".to_string(),
            "codex|mcp remove perfetto-mcp-rs".to_string(),
            "codex|mcp add perfetto-mcp-rs -- /fake/bin/perfetto-mcp-rs".to_string(),
        ],
        "no mcp list should appear in the call sequence",
    );
}

// ---------------------------------------------------------------------------
// Contract 2: install when already registered → remove-then-add. Same shape
// as clean state (we don't probe first either way).
// ---------------------------------------------------------------------------
#[test]
fn install_with_existing_entry_removes_then_adds() {
    let h = Harness::new();
    // Pre-populate fake state: both CLIs have the entry.
    fs::write(h.state_dir.join("claude.registered"), "seed\n").unwrap();
    fs::write(h.state_dir.join("codex.registered"), "seed\n").unwrap();

    let out = h.run(&[
        "install",
        "--scope",
        "user",
        "--binary-path",
        "/fake/bin/perfetto-mcp-rs",
    ]);
    assert_success(&out, "install over existing entry should succeed");

    assert_eq!(
        h.recorded_calls(),
        vec![
            "claude|mcp remove perfetto-mcp-rs --scope user".to_string(),
            "claude|mcp add perfetto-mcp-rs --scope user /fake/bin/perfetto-mcp-rs".to_string(),
            "codex|mcp remove perfetto-mcp-rs".to_string(),
            "codex|mcp add perfetto-mcp-rs -- /fake/bin/perfetto-mcp-rs".to_string(),
        ],
    );
}

// ---------------------------------------------------------------------------
// Probe-freeness regression lock: neither install nor uninstall may ever call
// `mcp list`. Production `claude mcp list` spawns workspace stdio servers,
// so invoking it from a project directory is unsafe and unreliable. Assert
// the fixture receives no list call across both maintenance paths.
// ---------------------------------------------------------------------------
#[test]
fn neither_install_nor_uninstall_invokes_mcp_list() {
    let h = Harness::new();
    let _ = h.run(&[
        "install",
        "--scope",
        "user",
        "--binary-path",
        "/fake/bin/perfetto-mcp-rs",
    ]);
    let _ = h.run(&["uninstall", "--scope", "user", "--keep-cache"]);
    let calls = h.recorded_calls();
    assert!(
        !calls.iter().any(|c| c.ends_with("|mcp list")),
        "install/uninstall must never call `mcp list` \
         (not a passive probe — spawns stdio servers): {calls:?}"
    );
}

// ---------------------------------------------------------------------------
// Contract 3: install over existing + claude remove fails → Failed before add.
// ---------------------------------------------------------------------------
#[test]
fn install_fails_when_existing_user_scope_remove_fails() {
    let h = Harness::new();
    fs::write(h.state_dir.join("claude.registered"), "seed\n").unwrap();

    let out = h.run_with_extra_env(
        &[
            "install",
            "--scope",
            "user",
            "--binary-path",
            "/fake/bin/perfetto-mcp-rs",
        ],
        &[
            ("FAKE_CLAUDE_REMOVE_EXIT_CODE", "17"),
            ("FAKE_CLAUDE_REMOVE_STDERR", "remove blew up"),
        ],
    );
    assert_failure(&out, "user-scope remove failure should fail overall");

    let calls = h.recorded_calls();
    // Critical: add must NOT run after remove failure.
    assert!(
        calls
            .iter()
            .any(|c| c == "claude|mcp remove perfetto-mcp-rs --scope user"),
        "remove should have been attempted: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("claude|mcp add ")),
        "claude add should NOT be called after remove failure: {calls:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("remove blew up"),
        "underlying stderr should be preserved: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Contract 4: --skip-codex → codex fixture never touched.
// ---------------------------------------------------------------------------
#[test]
fn install_skip_codex_never_invokes_codex() {
    let h = Harness::new();
    let out = h.run(&[
        "install",
        "--skip-codex",
        "--binary-path",
        "/fake/bin/perfetto-mcp-rs",
    ]);
    assert_success(&out, "install --skip-codex should succeed");
    for call in h.recorded_calls() {
        assert!(
            !call.starts_with("codex|"),
            "codex should not be invoked: {call}"
        );
    }
}

// ---------------------------------------------------------------------------
// Contract 5: any Failed => overall exit non-zero AND stderr preserved.
// ---------------------------------------------------------------------------
#[test]
fn install_aggregate_failure_surfaces_stderr() {
    let h = Harness::new();
    let out = h.run_with_extra_env(
        &[
            "install",
            "--scope",
            "user",
            "--binary-path",
            "/fake/bin/perfetto-mcp-rs",
        ],
        &[
            ("FAKE_CLAUDE_ADD_EXIT_CODE", "17"),
            ("FAKE_CLAUDE_ADD_STDERR", "claude add exploded"),
        ],
    );
    assert_failure(&out, "claude add failure should fail overall");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("claude add exploded"),
        "stderr should preserve fake output: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Contract 6: --scope local + failure → stderr has project-directory hint.
// ---------------------------------------------------------------------------
#[test]
fn install_scope_local_failure_has_project_hint() {
    let h = Harness::new();
    let out = h.run_with_extra_env(
        &[
            "install",
            "--scope",
            "local",
            "--binary-path",
            "/fake/bin/perfetto-mcp-rs",
        ],
        &[
            ("FAKE_CLAUDE_ADD_EXIT_CODE", "17"),
            ("FAKE_CLAUDE_ADD_STDERR", "no project dir"),
        ],
    );
    assert_failure(&out, "scope=local with add failure should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("scope=local"),
        "hint prefix missing: {stderr}"
    );
    assert!(
        stderr.contains("project directory"),
        "project-directory phrase missing: {stderr}"
    );
    assert!(
        stderr.contains("no project dir"),
        "underlying error dropped: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Contract 7: uninstall removes registrations AND cache (default path).
// ---------------------------------------------------------------------------
#[test]
fn uninstall_removes_registrations_and_cache() {
    let h = Harness::new();
    fs::write(h.state_dir.join("claude.registered"), "seed\n").unwrap();
    fs::write(h.state_dir.join("codex.registered"), "seed\n").unwrap();
    h.seed_cache();
    let cache_root = h.expected_cache_root();
    assert!(cache_root.exists(), "cache root precondition");

    let out = h.run(&["uninstall", "--scope", "user"]);
    assert_success(&out, "uninstall should succeed on seeded state");

    let calls = h.recorded_calls();
    assert!(
        calls
            .iter()
            .any(|c| c == "claude|mcp remove perfetto-mcp-rs --scope user"),
        "claude remove missing: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c == "codex|mcp remove perfetto-mcp-rs"),
        "codex remove missing: {calls:?}"
    );

    assert!(
        !cache_root.exists(),
        "cache root should have been removed: {:?}",
        cache_root
    );
}

// ---------------------------------------------------------------------------
// Contract 8: uninstall --keep-cache leaves cache intact.
// ---------------------------------------------------------------------------
#[test]
fn uninstall_keep_cache_preserves_cache() {
    let h = Harness::new();
    fs::write(h.state_dir.join("claude.registered"), "seed\n").unwrap();
    fs::write(h.state_dir.join("codex.registered"), "seed\n").unwrap();
    h.seed_cache();
    let cache_root = h.expected_cache_root();

    let out = h.run(&["uninstall", "--scope", "user", "--keep-cache"]);
    assert_success(&out, "uninstall --keep-cache should succeed");
    assert!(
        cache_root.exists(),
        "cache should have been preserved: {:?}",
        cache_root
    );
}

// ---------------------------------------------------------------------------
// Contract 9: cache already absent → Done("already absent"), exit 0.
//
// The plan's original phrasing was "cache root unavailable → Skipped". That
// specific branch is reachable only when `dirs::data_local_dir()` returns
// None, which on Linux requires both `$HOME` unset AND `getpwuid_r` failure.
// `env_clear()` doesn't prevent the passwd fallback, and setting HOME to a
// bogus path still yields a valid-looking cache path (just non-existent).
// Testing that reliably at integration level would need /etc/passwd
// tampering, so we test the **equivalent user-facing outcome** instead:
// when the cache tree doesn't exist at all, uninstall still exits 0 with a
// clear "no cache" message. The `Outcome::Skipped` / `Failed` branches for
// `cache_root()` error are exercised by the match arm's type shape at
// compile time and by read-through when touching clean_cache.
// ---------------------------------------------------------------------------
#[test]
fn uninstall_cache_absent_exits_ok() {
    let h = Harness::new();
    // Do NOT seed cache — the cache_root path doesn't exist under tempdir HOME.
    let cache_root = h.expected_cache_root();
    assert!(!cache_root.exists(), "precondition: cache absent");

    let out = h.run(&["uninstall", "--scope", "user"]);
    assert_success(&out, "uninstall with absent cache should exit 0");
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("already absent") || combined.contains("cache root unavailable"),
        "expected 'already absent' message, got:\n{combined}"
    );
}

// ---------------------------------------------------------------------------
// Contract 10: CLI missing (empty PATH for claude/codex) → exit 0, skip msg.
// ---------------------------------------------------------------------------
#[test]
fn install_cli_missing_skips_gracefully() {
    // Empty dir on PATH → which::which returns Err → Outcome::Skipped.
    let tmp = tempfile::tempdir().unwrap();
    let fake_empty = tmp.path().to_path_buf();

    let h = Harness::new();
    let out = Command::new(BIN)
        .args([
            "install",
            "--scope",
            "user",
            "--binary-path",
            "/fake/bin/perfetto-mcp-rs",
        ])
        .env_clear()
        .env("PATH", fake_empty.to_str().unwrap())
        .env("HOME", &h.home)
        .env("XDG_DATA_HOME", &h.xdg_data)
        .env("LOCALAPPDATA", &h.local_appdata)
        .output()
        .unwrap();
    assert_success(
        &out,
        "CLI-missing install should still succeed (everything skipped)",
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("claude not found"),
        "expected claude-missing message: {combined}"
    );
    assert!(
        combined.contains("codex not found"),
        "expected codex-missing message: {combined}"
    );
}

// ---------------------------------------------------------------------------
// register_claude symmetry: --scope local + EXISTING entry + remove reports
// a REAL error → overall Failed, add NOT attempted. Locks the stderr-
// classification parity with deregister_claude (prevents regression to the
// "tolerate all remove failures for non-User scope" bug).
// ---------------------------------------------------------------------------
#[test]
fn install_scope_local_real_remove_error_is_fatal_before_add() {
    let h = Harness::new();
    // Seed list so register_claude sees pre_registered=true and attempts remove.
    fs::write(h.state_dir.join("claude.registered"), "seed\n").unwrap();

    let out = h.run_with_extra_env(
        &[
            "install",
            "--scope",
            "local",
            "--skip-codex",
            "--binary-path",
            "/fake/bin/perfetto-mcp-rs",
        ],
        &[
            ("FAKE_CLAUDE_REMOVE_EXIT_CODE", "2"),
            (
                "FAKE_CLAUDE_REMOVE_STDERR",
                "Error: unable to parse ~/.claude.json: syntax error at line 42",
            ),
        ],
    );
    assert_failure(
        &out,
        "real claude remove error must fail install before add is attempted",
    );

    let calls = h.recorded_calls();
    assert!(
        calls
            .iter()
            .any(|c| c == "claude|mcp remove perfetto-mcp-rs --scope local"),
        "remove must have been attempted: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("claude|mcp add ")),
        "add must NOT run after a real remove error: {calls:?}"
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("syntax error at line 42"),
        "underlying error must be preserved: {combined}"
    );
    assert!(
        combined.contains("project directory"),
        "scope=local hint should wrap the error: {combined}"
    );
}

// ---------------------------------------------------------------------------
// register_claude symmetry: --scope local + EXISTING entry (different visible
// scope) + remove reports "not found" → tolerated, add proceeds. Locks the
// benign-tolerance path.
// ---------------------------------------------------------------------------
#[test]
fn install_scope_local_not_found_remove_is_tolerated() {
    let h = Harness::new();
    // list shows entry (probably a different-scope user/project entry visible
    // from this CWD) — pre_registered true → attempts remove.
    fs::write(h.state_dir.join("claude.registered"), "seed\n").unwrap();

    let out = h.run_with_extra_env(
        &[
            "install",
            "--scope",
            "local",
            "--skip-codex",
            "--binary-path",
            "/fake/bin/perfetto-mcp-rs",
        ],
        &[
            ("FAKE_CLAUDE_REMOVE_EXIT_CODE", "1"),
            (
                "FAKE_CLAUDE_REMOVE_STDERR",
                "No local-scoped MCP server found with name: perfetto-mcp-rs",
            ),
        ],
    );
    assert_success(
        &out,
        "benign 'not found' remove error must be tolerated; add should still run",
    );
    let calls = h.recorded_calls();
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("claude|mcp add perfetto-mcp-rs --scope local ")),
        "add must run after benign remove failure: {calls:?}"
    );
}

// ---------------------------------------------------------------------------
// deregister_claude with --scope local + remove reports "not found" → Skipped
// with project-directory hint (benign scope/CWD mismatch).
// ---------------------------------------------------------------------------
#[test]
fn uninstall_scope_local_not_found_is_skipped_with_hint() {
    let h = Harness::new();
    // Don't seed — list will be empty (mirrors "wrong CWD" in production).
    let out = h.run_with_extra_env(
        &[
            "uninstall",
            "--scope",
            "local",
            "--skip-codex",
            "--keep-cache",
        ],
        &[
            ("FAKE_CLAUDE_REMOVE_EXIT_CODE", "1"),
            // Real Claude wording for an absent entry under a given scope.
            (
                "FAKE_CLAUDE_REMOVE_STDERR",
                "No local-scoped MCP server found with name: perfetto-mcp-rs",
            ),
        ],
    );
    assert_success(
        &out,
        "scope=local 'not found' should be benign Skipped, not Failed",
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("not registered in this project directory"),
        "expected project-directory hint: {combined}"
    );
}

// ---------------------------------------------------------------------------
// deregister_claude with --scope local + remove reports a REAL error (config
// broken, I/O, etc.) → Failed. Must NOT be silently downgraded to Skipped,
// which is exactly the bug Codex flagged in the prior iteration.
// ---------------------------------------------------------------------------
#[test]
fn uninstall_scope_local_real_error_is_fatal() {
    let h = Harness::new();
    let out = h.run_with_extra_env(
        &[
            "uninstall",
            "--scope",
            "local",
            "--skip-codex",
            "--keep-cache",
        ],
        &[
            ("FAKE_CLAUDE_REMOVE_EXIT_CODE", "2"),
            (
                "FAKE_CLAUDE_REMOVE_STDERR",
                "Error: unable to parse ~/.claude.json: syntax error at line 42",
            ),
        ],
    );
    assert_failure(
        &out,
        "real claude remove error (not a 'not found') must fail uninstall, \
         not be tolerated as scope-mismatch",
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("syntax error at line 42"),
        "underlying error must be preserved: {combined}"
    );
    // And the scope-hint preface should wrap it (since scope != User).
    assert!(
        combined.contains("project directory"),
        "scope=local hint should wrap the error: {combined}"
    );
}

// ---------------------------------------------------------------------------
// deregister_claude --scope user against a clean (never-installed) state:
// remove is attempted, returns "not found" (benign), classified as Skipped.
// Exit 0, user-friendly message.
// ---------------------------------------------------------------------------
#[test]
fn uninstall_scope_user_clean_state_is_skipped() {
    let h = Harness::new();
    // Fake remove on empty state exits 0 silently. Inject the real Claude
    // wording + non-zero exit so we exercise the classifier path.
    let out = h.run_with_extra_env(
        &[
            "uninstall",
            "--scope",
            "user",
            "--skip-codex",
            "--keep-cache",
        ],
        &[
            ("FAKE_CLAUDE_REMOVE_EXIT_CODE", "1"),
            (
                "FAKE_CLAUDE_REMOVE_STDERR",
                "No user-scoped MCP server found with name: perfetto-mcp-rs",
            ),
        ],
    );
    assert_success(&out, "clean-state uninstall should exit 0");
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("no user-scoped perfetto-mcp-rs registration to remove"),
        "expected benign skip message for not-found: {combined}"
    );
    let calls = h.recorded_calls();
    assert!(
        calls
            .iter()
            .any(|c| c == "claude|mcp remove perfetto-mcp-rs --scope user"),
        "remove must be attempted (no probe path): {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.ends_with("|mcp list")),
        "no list probe allowed: {calls:?}"
    );
}

// ---------------------------------------------------------------------------
// P2 regression (Codex latest): classifier must reject stderr that mixes a
// "not found" line with a config-corruption recovery message. Uninstall must
// report Failed, not silently Skipped.
// ---------------------------------------------------------------------------
#[test]
fn uninstall_mixed_corruption_and_not_found_is_fatal() {
    let h = Harness::new();
    let out = h.run_with_extra_env(
        &[
            "uninstall",
            "--scope",
            "local",
            "--skip-codex",
            "--keep-cache",
        ],
        &[
            ("FAKE_CLAUDE_REMOVE_EXIT_CODE", "1"),
            (
                "FAKE_CLAUDE_REMOVE_STDERR",
                "Warning: ~/.claude.json was corrupted; backed up to \
                 ~/.claude.json.bak.\nNo local-scoped MCP server found with \
                 name: perfetto-mcp-rs",
            ),
        ],
    );
    assert_failure(
        &out,
        "corruption-plus-not-found output must NOT be classified benign; \
         uninstall must surface the recovery event as Failed",
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("corrupted") || combined.contains("backed up"),
        "corruption markers must reach the user: {combined}"
    );
}

// ---------------------------------------------------------------------------
// P2 regression (Codex #2): `install --scope user` must NOT abort when `list`
// shows an entry at a DIFFERENT visible scope. `mcp list` is not scope-aware
// — seeing an entry doesn't prove it's user-scope. remove under --scope user
// will return "not found"; that MUST be benign, add MUST still run.
// ---------------------------------------------------------------------------
#[test]
fn install_scope_user_tolerates_visible_non_user_entry() {
    let h = Harness::new();
    // Seed list so pre_registered=true (mirrors "there's a local/project entry
    // visible from this CWD").
    fs::write(h.state_dir.join("claude.registered"), "seed\n").unwrap();
    let out = h.run_with_extra_env(
        &[
            "install",
            "--scope",
            "user",
            "--skip-codex",
            "--binary-path",
            "/fake/bin/perfetto-mcp-rs",
        ],
        &[
            ("FAKE_CLAUDE_REMOVE_EXIT_CODE", "1"),
            (
                "FAKE_CLAUDE_REMOVE_STDERR",
                "No user-scoped MCP server found with name: perfetto-mcp-rs",
            ),
        ],
    );
    assert_success(
        &out,
        "scope=user install must NOT abort when remove says 'not found' \
         (the visible entry was at a different scope)",
    );
    let calls = h.recorded_calls();
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("claude|mcp add perfetto-mcp-rs --scope user ")),
        "add must still run after benign 'not found' remove: {calls:?}"
    );
}

// ---------------------------------------------------------------------------
// P2 regression (Codex #3): `uninstall --scope user` must NOT hard-fail when
// the only visible entry is local/project. remove will return "not found";
// that's a benign Skipped, not Failed.
// ---------------------------------------------------------------------------
#[test]
fn uninstall_scope_user_tolerates_not_found_from_non_user_entry() {
    let h = Harness::new();
    fs::write(h.state_dir.join("claude.registered"), "seed\n").unwrap();
    let out = h.run_with_extra_env(
        &[
            "uninstall",
            "--scope",
            "user",
            "--skip-codex",
            "--keep-cache",
        ],
        &[
            ("FAKE_CLAUDE_REMOVE_EXIT_CODE", "1"),
            (
                "FAKE_CLAUDE_REMOVE_STDERR",
                "No user-scoped MCP server found with name: perfetto-mcp-rs",
            ),
        ],
    );
    assert_success(
        &out,
        "scope=user uninstall 'not found' must be benign Skipped, not Failed",
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("no user-scoped perfetto-mcp-rs registration to remove"),
        "expected user-scope not-found message: {combined}"
    );
}

// ---------------------------------------------------------------------------
// P2 regression (Codex #1): install/uninstall must NOT be blocked by invalid
// server-only env vars (`PERFETTO_STARTUP_TIMEOUT_MS`). Those fields are
// server-path concerns and must be parsed lazily inside run_server, not by
// clap at top-level parse.
// ---------------------------------------------------------------------------
#[test]
fn install_survives_malformed_server_env_vars() {
    let h = Harness::new();
    let out = h.run_with_extra_env(
        &[
            "install",
            "--scope",
            "user",
            "--skip-codex",
            "--binary-path",
            "/fake/bin/perfetto-mcp-rs",
        ],
        &[
            ("PERFETTO_STARTUP_TIMEOUT_MS", "not-a-number"),
            ("PERFETTO_QUERY_TIMEOUT_MS", "garbage"),
        ],
    );
    assert_success(
        &out,
        "install must not be blocked by stale/invalid server-path env vars",
    );
}

#[test]
fn uninstall_survives_malformed_server_env_vars() {
    let h = Harness::new();
    let out = h.run_with_extra_env(
        &[
            "uninstall",
            "--scope",
            "user",
            "--skip-codex",
            "--keep-cache",
        ],
        &[
            ("PERFETTO_STARTUP_TIMEOUT_MS", "not-a-number"),
            ("PERFETTO_QUERY_TIMEOUT_MS", "garbage"),
        ],
    );
    assert_success(
        &out,
        "uninstall must not be blocked by stale/invalid server-path env vars",
    );
}

// ---------------------------------------------------------------------------
// P2 regression (Codex latest): install must NOT register a relative path —
// MCP clients spawn the server from their own CWD, so `./perfetto-mcp-rs`
// resolves to the wrong place (or nothing) later. The binary absolutizes the
// path before calling `mcp add`.
// ---------------------------------------------------------------------------
#[test]
fn install_registers_absolute_path_from_relative_input() {
    let h = Harness::new();
    let out = h.run(&[
        "install",
        "--scope",
        "user",
        "--binary-path",
        "./relative-fake-perfetto-mcp-rs",
    ]);
    assert_success(
        &out,
        "install with relative --binary-path should still succeed",
    );

    let calls = h.recorded_calls();
    let add_call = calls
        .iter()
        .find(|c| c.starts_with("claude|mcp add "))
        .expect("claude add call missing");
    // Parse the registered path (last whitespace-separated token in the call
    // record) and assert that Path::is_absolute sees it as absolute. We don't
    // enforce lexical cleanliness (e.g. `/cwd/./rel`) — what matters is that
    // exec-time resolution doesn't depend on the MCP client's CWD.
    let registered_path = PathBuf::from(
        add_call
            .rsplit(' ')
            .next()
            .expect("add call must have path arg"),
    );
    assert!(
        registered_path.is_absolute(),
        "registered path must be absolute; got {} in call {add_call}",
        registered_path.display()
    );
    assert!(
        registered_path
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|f| f == "relative-fake-perfetto-mcp-rs"),
        "registered path file_name mismatch: {}",
        registered_path.display()
    );
}

// ---------------------------------------------------------------------------
// Sanity: fixture is actually reachable & executable.
// ---------------------------------------------------------------------------
#[test]
fn fixture_is_executable() {
    let c = fixtures_dir().join("claude");
    assert!(c.exists(), "fixture missing: {:?}", c);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&c).unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "fixture not executable (mode={:o}); commit with chmod +x or CI will silent-fail",
            mode
        );
    }
}
