//! CLI for reading Apple Silicon hardware performance counters.
//!
//! Two subcommands:
//! - **`list`**: Discover available PMC events for the current CPU by reading
//!   the kpep database at `/usr/share/kpep/`.
//! - **`stat`**: Measure hardware performance counters while running a command.
//!
//! ## Counting modes
//!
//! **Per-process** (default): A dylib is injected via `DYLD_INSERT_LIBRARIES`
//! that uses `pthread_introspection_hook` to track thread lifecycle and
//! accumulates per-thread counter deltas. Results reflect only the target process.
//!
//! **System-wide** (`-S`): Reads global counters summed across all CPUs before
//! and after the command. Includes background system activity.
//!
//! Both modes require root privileges (`sudo`) for counter access.

use std::io::Read as _;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::Instant;

use apmc::kpc::KpcManager;
use apmc::kpep::KpepDatabase;
use clap::{Parser, Subcommand};

/// Minimal libc bindings for pipe, signal, and fd manipulation.
///
/// Using a private module avoids pulling in the full `libc` crate
/// for a handful of POSIX functions.
mod libc {
    extern "C" {
        pub fn pipe(fds: *mut [i32; 2]) -> i32;
        pub fn close(fd: i32) -> i32;
        pub fn fcntl(fd: i32, cmd: i32, ...) -> i32;
        pub fn signal(sig: i32, handler: usize) -> usize;
    }
    pub const F_GETFD: i32 = 1;
    pub const F_SETFD: i32 = 2;
    pub const FD_CLOEXEC: i32 = 1;
    pub const SIGINT: i32 = 2;
    pub const SIGTERM: i32 = 15;
    pub const SIG_IGN: usize = 1;
}

/// Default events measured when `-e` is not specified.
///
/// Chosen to cover the most common performance bottlenecks on Apple Silicon:
/// cache misses, branch mispredictions, pipeline stalls, and SIMD utilization.
const DEFAULT_EVENTS: &[&str] = &[
    "L1D_CACHE_MISS_LD",
    "L1D_CACHE_MISS_ST",
    "ATOMIC_OR_EXCLUSIVE_FAIL",
    "MAP_STALL",
    "LDST_X64_UOP",
    "BRANCH_MISPRED_NONSPEC",
    "MAP_SIMD_UOP",
    "SCHEDULE_EMPTY",
];

/// Apple Silicon hardware performance counters.
#[derive(Parser)]
#[command(
    name = "apmc",
    version,
    about = "Apple Silicon hardware performance counters"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List available PMC events for the current CPU.
    List {
        /// Case-insensitive filter applied to event names and descriptions.
        filter: Option<String>,
    },

    /// Measure hardware counters for a command (requires sudo).
    #[command(
        trailing_var_arg = true,
        after_help = concat!(
            "Default events: L1D_CACHE_MISS_LD, L1D_CACHE_MISS_ST, ATOMIC_OR_EXCLUSIVE_FAIL,\n",
            "  MAP_STALL, LDST_X64_UOP, BRANCH_MISPRED_NONSPEC, MAP_SIMD_UOP, SCHEDULE_EMPTY\n\n",
            "Run `apmc list` to see all available events.",
        )
    )]
    Stat {
        /// Comma-separated list of events to monitor.
        #[arg(short = 'e', long = "events", value_delimiter = ',')]
        events: Option<Vec<String>>,

        /// Use system-wide counting instead of per-process.
        #[arg(short = 'S', long = "system-wide")]
        system_wide: bool,

        /// Command and arguments to run.
        #[arg(required = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::List { filter } => cmd_list(filter.as_deref()),
        Commands::Stat {
            events,
            system_wide,
            command,
        } => cmd_stat(events.unwrap_or_default(), system_wide, &command),
    }
}

/// List all PMC events available on the current CPU.
///
/// Reads the kpep database from `/usr/share/kpep/` and prints fixed counters,
/// configurable events, aliases, and counter slot masks. When `filter` is
/// provided, only events whose name or description matches (case-insensitive)
/// are shown.
fn cmd_list(filter: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let db = KpepDatabase::load_current_cpu()?;

    println!("CPU: {} ({})", db.cpu.marketing_name, db.cpu.architecture);
    println!(
        "Fixed counters: {}, Configurable counters: {}",
        db.cpu.fixed_counters, db.cpu.config_counters
    );

    if !db.cpu.aliases.is_empty() {
        println!("\nAliases:");
        for (alias, target) in &db.cpu.aliases {
            println!("  {alias} -> {target}");
        }
    }

    let fixed: Vec<_> = db.fixed_events().collect();
    if !fixed.is_empty() {
        println!("\nFixed counters:");
        for event in &fixed {
            println!(
                "  [fixed {}] {:<35} {}",
                event.fixed_counter.unwrap_or(0),
                event.name,
                event.description
            );
        }
    }

    println!("\nConfigurable events:");
    let mut count = 0;
    for event in db.configurable_events() {
        if let Some(pattern) = filter {
            let pattern_lower = pattern.to_lowercase();
            if !event.name.to_lowercase().contains(&pattern_lower)
                && !event.description.to_lowercase().contains(&pattern_lower)
            {
                continue;
            }
        }

        let mask_str = match event.counters_mask {
            Some(mask) => format!("mask=0x{mask:x}"),
            None => "any slot".to_string(),
        };

        println!(
            "  [{:>3}] {:<35} ({}) {}",
            event.number.unwrap_or(0),
            event.name,
            mask_str,
            event.description,
        );
        count += 1;
    }
    println!("\n{count} events listed.");

    Ok(())
}

/// Measure hardware performance counters while running a command.
///
/// In per-process mode (default), injects `libapmc_inject.dylib` via
/// `DYLD_INSERT_LIBRARIES`. The dylib hooks thread creation/destruction to
/// accumulate per-thread counter deltas, then writes results back through a
/// pipe fd at process exit.
///
/// In system-wide mode (`-S`), reads global counters summed across all CPUs
/// before and after the child process runs. The child drops root privileges
/// when `SUDO_UID`/`SUDO_GID` environment variables are set.
fn cmd_stat(
    event_names: Vec<String>,
    system_wide: bool,
    cmd_args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let db = KpepDatabase::load_current_cpu()?;

    let event_names: Vec<String> = if event_names.is_empty() {
        DEFAULT_EVENTS.iter().map(|s| s.to_string()).collect()
    } else {
        event_names
    };

    let mut events = Vec::new();
    for name in &event_names {
        match db.event_by_name(name) {
            Some(event) => events.push(event),
            None => eprintln!("Warning: unknown event '{name}', skipping"),
        }
    }

    if events.is_empty() {
        eprintln!("No valid events to monitor.");
        std::process::exit(1);
    }

    let mut mgr = KpcManager::new()?;
    eprintln!(
        "CPU: {} ({} fixed + {} configurable counters, {} CPUs)",
        db.cpu.marketing_name,
        mgr.n_fixed(),
        mgr.n_configurable(),
        mgr.ncpu(),
    );

    mgr.configure(&events)?;

    let mut cmd = Command::new(&cmd_args[0]);
    cmd.args(&cmd_args[1..]);

    // Set up mode-specific configuration before spawning.
    let pipe_fds = if system_wide {
        // System-wide: drop root for the child process when possible.
        if let (Some(uid), Some(gid)) = (
            std::env::var("SUDO_UID")
                .ok()
                .and_then(|s| s.parse::<u32>().ok()),
            std::env::var("SUDO_GID")
                .ok()
                .and_then(|s| s.parse::<u32>().ok()),
        ) {
            cmd.uid(uid).gid(gid);
        }
        None
    } else {
        // Per-process: inject dylib and set up a pipe for results.
        let dylib_path = write_embedded_dylib()?;
        let (pipe_read, pipe_write) = create_pipe()?;
        cmd.env("DYLD_INSERT_LIBRARIES", &dylib_path);
        cmd.env("KPC_RESULT_FD", pipe_write.to_string());
        // Clear close-on-exec so the child (and its injected dylib) can write
        // counter results back through this fd.
        unsafe {
            cmd.pre_exec(move || {
                let flags = libc::fcntl(pipe_write, libc::F_GETFD);
                libc::fcntl(pipe_write, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                Ok(())
            });
        }
        Some((pipe_read, pipe_write))
    };

    // Take a system-wide snapshot before spawning (only in system-wide mode).
    let before = if system_wide {
        Some(mgr.read_system_wide()?)
    } else {
        None
    };

    let start_time = Instant::now();
    let mut child = cmd.spawn()?;

    // Ignore SIGINT/SIGTERM in the parent AFTER fork. This way the child
    // inherits default signal handling (Ctrl+C kills it normally) while the
    // parent survives to collect and display results. The race window between
    // spawn() and this call is microseconds — acceptable for a CLI tool.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }

    // Close the parent's copy of the pipe write end so reads see EOF
    // when the child exits.
    if let Some((_, pipe_write)) = pipe_fds {
        unsafe { libc::close(pipe_write) };
    }

    let status = child.wait()?;
    let elapsed = start_time.elapsed();

    // Compute counter deltas from the appropriate source.
    let delta = if let Some(before) = before {
        // System-wide: diff global counters before/after.
        let after = mgr.read_system_wide()?;
        mgr.delta(&before, &after)
    } else {
        // Per-process: the inject dylib accumulates per-thread deltas, so the
        // snapshot values ARE the deltas. Diff against a zeroed snapshot to
        // produce a CounterDelta struct.
        let pipe_read = pipe_fds.unwrap().0;
        let snap = read_inject_results(pipe_read, mgr.n_fixed())
            .ok_or("per-process counting failed: no results from inject dylib")?;
        let zero = apmc::kpc::CounterSnapshot {
            values: vec![0u64; snap.values.len()],
            n_fixed: snap.n_fixed,
        };
        mgr.delta(&zero, &snap)
    };

    print_results(&mgr, &delta, cmd_args, elapsed, status);
    Ok(())
}

/// Format and print counter results to stderr.
///
/// Always prints cycles and instructions (from fixed counters) with IPC,
/// followed by each configured event's value, wall-clock time, and
/// exit status if the command failed.
fn print_results(
    mgr: &KpcManager,
    delta: &apmc::kpc::CounterDelta,
    cmd_args: &[String],
    elapsed: std::time::Duration,
    status: std::process::ExitStatus,
) {
    let labeled = mgr.labeled_counters(delta);

    let cmd_display = cmd_args.join(" ");
    eprintln!("\n Performance counter stats for '{cmd_display}':\n");

    eprintln!("  {:>20}  cycles", fmt_comma(delta.cycles));
    let ipc = if delta.cycles > 0 {
        delta.instructions as f64 / delta.cycles as f64
    } else {
        0.0
    };
    eprintln!(
        "  {:>20}  instructions  # {:.2} insn per cycle",
        fmt_comma(delta.instructions),
        ipc,
    );
    eprintln!();

    for (name, value) in &labeled {
        eprintln!("  {:>20}  {}", fmt_comma(*value), name);
    }

    eprintln!("\n  {:>16.6} seconds wall clock", elapsed.as_secs_f64());
    if !status.success() {
        eprintln!("  (exit status {:?})", status.code());
    }
    eprintln!();
}

/// Format a `u64` with comma-separated thousands (e.g., `1,234,567`).
fn fmt_comma(value: u64) -> String {
    let digits = value.to_string();
    let mut result = String::new();
    for (position, digit) in digits.chars().rev().enumerate() {
        if position > 0 && position % 3 == 0 {
            result.push(',');
        }
        result.push(digit);
    }
    result.chars().rev().collect()
}

/// Create a POSIX pipe, returning `(read_fd, write_fd)`.
fn create_pipe() -> Result<(i32, i32), Box<dyn std::error::Error>> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(&mut fds) } != 0 {
        return Err("pipe() failed".into());
    }
    Ok((fds[0], fds[1]))
}

/// Dylib bytes compiled by `build.rs` and embedded at compile time.
const INJECT_DYLIB_BYTES: &[u8] = include_bytes!(env!("KPC_INJECT_DYLIB"));

/// Write the embedded inject dylib to a temp file and return its path.
///
/// The dylib is extracted to `/tmp/libapmc_inject.dylib` each run. This is
/// necessary because `DYLD_INSERT_LIBRARIES` requires a filesystem path.
fn write_embedded_dylib() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join("libapmc_inject.dylib");
    std::fs::write(&path, INJECT_DYLIB_BYTES)?;
    Ok(path)
}

/// Read per-process counter results written by `libapmc_inject.dylib`.
///
/// Wire protocol: `u32` counter count, then `count * u64` delta values.
/// Returns `None` on EOF, short read, or invalid count (0 or >16).
/// Takes ownership of `fd` via `File::from_raw_fd` (closed on drop).
fn read_inject_results(fd: i32, n_fixed: usize) -> Option<apmc::kpc::CounterSnapshot> {
    let mut file: std::fs::File = unsafe { std::os::unix::io::FromRawFd::from_raw_fd(fd) };

    let mut count_buf = [0u8; 4];
    file.read_exact(&mut count_buf).ok()?;
    let counter_count = u32::from_ne_bytes(count_buf) as usize;
    if counter_count == 0 || counter_count > 16 {
        return None;
    }

    let mut values = vec![0u64; counter_count];
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(values.as_mut_ptr() as *mut u8, counter_count * 8)
    };
    file.read_exact(bytes).ok()?;

    Some(apmc::kpc::CounterSnapshot { values, n_fixed })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_comma_zero() {
        assert_eq!(fmt_comma(0), "0");
    }

    #[test]
    fn fmt_comma_small() {
        assert_eq!(fmt_comma(1), "1");
        assert_eq!(fmt_comma(999), "999");
    }

    #[test]
    fn fmt_comma_thousands() {
        assert_eq!(fmt_comma(1_000), "1,000");
        assert_eq!(fmt_comma(1_234_567), "1,234,567");
        assert_eq!(fmt_comma(1_000_000_000), "1,000,000,000");
    }

    #[test]
    fn fmt_comma_u64_max() {
        let s = fmt_comma(u64::MAX);
        assert!(s.contains(','));
        // u64::MAX = 18,446,744,073,709,551,615
        assert_eq!(s, "18,446,744,073,709,551,615");
    }

    #[test]
    fn pipe_roundtrip_inject_protocol() {
        use std::io::Write;
        use std::os::unix::io::FromRawFd;

        let (read_fd, write_fd) = create_pipe().unwrap();

        let mut write_file = unsafe { std::fs::File::from_raw_fd(write_fd) };
        let counter_count: u32 = 3;
        write_file.write_all(&counter_count.to_ne_bytes()).unwrap();
        for &val in &[100u64, 200, 300] {
            write_file.write_all(&val.to_ne_bytes()).unwrap();
        }
        drop(write_file); // closes write_fd

        let snap = read_inject_results(read_fd, 2).unwrap();
        assert_eq!(snap.values, vec![100, 200, 300]);
        assert_eq!(snap.n_fixed, 2);
    }

    #[test]
    fn inject_results_empty_pipe_returns_none() {
        let (read_fd, write_fd) = create_pipe().unwrap();
        unsafe { libc::close(write_fd) };

        assert!(read_inject_results(read_fd, 2).is_none());
    }

    #[test]
    fn inject_results_zero_count_returns_none() {
        use std::io::Write;
        use std::os::unix::io::FromRawFd;

        let (read_fd, write_fd) = create_pipe().unwrap();
        let mut write_file = unsafe { std::fs::File::from_raw_fd(write_fd) };
        write_file.write_all(&0u32.to_ne_bytes()).unwrap();
        drop(write_file);

        assert!(read_inject_results(read_fd, 2).is_none());
    }

    #[test]
    fn inject_results_count_too_large_returns_none() {
        use std::io::Write;
        use std::os::unix::io::FromRawFd;

        let (read_fd, write_fd) = create_pipe().unwrap();
        let mut write_file = unsafe { std::fs::File::from_raw_fd(write_fd) };
        write_file.write_all(&17u32.to_ne_bytes()).unwrap();
        drop(write_file);

        assert!(read_inject_results(read_fd, 2).is_none());
    }

    #[test]
    fn inject_results_truncated_data_returns_none() {
        use std::io::Write;
        use std::os::unix::io::FromRawFd;

        let (read_fd, write_fd) = create_pipe().unwrap();
        let mut write_file = unsafe { std::fs::File::from_raw_fd(write_fd) };
        // Write count=2 but only one u64 value (incomplete)
        write_file.write_all(&2u32.to_ne_bytes()).unwrap();
        write_file.write_all(&42u64.to_ne_bytes()).unwrap();
        drop(write_file);

        assert!(read_inject_results(read_fd, 2).is_none());
    }
}
