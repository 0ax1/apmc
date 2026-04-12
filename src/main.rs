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
        "  apmc stat [-e EVT1,EVT2,...] [--] <cmd> [args...]    Measure counters for a command"
    );
    eprintln!("  apmc help                                             Show this help\n");
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
        if let Some(f) = filter {
            let f_lower = f.to_lowercase();
            if !event.name.to_lowercase().contains(&f_lower)
                && !event.description.to_lowercase().contains(&f_lower)
            {
                continue;
            }
        }

        let mask_str = match event.counters_mask {
            Some(m) => format!("mask=0x{m:x}"),
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
    let mut cmd_start = 0;

    let mut i = 0;
    while i < args.len() {
        if args[i] == "--events" || args[i] == "-e" {
            i += 1;
            if i < args.len() {
                event_names = args[i].split(',').map(|s| s.to_string()).collect();
            }
            i += 1;
        } else if args[i] == "--" {
            cmd_start = i + 1;
            break;
        } else {
            cmd_start = i;
            break;
        }
    }

    if cmd_start >= args.len() {
        eprintln!("Usage: sudo apmc stat [-e EVT1,EVT2,...] [--] <command> [args...]");
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

    // The injector dylib is compiled by build.rs and embedded in the binary.
    let inject_dylib = write_embedded_dylib().ok();

    let mut cmd = Command::new(&cmd_args[0]);
    cmd.args(&cmd_args[1..]);

    let (pipe_read, pipe_write) = pipe()?;

    // Ignore SIGINT/SIGTERM in parent so we survive Ctrl+C and collect
    // results after the child exits. KpcManager::drop releases counters.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }

    let has_inject = inject_dylib.is_some();
    if let Some(ref dylib_path) = inject_dylib {
        // Child keeps root so it can read its own thread counters.
        cmd.env("DYLD_INSERT_LIBRARIES", dylib_path);
        cmd.env("KPC_RESULT_FD", pipe_write.to_string());
    } else {
        // No dylib -- fall back to system-wide, drop root for child.
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
    }
    unsafe {
        cmd.pre_exec(move || {
            // Restore default signal handling in child so Ctrl+C kills it.
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            if has_inject {
                let flags = libc::fcntl(pipe_write, libc::F_GETFD);
                libc::fcntl(pipe_write, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
            Ok(())
        });
    }

    let before_sw = mgr.read_system_wide()?;
    let t0 = Instant::now();

    let mut child = cmd.spawn()?;
    unsafe { libc::close(pipe_write) };

    let status = child.wait()?;
    let elapsed = t0.elapsed();

    // Use per-process results if available, otherwise fall back to system-wide.
    let delta = match read_inject_results(pipe_read, mgr.n_fixed()) {
        Some(snap) => {
            let zero = apmc::kpc::CounterSnapshot {
                values: vec![0u64; snap.values.len()],
                n_fixed: snap.n_fixed,
            };
            mgr.delta(&zero, &snap)
        }
        None => {
            let after = mgr.read_system_wide()?;
            mgr.delta(&before_sw, &after)
        }
    };
    unsafe { libc::close(pipe_read) };

    let labeled = mgr.labeled_counters(&delta);

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

    Ok(())
}

fn fmt_comma(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
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
    let mut file = unsafe { std::os::unix::io::FromRawFd::from_raw_fd(fd) };
    let file: &mut std::fs::File = &mut file;

    let mut n_buf = [0u8; 4];
    file.read_exact(&mut n_buf).ok()?;
    let n = u32::from_ne_bytes(n_buf) as usize;
    if n == 0 || n > 16 {
        return None;
    }

    let mut values = vec![0u64; n];
    let bytes = unsafe { std::slice::from_raw_parts_mut(values.as_mut_ptr() as *mut u8, n * 8) };
    file.read_exact(bytes).ok()?;

    Some(apmc::kpc::CounterSnapshot { values, n_fixed })
}
