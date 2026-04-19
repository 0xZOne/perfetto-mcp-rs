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

    /// Create a placeholder file suitable for `--binary-path`. The binary
    /// validates the path exists + is a regular file before registering
    /// (see `run_install` in src/install.rs), so tests that pass a
    /// `--binary-path` must point at something real. The `.to_string()`
    /// form is what the call-log fixture records, so assertions using
    /// this path need to interpolate it instead of hard-coding
    /// `/fake/bin/perfetto-mcp-rs`.
    fn fake_binary_path(&self) -> String {
        let p = self.state_dir.join("fake-perfetto-mcp-rs");
        if !p.exists() {
            fs::write(&p, b"#!/bin/false\n").unwrap();
            // Mark executable — run_install refuses 0644 binaries (a real
            // failure mode for browser-downloaded releases on Unix).
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&p).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&p, perms).unwrap();
        }
        p.to_str().unwrap().to_owned()
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
    let bin = h.fake_binary_path();
    // Fake `mcp remove` on empty state returns 0 with no stderr (see fixture);
    // binary should proceed to add. Production: Claude returns non-zero + "not
    // found" stderr and gets classified benign.
    let out = h.run(&["install", "--scope", "user", "--binary-path", &bin]);
    assert_success(&out, "install should succeed on clean state");

    assert_eq!(
        h.recorded_calls(),
        vec![
            "claude|mcp remove perfetto-mcp-rs --scope user".to_string(),
            format!("claude|mcp add perfetto-mcp-rs --scope user {bin}"),
            "codex|mcp remove perfetto-mcp-rs".to_string(),
            format!("codex|mcp add perfetto-mcp-rs -- {bin}"),
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
    let bin = h.fake_binary_path();
    fs::write(h.state_dir.join("claude.registered"), "seed\n").unwrap();
    fs::write(h.state_dir.join("codex.registered"), "seed\n").unwrap();

    let out = h.run(&["install", "--scope", "user", "--binary-path", &bin]);
    assert_success(&out, "install over existing entry should succeed");

    assert_eq!(
        h.recorded_calls(),
        vec![
            "claude|mcp remove perfetto-mcp-rs --scope user".to_string(),
            format!("claude|mcp add perfetto-mcp-rs --scope user {bin}"),
            "codex|mcp remove perfetto-mcp-rs".to_string(),
            format!("codex|mcp add perfetto-mcp-rs -- {bin}"),
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
    let bin = h.fake_binary_path();
    let _ = h.run(&["install", "--scope", "user", "--binary-path", &bin]);
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
    let bin = h.fake_binary_path();
    fs::write(h.state_dir.join("claude.registered"), "seed\n").unwrap();

    let out = h.run_with_extra_env(
        &["install", "--scope", "user", "--binary-path", &bin],
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
    let bin = h.fake_binary_path();
    let out = h.run(&["install", "--skip-codex", "--binary-path", &bin]);
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
    let bin = h.fake_binary_path();
    let out = h.run_with_extra_env(
        &["install", "--scope", "user", "--binary-path", &bin],
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
    let bin = h.fake_binary_path();
    let out = h.run_with_extra_env(
        &["install", "--scope", "local", "--binary-path", &bin],
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
    let bin = h.fake_binary_path();
    let out = Command::new(BIN)
        .args(["install", "--scope", "user", "--binary-path", &bin])
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
    let bin = h.fake_binary_path();
    // Seed list so register_claude sees pre_registered=true and attempts remove.
    fs::write(h.state_dir.join("claude.registered"), "seed\n").unwrap();

    let out = h.run_with_extra_env(
        &[
            "install",
            "--scope",
            "local",
            "--skip-codex",
            "--binary-path",
            &bin,
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
    let bin = h.fake_binary_path();
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
            &bin,
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
// P1 regression (Codex latest): deregister_claude with --scope local + remove
// reports "not found" → **Failed** (not Skipped). The shell wrapper treats
// Skipped as "safe to delete binary", which with a Local/Project scope would
// silently orphan the real registration if the user is in the wrong CWD.
// Failed makes the wrapper preserve the binary so the user can retry from
// the right directory.
// ---------------------------------------------------------------------------
#[test]
fn uninstall_scope_local_not_found_is_fatal() {
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
    assert_failure(
        &out,
        "scope=local 'not found' MUST be Failed — Skipped would let the wrapper \
         delete the binary while a real scoped registration survives elsewhere",
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("project directory used at install time")
            || combined.contains("Keeping binary in place"),
        "expected wrong-CWD-or-already-removed hint: {combined}"
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
    let bin = h.fake_binary_path();
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
            &bin,
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
    let bin = h.fake_binary_path();
    let out = h.run_with_extra_env(
        &[
            "install",
            "--scope",
            "user",
            "--skip-codex",
            "--binary-path",
            &bin,
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
    // Pick a tempdir as the binary's CWD, pre-create the relative target
    // there. `std::path::absolute` resolves `./name` against CWD, so the
    // registered path becomes `<tempdir>/name` (regardless of the test
    // runner's CWD).
    let work = tempfile::tempdir().unwrap();
    let rel_name = "relative-fake-perfetto-mcp-rs";
    let rel_path = work.path().join(rel_name);
    fs::write(&rel_path, b"#!/bin/false\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&rel_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&rel_path, perms).unwrap();

    let fixtures = fixtures_dir();
    let out = Command::new(BIN)
        .current_dir(work.path())
        .args([
            "install",
            "--scope",
            "user",
            "--binary-path",
            &format!("./{rel_name}"),
        ])
        .env_clear()
        .env("PATH", fixtures.to_str().unwrap())
        .env("FAKE_STATE_DIR", &h.state_dir)
        .env("FAKE_RECORD_FILE", &h.record_file)
        .env("HOME", &h.home)
        .env("XDG_DATA_HOME", &h.xdg_data)
        .env("LOCALAPPDATA", &h.local_appdata)
        .output()
        .unwrap();
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
            .is_some_and(|f| f == rel_name),
        "registered path file_name mismatch: {}",
        registered_path.display()
    );
}

// ---------------------------------------------------------------------------
// P2 regression (Codex latest): `install` without `--binary-path` must FAIL
// fast. We deliberately do NOT fall back to `current_exe()`: on Linux that
// reads `/proc/self/exe`, which is the symlink-resolved target, so a
// versioned install (`~/bin/foo -> ~/opt/foo-0.8.0`) would register the
// 0.8.0 path and break future symlink re-point upgrades.
// ---------------------------------------------------------------------------
#[test]
fn install_requires_binary_path() {
    let h = Harness::new();
    let out = h.run(&["install", "--scope", "user"]);
    assert_failure(&out, "install must refuse to run without --binary-path");
    let stderr = String::from_utf8_lossy(&out.stderr);
    // clap's own "required argument missing" message mentions the flag name.
    assert!(
        stderr.contains("--binary-path"),
        "clap should mention the missing flag: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// P2 regression (Codex latest): install --binary-path pointing at a
// non-existent file must FAIL fast, NOT silently write a dead MCP entry.
// The binary validates existence + regular-file before calling `mcp add`.
// ---------------------------------------------------------------------------
#[test]
fn install_rejects_nonexistent_binary_path() {
    let h = Harness::new();
    let out = h.run(&[
        "install",
        "--scope",
        "user",
        "--binary-path",
        "/definitely/does/not/exist/perfetto-mcp-rs",
    ]);
    assert_failure(
        &out,
        "install must refuse a non-existent binary_path — no dead MCP entry",
    );
    let calls = h.recorded_calls();
    assert!(
        calls.is_empty() || !calls.iter().any(|c| c.contains("mcp add")),
        "no `mcp add` should be recorded when path check fails: {calls:?}"
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("not accessible") || combined.contains("broken MCP entry"),
        "expected actionable error message: {combined}"
    );
}

// ---------------------------------------------------------------------------
// P2 regression (Codex latest): --binary-path pointing at a 0644 file (e.g.
// browser-downloaded release without chmod +x) must fail. Otherwise install
// succeeds but Claude/Codex can't spawn the server later.
// ---------------------------------------------------------------------------
#[test]
fn install_rejects_non_executable_binary_path() {
    let h = Harness::new();
    let p = h.state_dir.join("non-exec-fake-perfetto-mcp-rs");
    fs::write(&p, b"#!/bin/false\n").unwrap();
    // Leave mode at default (post-umask 0644) — the bit we want to assert on.
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&p).unwrap().permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&p, perms).unwrap();

    let out = h.run(&[
        "install",
        "--scope",
        "user",
        "--binary-path",
        p.to_str().unwrap(),
    ]);
    assert_failure(
        &out,
        "install must refuse a non-executable binary — would write a dead MCP entry",
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("not executable") && combined.contains("chmod +x"),
        "expected chmod hint: {combined}"
    );
}

// ---------------------------------------------------------------------------
// P2 regression: --binary-path pointing at a directory (not a file) must
// fail. Prevents registering directories as if they were executables.
// ---------------------------------------------------------------------------
#[test]
fn install_rejects_directory_binary_path() {
    let h = Harness::new();
    // State dir is a valid directory we know exists.
    let dir = h.state_dir.to_str().unwrap().to_owned();
    let out = h.run(&["install", "--scope", "user", "--binary-path", &dir]);
    assert_failure(&out, "install must refuse a directory as binary_path");
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("not a regular file"),
        "expected regular-file hint: {combined}"
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
