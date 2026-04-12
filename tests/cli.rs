//! Integration tests for the apmc CLI binary.
//!
//! Tests that require root are `#[ignore]`d. Run them with:
//!   sudo cargo test -- --ignored

use std::process::Command;

fn apmc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_apmc"))
}

// ── help ──────────────────────────────────────────────────────────────────

#[test]
fn help_subcommand_exits_zero() {
    let output = apmc().arg("help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("apmc"));
}

#[test]
fn help_long_flag_exits_zero() {
    let output = apmc().arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("list"));
    assert!(stdout.contains("stat"));
}

#[test]
fn no_args_exits_nonzero() {
    let output = apmc().output().unwrap();
    assert!(!output.status.success());
}

// ── list ──────────────────────────────────────────────────────────────────

#[test]
fn list_shows_cpu_and_events() {
    let output = apmc().arg("list").output().unwrap();
    assert!(output.status.success(), "list should not require root");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("CPU:"));
    assert!(stdout.contains("events listed"));
}

#[test]
fn list_filter_narrows_results() {
    let all = apmc().arg("list").output().unwrap();
    let filtered = apmc().args(["list", "CACHE"]).output().unwrap();
    assert!(all.status.success());
    assert!(filtered.status.success());

    let all_count = extract_event_count(&String::from_utf8_lossy(&all.stdout));
    let filtered_count = extract_event_count(&String::from_utf8_lossy(&filtered.stdout));
    assert!(filtered_count < all_count, "filter should narrow results");
    assert!(filtered_count > 0, "CACHE filter should match some events");
}

#[test]
fn list_filter_no_match_shows_zero() {
    let output = apmc()
        .args(["list", "ZZZZZ_NO_MATCH_EVER_ZZZZZ"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("0 events listed"));
}

// ── stat ──────────────────────────────────────────────────────────────────

#[test]
fn stat_no_command_exits_nonzero() {
    let output = apmc().arg("stat").output().unwrap();
    assert!(!output.status.success());
}

#[test]
fn stat_help_shows_default_events() {
    let output = apmc().args(["stat", "--help"]).output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("L1D_CACHE_MISS_LD"));
    assert!(stdout.contains("system-wide"));
}

#[test]
#[ignore] // Requires root: run with `sudo cargo test -- --ignored`
fn stat_per_process_runs_true() {
    let output = apmc().args(["stat", "--", "true"]).output().unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cycles"));
    assert!(stderr.contains("instructions"));
    assert!(stderr.contains("seconds wall clock"));
}

#[test]
#[ignore] // Requires root
fn stat_system_wide_runs_true() {
    let output = apmc().args(["stat", "-s", "--", "true"]).output().unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cycles"));
    assert!(stderr.contains("instructions"));
}

#[test]
#[ignore] // Requires root
fn stat_custom_events() {
    let output = apmc()
        .args([
            "stat",
            "-e",
            "FIXED_CYCLES,FIXED_INSTRUCTIONS",
            "--",
            "true",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cycles"));
}

#[test]
#[ignore] // Requires root
fn stat_reports_nonzero_exit_status() {
    let output = apmc().args(["stat", "--", "false"]).output().unwrap();
    // apmc itself exits 0; it reports the child's exit status in output.
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("exit status"));
}

// ── helpers ───────────────────────────────────────────────────────────────

fn extract_event_count(output: &str) -> usize {
    for line in output.lines() {
        if line.contains("events listed") {
            return line
                .trim()
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        }
    }
    0
}

