use std::io::Read as _;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::Instant;

use apmc::kpc::KpcManager;
use apmc::kpep::KpepDatabase;

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
    pub const SIG_DFL: usize = 0;
    pub const SIG_IGN: usize = 1;
}

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("list") => cmd_list(&args[2..]),
        Some("stat") => cmd_stat(&args[2..]),
        Some("help") | Some("--help") | Some("-h") => {
            print_usage();
            Ok(())
        }
        _ => {
            print_usage();
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!("apmc — Apple Silicon hardware performance counters\n");
    eprintln!("Usage:");
    eprintln!("  apmc list [filter]                                    List available PMC events");
    eprintln!(
        "  apmc stat [-e EVT1,EVT2,...] [-S] [--] <cmd> [args...]  Measure counters for a command"
    );
    eprintln!("  apmc help                                             Show this help\n");
    eprintln!("Options:");
    eprintln!("  -e, --events EVT1,EVT2,...   Comma-separated list of events to monitor");
    eprintln!("  -S, --system-wide            System-wide counting instead of per-process\n");
    eprintln!("The stat subcommand requires root (sudo).");
}

fn cmd_list(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let db = KpepDatabase::load_current_cpu()?;
    let filter = args.first();

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

fn cmd_stat(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut event_names: Vec<String> = Vec::new();
    let mut system_wide = false;
    let mut cmd_start = 0;

    let mut arg_idx = 0;
    while arg_idx < args.len() {
        if args[arg_idx] == "--events" || args[arg_idx] == "-e" {
            arg_idx += 1;
            if arg_idx < args.len() {
                event_names = args[arg_idx].split(',').map(|s| s.to_string()).collect();
            }
            arg_idx += 1;
        } else if args[arg_idx] == "--system-wide" || args[arg_idx] == "-S" {
            system_wide = true;
            arg_idx += 1;
        } else if args[arg_idx] == "--" {
            cmd_start = arg_idx + 1;
            break;
        } else {
            cmd_start = arg_idx;
            break;
        }
    }

    if cmd_start >= args.len() {
        eprintln!("Usage: sudo apmc stat [-e EVT1,EVT2,...] [-S] [--] <command> [args...]");
        eprintln!("\nDefault events: {}", DEFAULT_EVENTS.join(", "));
        eprintln!("\nRun `apmc list` to see all available events.");
        std::process::exit(1);
    }

    let cmd_args = &args[cmd_start..];

    let db = KpepDatabase::load_current_cpu()?;

    if event_names.is_empty() {
        event_names = DEFAULT_EVENTS.iter().map(|s| s.to_string()).collect();
    }

    let mut events = Vec::new();
    for name in &event_names {
        match db.event_by_name(name) {
            Some(e) => events.push(e),
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

    // Ignore SIGINT/SIGTERM in parent so we survive Ctrl+C and collect
    // results after the child exits. KpcManager::drop releases counters.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }

    if !system_wide {
        // Per-process mode (default): inject a dylib that tracks per-thread counters.
        let dylib_path = write_embedded_dylib()?;
        let (pipe_read, pipe_write) = pipe()?;

        cmd.env("DYLD_INSERT_LIBRARIES", &dylib_path);
        cmd.env("KPC_RESULT_FD", pipe_write.to_string());
        unsafe {
            cmd.pre_exec(move || {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGTERM, libc::SIG_DFL);
                let flags = libc::fcntl(pipe_write, libc::F_GETFD);
                libc::fcntl(pipe_write, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                Ok(())
            });
        }

        let start_time = Instant::now();
        let mut child = cmd.spawn()?;
        unsafe { libc::close(pipe_write) };
        let status = child.wait()?;
        let elapsed = start_time.elapsed();

        // The inject dylib accumulates per-thread deltas, so the snapshot
        // values are already deltas. Compute delta against a zeroed snapshot
        // (identity op) to produce a CounterDelta.
        let snap = read_inject_results(pipe_read, mgr.n_fixed())
            .ok_or("per-process counting failed: no results from inject dylib")?;
        let zero = apmc::kpc::CounterSnapshot {
            values: vec![0u64; snap.values.len()],
            n_fixed: snap.n_fixed,
        };
        let delta = mgr.delta(&zero, &snap);
        print_results(&mgr, &delta, cmd_args, elapsed, status);
    } else {
        // System-wide mode (opt-in via -S): read global counters before/after.
        // Drop root for the child process when possible.
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
        unsafe {
            cmd.pre_exec(|| {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGTERM, libc::SIG_DFL);
                Ok(())
            });
        }

        let before = mgr.read_system_wide()?;
        let start_time = Instant::now();
        let mut child = cmd.spawn()?;
        let status = child.wait()?;
        let elapsed = start_time.elapsed();

        let after = mgr.read_system_wide()?;
        let delta = mgr.delta(&before, &after);
        print_results(&mgr, &delta, cmd_args, elapsed, status);
    }
    Ok(())
}

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

fn pipe() -> Result<(i32, i32), Box<dyn std::error::Error>> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(&mut fds) } != 0 {
        return Err("pipe() failed".into());
    }
    Ok((fds[0], fds[1]))
}

/// Dylib bytes compiled by build.rs and embedded at compile time.
const INJECT_DYLIB_BYTES: &[u8] = include_bytes!(env!("KPC_INJECT_DYLIB"));

/// Write the embedded dylib to a temp file and return its path.
fn write_embedded_dylib() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join("libapmc_inject.dylib");
    std::fs::write(&path, INJECT_DYLIB_BYTES)?;
    Ok(path)
}

/// Read the per-process counter results written by libkpc_inject.dylib.
/// Protocol: u32 n_counters, then n_counters × u64 delta values.
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
