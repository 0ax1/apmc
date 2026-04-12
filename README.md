# apmc

The macOS equivalent of `perf stat`.

## Install

```bash
cargo install --git https://github.com/0ax1/apmc
```

## Usage

```bash
# List all available PMC events for your CPU
apmc list

# Filter events by keyword
apmc list cache
apmc list branch

# Measure hardware counters for a command (requires root)
sudo apmc stat -- sleep 1

# Choose specific events
sudo apmc stat -e L1D_CACHE_MISS_LD,BRANCH_MISPRED_NONSPEC -- ./my_program

# System-wide counting (includes background activity)
sudo apmc stat -s -- ./my_program

# Disable colored output
sudo apmc stat --no-color -- ./my_program
```

## Example Output

```
 Performance counter stats for 'sleep 0.1':

             1,049,886  cycles
             2,379,442  instructions              # 2.27 insn per cycle

                 1,553  BRANCH_MISPRED_NONSPEC    # Instruction architecturally executed, mispredicted branch
                 6,326  L1D_CACHE_MISS_LD         # Loads that missed the L1 Data Cache
                 8,071  L1D_CACHE_MISS_ST         # Stores that missed the L1 Data Cache
                     0  ATOMIC_OR_EXCLUSIVE_FAIL  # Atomic or exclusive instruction failed due to contention
                54,808  MAP_STALL                 # Cycles while the Map Unit was stalled for any reason
                   766  LDST_X64_UOP              # Load and store uops that crossed a 64B boundary
                17,919  MAP_SIMD_UOP              # Mapped Advanced SIMD and FP Unit uops
                29,623  SCHEDULE_EMPTY            # Cycles while the uop scheduler is empty

          0.121722 seconds wall clock
```

The `# …` descriptions next to each event are sourced from Apple's kpep database (`/usr/share/kpep/`). Disable colored output with `--no-color` or the `NO_COLOR` environment variable.

## How It Works

1. **Event discovery**: Parses Apple's kpep database at `/usr/share/kpep/` (binary plists describing all PMC events for each CPU)
2. **Counter programming**: Loads the private `kperf.framework` via `dlopen` and calls the `kpc_*` API to configure and read hardware counters
3. **Slot assignment**: Automatically assigns events to counter slots respecting hardware constraints (`counters_mask`)
4. **Per-process measurement** (default): A dylib injected via `DYLD_INSERT_LIBRARIES` hooks thread lifecycle to capture per-thread counters, covering both naturally terminating threads and long-lived thread pools
5. **System-wide measurement** (`-s`): Reads global counters summed across all CPUs before and after the command

## Architecture

Apple Silicon CPUs expose:
- **Fixed counters** (always active): CPU cycles, retired instructions
- **Configurable counters** (8 slots on M-series): programmable to count any supported PMC event

Some events are constrained to specific counter slots via a `counters_mask`. The tool handles this automatically, assigning most-constrained events first.

## Default Events

When no `-e` flag is given, `apmc stat` monitors these 8 events (plus cycles and instructions from fixed counters):

| Event | Description |
|---|---|
| `L1D_CACHE_MISS_LD` | Loads that missed L1 data cache |
| `L1D_CACHE_MISS_ST` | Stores that missed L1 data cache |
| `ATOMIC_OR_EXCLUSIVE_FAIL` | Atomic/exclusive ops that failed due to contention |
| `MAP_STALL` | Cycles the pipeline was stalled |
| `LDST_X64_UOP` | Loads/stores crossing a 64-byte cacheline boundary |
| `BRANCH_MISPRED_NONSPEC` | Mispredicted branches |
| `MAP_SIMD_UOP` | SIMD/FP micro-ops dispatched |
| `SCHEDULE_EMPTY` | Cycles the scheduler had nothing to run |

Note: `INST_*` events (retired instruction counts) require Apple's private `com.apple.private.kperf` entitlement and silently read 0 without it. The `MAP_*_UOP` variants are the working alternative.

## Requirements

- macOS on Apple Silicon
- SIP disabled (`csrutil disable` from Recovery Mode)
- Root privileges for `apmc stat`

## See Also

- [COUNTERS.md](COUNTERS.md) -- full list of available PMC events on Apple A15/M2
- Apple's kpep database: `/usr/share/kpep/`
