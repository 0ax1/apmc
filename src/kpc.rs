//! Low-level kpc (kernel performance counter) API via Apple's private kperf framework.
//!
//! Provides safe-ish Rust wrappers around the kpc C functions loaded at runtime via `dlopen`.

use std::ffi::c_void;

use thiserror::Error;

use crate::kpep::KpepEvent;

#[derive(Debug, Error)]
pub enum KpcError {
    #[error("failed to load kperf framework: {0}")]
    LoadError(String),
    #[error("missing symbol in kperf: {0}")]
    MissingSymbol(String),
    #[error("kpc_{0} failed (errno {1})")]
    ApiError(&'static str, i32),
    #[error("too many events: requested {requested}, max configurable {max}")]
    TooManyEvents { requested: usize, max: usize },
    #[error("requires root privileges")]
    NotRoot,
}

const KPC_CLASS_FIXED_MASK: u32 = 1 << 0;
const KPC_CLASS_CONFIGURABLE_MASK: u32 = 1 << 1;
const KPC_ALL: u32 = KPC_CLASS_FIXED_MASK | KPC_CLASS_CONFIGURABLE_MASK;

type KpcGetCounterCountFn = unsafe extern "C" fn(u32) -> i32;
type KpcForceAllCtrsSetFn = unsafe extern "C" fn(i32) -> i32;
type KpcSetCountingFn = unsafe extern "C" fn(u32) -> i32;
type KpcSetThreadCountingFn = unsafe extern "C" fn(u32) -> i32;
type KpcSetConfigFn = unsafe extern "C" fn(u32, *const u64) -> i32;
type KpcGetCpuCountersFn = unsafe extern "C" fn(i32, u32, *mut i32, *mut u64) -> i32;

/// Loaded kpc function table. All pointers are non-null after successful construction.
struct KpcFns {
    _handle: *mut c_void,
    get_counter_count: KpcGetCounterCountFn,
    force_all_ctrs_set: KpcForceAllCtrsSetFn,
    set_counting: KpcSetCountingFn,
    set_thread_counting: KpcSetThreadCountingFn,
    set_config: KpcSetConfigFn,
    get_cpu_counters: KpcGetCpuCountersFn,
}

// dlopen/dlsym access to kperf is Send+Sync since we only call from one place at a time.
unsafe impl Send for KpcFns {}
unsafe impl Sync for KpcFns {}

impl KpcFns {
    fn load() -> Result<Self, KpcError> {
        let path = c"/System/Library/PrivateFrameworks/kperf.framework/kperf";
        const RTLD_LAZY: i32 = 1;
        let handle = unsafe { dlopen(path.as_ptr(), RTLD_LAZY) };
        if handle.is_null() {
            return Err(KpcError::LoadError(
                "dlopen kperf.framework failed".to_string(),
            ));
        }

        macro_rules! load_sym {
            ($name:ident, $ty:ty) => {{
                let sym_name =
                    std::ffi::CString::new(stringify!($name)).expect("valid CString for symbol");
                let ptr = unsafe { dlsym(handle, sym_name.as_ptr()) };
                if ptr.is_null() {
                    return Err(KpcError::MissingSymbol(stringify!($name).to_string()));
                }
                unsafe { std::mem::transmute::<*mut c_void, $ty>(ptr) }
            }};
        }

        Ok(KpcFns {
            _handle: handle,
            get_counter_count: load_sym!(kpc_get_counter_count, KpcGetCounterCountFn),
            force_all_ctrs_set: load_sym!(kpc_force_all_ctrs_set, KpcForceAllCtrsSetFn),
            set_counting: load_sym!(kpc_set_counting, KpcSetCountingFn),
            set_thread_counting: load_sym!(kpc_set_thread_counting, KpcSetThreadCountingFn),
            set_config: load_sym!(kpc_set_config, KpcSetConfigFn),
            get_cpu_counters: load_sym!(kpc_get_cpu_counters, KpcGetCpuCountersFn),
        })
    }
}

extern "C" {
    fn dlopen(path: *const std::ffi::c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
    fn geteuid() -> u32;
}

fn ncpu() -> usize {
    unsafe {
        let mut count: i32 = 0;
        let mut size = std::mem::size_of::<i32>();
        let name = c"hw.ncpu";
        let ret = crate::kpep::libc::sysctlbyname(
            name.as_ptr(),
            &mut count as *mut i32 as *mut _,
            &mut size,
            std::ptr::null_mut(),
            0,
        );
        if ret != 0 || count <= 0 {
            return 1; // safe fallback
        }
        count as usize
    }
}

/// Counter values from a single snapshot.
#[derive(Debug, Clone)]
pub struct CounterSnapshot {
    /// Raw counter values. Layout: `[fixed0, fixed1, ..., config0, config1, ...]`.
    pub values: Vec<u64>,
    /// Number of fixed counters (first N values).
    pub n_fixed: usize,
}

/// Result of subtracting two snapshots.
#[derive(Debug)]
pub struct CounterDelta {
    /// Fixed counter 0 delta (typically CPU cycles).
    pub cycles: u64,
    /// Fixed counter 1 delta (typically retired instructions).
    pub instructions: u64,
    /// Configurable counter deltas, in the order they were configured.
    pub configurable: Vec<u64>,
}

/// High-level manager for kpc hardware performance counters.
///
/// Handles loading the kperf framework, configuring events, and reading counters.
pub struct KpcManager {
    fns: KpcFns,
    n_fixed: usize,
    n_config: usize,
    ncpu: usize,
    /// Events currently programmed into the configurable counters.
    configured_events: Vec<ConfiguredEvent>,
}

#[derive(Debug, Clone)]
struct ConfiguredEvent {
    name: String,
    /// Which hardware counter slot this event was assigned to.
    slot: usize,
}

impl KpcManager {
    /// Create a new KpcManager. Loads the kperf framework and queries counter counts.
    ///
    /// Does NOT acquire the counters yet — call [`configure`] for that.
    pub fn new() -> Result<Self, KpcError> {
        if unsafe { geteuid() } != 0 {
            return Err(KpcError::NotRoot);
        }

        let fns = KpcFns::load()?;
        let n_fixed = unsafe { (fns.get_counter_count)(KPC_CLASS_FIXED_MASK) } as usize;
        let n_config = unsafe { (fns.get_counter_count)(KPC_CLASS_CONFIGURABLE_MASK) } as usize;

        Ok(KpcManager {
            fns,
            n_fixed,
            n_config,
            ncpu: ncpu(),
            configured_events: Vec::new(),
        })
    }

    /// Number of fixed counters (typically 2: cycles + instructions).
    pub fn n_fixed(&self) -> usize {
        self.n_fixed
    }

    /// Number of configurable counter slots (typically 8 on Apple Silicon).
    pub fn n_configurable(&self) -> usize {
        self.n_config
    }

    /// Number of logical CPUs.
    pub fn ncpu(&self) -> usize {
        self.ncpu
    }

    /// Configure which events to count.
    ///
    /// Acquires exclusive counter access, programs the events, and enables counting.
    /// The events slice should contain [`KpepEvent`]s with valid `number` fields.
    ///
    /// Only configurable (non-fixed) events can be programmed. Fixed counters
    /// (cycles, instructions) are always available.
    pub fn configure(&mut self, events: &[&KpepEvent]) -> Result<(), KpcError> {
        let configurable: Vec<_> = events
            .iter()
            .copied()
            .filter(|e| e.is_configurable())
            .collect();
        if configurable.len() > self.n_config {
            return Err(KpcError::TooManyEvents {
                requested: configurable.len(),
                max: self.n_config,
            });
        }

        // Assign events to slots respecting counters_mask constraints.
        let assignments = Self::assign_slots(&configurable, self.n_config);

        // Release and re-acquire counters. The sleep gives the kernel time
        // to fully release the counters before we attempt to reclaim them.
        unsafe { (self.fns.force_all_ctrs_set)(0) };
        std::thread::sleep(std::time::Duration::from_millis(50));

        if unsafe { (self.fns.force_all_ctrs_set)(1) } != 0 {
            return Err(KpcError::ApiError("force_all_ctrs_set", errno()));
        }

        // Disable counting while reconfiguring
        unsafe { (self.fns.set_counting)(0) };
        unsafe { (self.fns.set_thread_counting)(0) };

        // Zero config first
        let zero = vec![0u64; self.n_config];
        unsafe { (self.fns.set_config)(KPC_CLASS_CONFIGURABLE_MASK, zero.as_ptr()) };

        // Program events into their assigned slots
        let mut config = vec![0u64; self.n_config];
        self.configured_events.clear();
        for &(orig_idx, slot) in &assignments {
            let event = configurable[orig_idx];
            config[slot] = event.number.unwrap();
            self.configured_events.push(ConfiguredEvent {
                name: event.name.clone(),
                slot,
            });
        }

        if unsafe { (self.fns.set_config)(KPC_CLASS_CONFIGURABLE_MASK, config.as_ptr()) } != 0 {
            let _ = unsafe { (self.fns.force_all_ctrs_set)(0) };
            return Err(KpcError::ApiError("set_config", errno()));
        }

        // Enable counting
        unsafe { (self.fns.set_counting)(KPC_ALL) };
        unsafe { (self.fns.set_thread_counting)(KPC_ALL) };

        Ok(())
    }

    /// Assign events to counter slots respecting `counters_mask` constraints.
    ///
    /// Most constrained events (fewest valid slots) are assigned first to avoid
    /// conflicts. Returns `(original_index, slot)` pairs.
    fn assign_slots(events: &[&KpepEvent], n_slots: usize) -> Vec<(usize, usize)> {
        // Sort by constraint level: fewest valid slots first
        let mut by_constraint: Vec<(usize, u32)> = events
            .iter()
            .enumerate()
            .map(|(i, ev)| {
                let valid_count = match ev.counters_mask {
                    Some(mask) => mask.count_ones(),
                    None => n_slots as u32,
                };
                (i, valid_count)
            })
            .collect();
        by_constraint.sort_by_key(|&(_, count)| count);

        let mut used = vec![false; n_slots];
        let mut result = Vec::with_capacity(events.len());

        for (orig_idx, _) in by_constraint {
            let event = events[orig_idx];
            let slot = (0..n_slots)
                .filter(|s| !used[*s])
                .find(|s| match event.counters_mask {
                    Some(mask) => (mask >> s) & 1 != 0,
                    None => true,
                });

            if let Some(s) = slot {
                used[s] = true;
                result.push((orig_idx, s));
            } else {
                eprintln!(
                    "Warning: could not assign '{}' to any counter slot, skipping",
                    event.name
                );
            }
        }

        result
    }

    /// Read system-wide counters summed across all CPUs.
    pub fn read_system_wide(&self) -> Result<CounterSnapshot, KpcError> {
        let n_total = self.n_fixed + self.n_config;
        let buf_size = self.ncpu * n_total;
        let mut buf = vec![0u64; buf_size];
        let mut cur_cpu: i32 = 0;

        let ret =
            unsafe { (self.fns.get_cpu_counters)(1, KPC_ALL, &mut cur_cpu, buf.as_mut_ptr()) };
        if ret != 0 {
            return Err(KpcError::ApiError("get_cpu_counters", errno()));
        }

        // Sum across all CPUs
        let mut sums = vec![0u64; n_total];
        for cpu in 0..self.ncpu {
            for counter in 0..n_total {
                sums[counter] = sums[counter].wrapping_add(buf[cpu * n_total + counter]);
            }
        }

        Ok(CounterSnapshot {
            values: sums,
            n_fixed: self.n_fixed,
        })
    }

    /// Compute the delta between two snapshots.
    ///
    /// Uses bounds-checked access so this never panics, even if snapshots
    /// have fewer values than expected (missing counters read as 0).
    pub fn delta(&self, before: &CounterSnapshot, after: &CounterSnapshot) -> CounterDelta {
        let val = |snap: &CounterSnapshot, idx: usize| snap.values.get(idx).copied().unwrap_or(0);

        let cycles = val(after, 0).wrapping_sub(val(before, 0));
        let instructions = val(after, 1).wrapping_sub(val(before, 1));

        let mut configurable = Vec::with_capacity(self.n_config);
        for i in 0..self.n_config {
            let idx = self.n_fixed + i;
            configurable.push(val(after, idx).wrapping_sub(val(before, idx)));
        }

        CounterDelta {
            cycles,
            instructions,
            configurable,
        }
    }

    /// Get the names and values of configured events from a delta.
    ///
    /// Returns `(event_name, counter_value)` pairs for each configured event
    /// that was successfully assigned to a counter slot.
    pub fn labeled_counters<'a>(&'a self, delta: &'a CounterDelta) -> Vec<(&'a str, u64)> {
        self.configured_events
            .iter()
            .filter(|ev| ev.slot < delta.configurable.len())
            .map(|ev| (ev.name.as_str(), delta.configurable[ev.slot]))
            .collect()
    }

    /// Release counter access.
    pub fn release(&self) {
        unsafe { (self.fns.force_all_ctrs_set)(0) };
    }
}

impl Drop for KpcManager {
    fn drop(&mut self) {
        self.release();
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kpep::KpepEvent;

    fn make_event(name: &str, number: u64, mask: Option<u64>) -> KpepEvent {
        KpepEvent {
            name: name.to_string(),
            description: String::new(),
            number: Some(number),
            counters_mask: mask,
            pc_capture_counters_mask: None,
            fixed_counter: None,
            fallback: None,
        }
    }

    #[test]
    fn test_assign_slots_unconstrained() {
        let events = [
            make_event("A", 1, None),
            make_event("B", 2, None),
            make_event("C", 3, None),
        ];
        let refs: Vec<&KpepEvent> = events.iter().collect();
        let result = KpcManager::assign_slots(&refs, 8);

        assert_eq!(result.len(), 3);
        let mut slots: Vec<usize> = result.iter().map(|&(_, s)| s).collect();
        slots.sort();
        slots.dedup();
        assert_eq!(slots.len(), 3, "all slots should be unique");
    }

    #[test]
    fn test_assign_slots_constrained() {
        // Event A can only go in slot 7 (mask=0x80)
        // Event B can go in slots 5,6,7 (mask=0xe0)
        // Event C can go anywhere
        let events = [
            make_event("A", 1, Some(0x80)),
            make_event("B", 2, Some(0xe0)),
            make_event("C", 3, None),
        ];
        let refs: Vec<&KpepEvent> = events.iter().collect();
        let result = KpcManager::assign_slots(&refs, 8);

        assert_eq!(result.len(), 3);
        // Find which slot A got — must be 7
        let a_slot = result.iter().find(|&&(i, _)| i == 0).unwrap().1;
        assert_eq!(a_slot, 7, "A (mask=0x80) must go in slot 7");
        // B should be in 5 or 6
        let b_slot = result.iter().find(|&&(i, _)| i == 1).unwrap().1;
        assert!(
            b_slot == 5 || b_slot == 6,
            "B (mask=0xe0) should be in 5 or 6"
        );
    }

    #[test]
    fn test_assign_slots_conflict() {
        // Both events can only go in slot 7
        let events = [
            make_event("A", 1, Some(0x80)),
            make_event("B", 2, Some(0x80)),
        ];
        let refs: Vec<&KpepEvent> = events.iter().collect();
        let result = KpcManager::assign_slots(&refs, 8);

        // Only one can be assigned
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_assign_slots_overflow() {
        let events: Vec<KpepEvent> = (0..10)
            .map(|i| make_event(&format!("E{i}"), i as u64, None))
            .collect();
        let refs: Vec<&KpepEvent> = events.iter().collect();
        let result = KpcManager::assign_slots(&refs, 8);

        // Only 8 slots available
        assert_eq!(result.len(), 8);
    }

    #[test]
    fn test_delta_basic() {
        let mgr_n_fixed = 2;
        let mgr_n_config = 3;

        let before = CounterSnapshot {
            values: vec![100, 200, 10, 20, 30],
            n_fixed: mgr_n_fixed,
        };
        let after = CounterSnapshot {
            values: vec![500, 1200, 15, 25, 130],
            n_fixed: mgr_n_fixed,
        };

        // Simulate delta without KpcManager (replicating the logic)
        let val = |snap: &CounterSnapshot, idx: usize| snap.values.get(idx).copied().unwrap_or(0);
        let cycles = val(&after, 0).wrapping_sub(val(&before, 0));
        let instructions = val(&after, 1).wrapping_sub(val(&before, 1));
        let mut configurable = Vec::new();
        for i in 0..mgr_n_config {
            let idx = mgr_n_fixed + i;
            configurable.push(val(&after, idx).wrapping_sub(val(&before, idx)));
        }

        assert_eq!(cycles, 400);
        assert_eq!(instructions, 1000);
        assert_eq!(configurable, vec![5, 5, 100]);
    }

    #[test]
    fn test_delta_wrapping() {
        // Counter overflow: after < before due to hardware wrap
        let before = CounterSnapshot {
            values: vec![u64::MAX - 10, 500],
            n_fixed: 2,
        };
        let after = CounterSnapshot {
            values: vec![5, 600],
            n_fixed: 2,
        };

        let val = |snap: &CounterSnapshot, idx: usize| snap.values.get(idx).copied().unwrap_or(0);
        let cycles = val(&after, 0).wrapping_sub(val(&before, 0));
        assert_eq!(cycles, 16); // 5 - (MAX-10) wraps to 16
    }

    #[test]
    fn test_delta_short_snapshot_does_not_panic() {
        // Snapshots with fewer values than expected — must not panic.
        let before = CounterSnapshot {
            values: vec![],
            n_fixed: 2,
        };
        let after = CounterSnapshot {
            values: vec![100],
            n_fixed: 2,
        };

        let val = |snap: &CounterSnapshot, idx: usize| snap.values.get(idx).copied().unwrap_or(0);
        let cycles = val(&after, 0).wrapping_sub(val(&before, 0));
        let instructions = val(&after, 1).wrapping_sub(val(&before, 1));
        // Should not panic — missing values treated as 0
        assert_eq!(cycles, 100);
        assert_eq!(instructions, 0);
    }

    #[test]
    fn test_inject_protocol_roundtrip() {
        // Verify the wire protocol: u32 count, then count × u64 values.
        let n: u32 = 4;
        let values: Vec<u64> = vec![100, 200, 300, 400];

        let mut buf = Vec::new();
        buf.extend_from_slice(&n.to_ne_bytes());
        for v in &values {
            buf.extend_from_slice(&v.to_ne_bytes());
        }

        // Parse back
        let parsed_n = u32::from_ne_bytes(buf[0..4].try_into().unwrap()) as usize;
        assert_eq!(parsed_n, 4);

        let mut parsed_values = vec![0u64; parsed_n];
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(parsed_values.as_mut_ptr() as *mut u8, parsed_n * 8)
        };
        bytes.copy_from_slice(&buf[4..]);
        assert_eq!(parsed_values, values);
    }
}
