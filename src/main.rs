use std::process::Command;
use std::time::Instant;

use kpc::kpc::KpcManager;
use kpc::kpep::KpepDatabase;

const DEFAULT_EVENTS: &[&str] = &[
    "L1D_CACHE_MISS_LD",
    "L1D_CACHE_MISS_ST",
    "ATOMIC_OR_EXCLUSIVE_FAIL",
    "MAP_STALL",
    "LDST_X64_UOP",
    "BRANCH_MISPRED_NONSPEC",
    "INST_SIMD_ALU",
    "INST_BARRIER",
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
    eprintln!("kpc — Apple Silicon hardware performance counters\n");
    eprintln!("Usage:");
    eprintln!("  kpc list [filter]                                   List available PMC events");
    eprintln!(
        "  kpc stat [-e EVT1,EVT2,...] [--] <cmd> [args...]    Measure counters for a command"
    );
    eprintln!("  kpc help                                            Show this help\n");
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
        eprintln!("Usage: sudo kpc stat [-e EVT1,EVT2,...] [--] <command> [args...]");
        eprintln!("\nDefault events: {}", DEFAULT_EVENTS.join(", "));
        eprintln!("\nRun `kpc list` to see all available events.");
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

    let before = mgr.read_system_wide()?;
    let t0 = Instant::now();

    let status = Command::new(&cmd_args[0]).args(&cmd_args[1..]).status()?;

    let elapsed = t0.elapsed();
    let after = mgr.read_system_wide()?;

    let delta = mgr.delta(&before, &after);
    let labeled = mgr.labeled_counters(&delta);

    let cmd_display = cmd_args.join(" ");
    eprintln!(
        "\n Performance counter stats for '{cmd_display}' (system-wide, {} CPUs):\n",
        mgr.ncpu()
    );

    eprintln!("  {:>20}  {}", fmt_comma(delta.cycles), "cycles");
    let ipc = if delta.cycles > 0 {
        delta.instructions as f64 / delta.cycles as f64
    } else {
        0.0
    };
    eprintln!(
        "  {:>20}  {}  # {:.2} insn per cycle",
        fmt_comma(delta.instructions),
        "instructions",
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
