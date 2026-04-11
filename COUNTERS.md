# Apple A15 / M2 PMC Events

Source: `/usr/share/kpep/cpu_100000c_2_da33d83d.plist`

## Fixed Counters

Always active, not programmable.

| Slot | Name | Alias |
|------|------|-------|
| 0 | FIXED_CYCLES | Cycles |
| 1 | FIXED_INSTRUCTIONS | Instructions |

## Configurable Events (58)

8 counter slots available. Events with a `counters_mask` can only be assigned to specific slots.

### Slot Constraints

- **any slot** -- can be assigned to slots 0-7
- **mask=0xe0** -- restricted to slots 5, 6, 7
- **mask=0x80** -- restricted to slot 7 only

### Cache

| # | Event | Slots | Description |
|---|-------|-------|-------------|
| 163 | L1D_CACHE_MISS_LD | any | Loads that missed the L1 Data Cache |
| 162 | L1D_CACHE_MISS_ST | any | Stores that missed the L1 Data Cache |
| 191 | L1D_CACHE_MISS_LD_NONSPEC | 0xe0 | Retired loads that missed in the L1 Data Cache |
| 192 | L1D_CACHE_MISS_ST_NONSPEC | 0xe0 | Retired stores that missed in the L1 Data Cache |
| 168 | L1D_CACHE_WRITEBACK | any | Dirty cache lines written back from L1D toward the Shared L2 Cache |
| 219 | L1I_CACHE_MISS_DEMAND | any | Demand fetch misses requiring a new cache line fill of the L1 Instruction Cache |

### TLB / MMU

| # | Event | Slots | Description |
|---|-------|-------|-------------|
| 160 | L1D_TLB_ACCESS | any | Load and store accesses to the L1 Data TLB |
| 5 | L1D_TLB_FILL | any | Translations filled into the L1 Data TLB |
| 161 | L1D_TLB_MISS | any | Load and store accesses that missed the L1 Data TLB |
| 193 | L1D_TLB_MISS_NONSPEC | 0xe0 | Retired loads and stores that missed in the L1 Data TLB |
| 4 | L1I_TLB_FILL | any | Translations filled into the L1 Instruction TLB |
| 212 | L1I_TLB_MISS_DEMAND | any | Demand instruction fetches that missed in the L1 Instruction TLB |
| 11 | L2_TLB_MISS_DATA | any | Loads and stores that missed in the L2 TLB |
| 10 | L2_TLB_MISS_INSTRUCTION | any | Instruction fetches that missed in the L2 TLB |
| 8 | MMU_TABLE_WALK_DATA | any | Table walk memory requests on behalf of data accesses |
| 7 | MMU_TABLE_WALK_INSTRUCTION | any | Table walk memory requests on behalf of instruction fetches |

### Branch Prediction

| # | Event | Slots | Description |
|---|-------|-------|-------------|
| 203 | BRANCH_MISPRED_NONSPEC | 0xe0 | Instruction architecturally executed, mispredicted branch |
| 197 | BRANCH_COND_MISPRED_NONSPEC | 0xe0 | Retired conditional branch instructions that mispredicted |
| 198 | BRANCH_INDIR_MISPRED_NONSPEC | 0xe0 | Retired indirect branch instructions (including calls and returns) that mispredicted |
| 202 | BRANCH_CALL_INDIR_MISPRED_NONSPEC | 0xe0 | Retired indirect call instructions mispredicted |
| 200 | BRANCH_RET_INDIR_MISPRED_NONSPEC | 0xe0 | Retired return instructions that mispredicted |

### Instruction Mix

| # | Event | Slots | Description |
|---|-------|-------|-------------|
| 140 | INST_ALL | 0x80 | All retired instructions |
| 155 | INST_LDST | 0x80 | Retired load and store instructions (excludes DC ZVA) |
| 141 | INST_BRANCH | 0xe0 | Retired branch instructions including calls and returns |
| 142 | INST_BRANCH_CALL | 0xe0 | Retired subroutine call instructions |
| 143 | INST_BRANCH_RET | 0xe0 | Retired subroutine return instructions |
| 144 | INST_BRANCH_TAKEN | 0xe0 | Retired taken branch instructions |
| 147 | INST_BRANCH_INDIR | 0xe0 | Retired indirect branch instructions including indirect calls |
| 156 | INST_BARRIER | 0xe0 | Retired data barrier instructions |
| 151 | INST_INT_ALU | 0x80 | Retired non-branch and non-load/store Integer Unit instructions |
| 149 | INST_INT_LD | 0xe0 | Retired load Integer Unit instructions |
| 150 | INST_INT_ST | 0x80 | Retired store Integer Unit instructions (excludes DC ZVA) |
| 154 | INST_SIMD_ALU | 0x80 | Retired non-load/store Advanced SIMD and FP Unit instructions |
| 159 | INST_SIMD_ALU_VEC | 0x80 | Retired non-load/store vector Advanced SIMD instructions |
| 152 | INST_SIMD_LD | 0xe0 | Retired load Advanced SIMD and FP Unit instructions |
| 153 | INST_SIMD_ST | 0xe0 | Retired store Advanced SIMD and FP Unit instructions |

### Pipeline / Micro-ops

| # | Event | Slots | Description |
|---|-------|-------|-------------|
| 1 | RETIRE_UOP | 0x80 | All retired uops |
| 2 | CORE_ACTIVE_CYCLE | any | Cycles while the core was active |
| 81 | SCHEDULE_EMPTY | any | Cycles while the uop scheduler is empty |
| 112 | MAP_STALL_DISPATCH | any | Cycles while the Map Unit was stalled because of Dispatch back pressure |
| 118 | MAP_STALL | any | Cycles while the Map Unit was stalled for any reason |
| 117 | MAP_REWIND | any | Cycles while the Map Unit was blocked while rewinding due to flush and restart |
| 214 | MAP_DISPATCH_BUBBLE | any | Cycles while the Map Unit had no uops to process and was not stalled |
| 124 | MAP_INT_UOP | any | Mapped Integer Unit uops |
| 125 | MAP_LDST_UOP | any | Mapped Load and Store Unit uops (including GPR to vector register converts) |
| 126 | MAP_SIMD_UOP | any | Mapped Advanced SIMD and FP Unit uops |
| 132 | FLUSH_RESTART_OTHER_NONSPEC | any | Pipeline flush and restarts not due to branch mispredictions or memory order violations |
| 222 | FETCH_RESTART | any | Fetch Unit internal restarts for any reason (excludes branch mispredicts) |

### Load/Store Unit

| # | Event | Slots | Description |
|---|-------|-------|-------------|
| 166 | LD_UNIT_UOP | any | Uops that flowed through the Load Unit |
| 167 | ST_UNIT_UOP | any | Uops that flowed through the Store Unit |
| 177 | LDST_X64_UOP | any | Load and store uops that crossed a 64B boundary |
| 178 | LDST_XPG_UOP | any | Load and store uops that crossed a 16KiB page boundary |
| 230 | LD_NT_UOP | any | Load uops that executed with non-temporal hint |
| 229 | ST_NT_UOP | any | Store uops that executed with non-temporal hint |
| 196 | ST_MEM_ORDER_VIOL_LD_NONSPEC | 0xe0 | Retired core store uops that triggered memory order violations with core load uops |

### Atomics

| # | Event | Slots | Description |
|---|-------|-------|-------------|
| 179 | ATOMIC_OR_EXCLUSIVE_SUCC | any | Atomic or exclusive instruction successfully completed |
| 180 | ATOMIC_OR_EXCLUSIVE_FAIL | any | Atomic or exclusive instruction failed due to contention |

### Interrupts

| # | Event | Slots | Description |
|---|-------|-------|-------------|
| 108 | INTERRUPT_PENDING | any | Cycles while an interrupt was pending because it was masked |

## Notes

- **Speculative vs Non-speculative**: Events ending in `_NONSPEC` count only architecturally retired operations. Events without that suffix may include speculative operations that were later squashed.
- **counters_mask**: The kernel rejects events programmed into invalid slots. `kpc` handles slot assignment automatically.
- **System-wide**: `kpc stat` measures counters across all CPUs. Background system activity contributes noise; for precise measurement, pin your workload to a core or run on a quiet system.
- **Root required**: Programming configurable counters requires `sudo`. The `kpc list` command works without root.
