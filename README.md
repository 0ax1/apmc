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
```

## Example Output

```
 Performance counter stats for 'datafusion-bench tpch ...' (per-process):

        24,531,787,227  cycles
        58,130,536,158  instructions  # 2.37 insn per cycle

            77,463,891  BRANCH_MISPRED_NONSPEC
         1,110,914,261  L1D_CACHE_MISS_LD
         1,625,780,918  L1D_CACHE_MISS_ST
                47,611  ATOMIC_OR_EXCLUSIVE_FAIL
         8,737,883,140  MAP_STALL
           334,101,420  LDST_X64_UOP
         3,428,355,453  MAP_SIMD_UOP
           534,447,924  SCHEDULE_EMPTY

          1.452434 seconds wall clock
```

## How It Works

1. **Event discovery**: Parses Apple's kpep database at `/usr/share/kpep/` (binary plists describing all PMC events for each CPU)
2. **Counter programming**: Loads the private `kperf.framework` via `dlopen` and calls the `kpc_*` API to configure and read hardware counters
3. **Slot assignment**: Automatically assigns events to counter slots respecting hardware constraints (`counters_mask`)
4. **Per-process measurement**: Injects a small dylib (`DYLD_INSERT_LIBRARIES`) into the target process that captures hardware counters on every thread. Two mechanisms ensure full coverage:
   - **TLS destructor**: captures counters for threads that terminate naturally (spawn/join)
   - **Signal collection**: at process exit, sends `SIGUSR2` to all live threads (thread pools, async runtimes) so each reads its own counters

The injector dylib is compiled from C by `build.rs` and embedded in the `apmc` binary — no external files needed.

## Architecture

Apple Silicon CPUs expose:
- **Fixed counters** (always active): CPU cycles, retired instructions
- **Configurable counters** (8 slots on M-series): programmable to count any supported PMC event

Some events are constrained to specific counter slots via a `counters_mask`. The tool handles this automatically, assigning most-constrained events first.

## Default Events

When no `-e` flag is given, `apmc stat` monitors these 8 events (plus cycles and instructions from fixed counters):

| Event | What it measures | Why it matters |
|---|---|---|
| `L1D_CACHE_MISS_LD` | Loads that missed L1 data cache | Primary memory bottleneck signal -- every miss goes to L2 or further |
| `L1D_CACHE_MISS_ST` | Stores that missed L1 data cache | Write-path cache pressure -- high counts mean dirty data thrashing |
| `ATOMIC_OR_EXCLUSIVE_FAIL` | Atomic/exclusive ops that failed due to contention | Cross-core cache line contention -- the coherency signal Apple exposes |
| `MAP_STALL` | Total cycles the pipeline was stalled | Universal "time wasted" counter -- how much of your runtime is stalls |
| `LDST_X64_UOP` | Loads/stores crossing a 64-byte cacheline boundary | Alignment problems -- directly actionable by fixing struct/buffer layout |
| `BRANCH_MISPRED_NONSPEC` | Retired mispredicted branches | Unpredictable control flow -- costly pipeline flushes on each mispredict |
| `MAP_SIMD_UOP` | SIMD/FP micro-ops dispatched | Vectorization utilization -- low ratio vs total instructions means work is scalar |
| `SCHEDULE_EMPTY` | Cycles the scheduler had nothing to run | Frontend starvation -- complements MAP_STALL to distinguish backend-blocked vs starved |

Note: `INST_*` events (retired instruction counts) require Apple's private `com.apple.private.kperf` entitlement and silently read 0 without it. The `MAP_*_UOP` variants are the working alternative.

## Requirements

- macOS on Apple Silicon
- **SIP must be disabled** to access configurable counters (`csrutil disable` from Recovery Mode). Fixed counters (cycles, instructions) may work with SIP enabled, but configurable event programming requires `kpc_force_all_ctrs_set` which is blocked by SIP.
- Root privileges for `apmc stat` (the kernel requires root to program PMC counters and read per-thread counters)
- `apmc list` works without root and without disabling SIP

## See Also

- [COUNTERS.md](COUNTERS.md) -- full list of available PMC events on Apple A15/M2
- Apple's kpep database: `/usr/share/kpep/`
