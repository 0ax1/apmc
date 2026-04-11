# kpc

CLI tool and Rust library for reading Apple Silicon hardware performance counters via the private `kperf` framework.

The macOS equivalent of `perf stat` on Linux.

## Quick Start

```bash
cargo build --release

# List all available PMC events for your CPU
./target/release/kpc list

# Filter events by keyword
./target/release/kpc list cache
./target/release/kpc list branch

# Measure hardware counters for a command (requires root)
sudo ./target/release/kpc stat -- sleep 1

# Choose specific events
sudo ./target/release/kpc stat -e L1D_CACHE_MISS_LD,BRANCH_MISPRED_NONSPEC -- ./my_program
```

## Example Output

```
CPU: Apple M2 (2 fixed + 8 configurable counters, 8 CPUs)

 Performance counter stats for 'sleep 1' (system-wide, 8 CPUs):

       1,042,861,523  cycles
         348,291,082  instructions  # 0.33 insn per cycle

           1,204,531  L1D_CACHE_MISS_LD
             892,104  L1D_CACHE_WRITEBACK
             304,821  L1I_CACHE_MISS_DEMAND
              12,491  L1D_TLB_MISS
               2,304  L2_TLB_MISS_DATA
          22,103,842  MAP_STALL_DISPATCH
              48,201  BRANCH_MISPRED_NONSPEC
         102,384,012  INST_LDST

        1.002841 seconds wall clock
```

## How It Works

1. **Event discovery**: Parses Apple's kpep database at `/usr/share/kpep/` (binary plists describing all PMC events for each CPU)
2. **Counter programming**: Loads the private `kperf.framework` via `dlopen` and calls the `kpc_*` API to configure and read hardware counters
3. **Slot assignment**: Automatically assigns events to counter slots respecting hardware constraints (`counters_mask`)
4. **Measurement**: Takes system-wide counter snapshots before/after running the target command

## Architecture

Apple Silicon CPUs expose:
- **Fixed counters** (always active): CPU cycles, retired instructions
- **Configurable counters** (8 slots on M-series): programmable to count any supported PMC event

Some events are constrained to specific counter slots via a `counters_mask`. The tool handles this automatically, assigning most-constrained events first.

## Default Events

When no `-e` flag is given, `kpc stat` monitors:

| Event | What it measures |
|---|---|
| `L1D_CACHE_MISS_LD` | L1 data cache load misses |
| `L1I_CACHE_MISS_DEMAND` | L1 instruction cache misses |
| `L1D_TLB_MISS` | L1 data TLB misses |
| `L2_TLB_MISS_DATA` | L2 TLB misses (triggers page walks) |
| `ATOMIC_OR_EXCLUSIVE_FAIL` | Atomic/exclusive contention failures |
| `MAP_STALL_DISPATCH` | Cycles stalled on dispatch backpressure |
| `BRANCH_MISPRED_NONSPEC` | Retired mispredicted branches |
| `INST_LDST` | Retired load/store instructions |

## Requirements

- macOS on Apple Silicon (M1/M2/M3/M4)
- Root privileges for `kpc stat` (the kernel requires root to program PMC counters)
- `kpc list` works without root

## Library Usage

```rust
use kpc::{KpcManager, KpepDatabase};

let db = KpepDatabase::load_current_cpu()?;

// Look up events
let events: Vec<_> = ["L1D_CACHE_MISS_LD", "BRANCH_MISPRED_NONSPEC"]
    .iter()
    .filter_map(|name| db.event_by_name(name))
    .collect();

// Configure and read counters
let mut mgr = KpcManager::new()?; // requires root
mgr.configure(&events)?;

let before = mgr.read_system_wide()?;
// ... run workload ...
let after = mgr.read_system_wide()?;

let delta = mgr.delta(&before, &after);
for (name, value) in mgr.labeled_counters(&delta) {
    println!("{name}: {value}");
}
```

## See Also

- [COUNTERS.md](COUNTERS.md) -- full list of available PMC events on Apple A15/M2
- Apple's kpep database: `/usr/share/kpep/`
