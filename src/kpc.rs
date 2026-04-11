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
type KpcGetConfigFn = unsafe extern "C" fn(u32, *mut u64) -> i32;
type KpcGetThreadCountersFn = unsafe extern "C" fn(i32, u32, *mut u64) -> i32;
type KpcGetCpuCountersFn = unsafe extern "C" fn(i32, u32, *mut i32, *mut u64) -> i32;

/// Loaded kpc function table. All pointers are non-null after successful construction.
struct KpcFns {
    _handle: *mut c_void,
    get_counter_count: KpcGetCounterCountFn,
    force_all_ctrs_set: KpcForceAllCtrsSetFn,
    set_counting: KpcSetCountingFn,
    set_thread_counting: KpcSetThreadCountingFn,
    set_config: KpcSetConfigFn,
    get_config: KpcGetConfigFn,
    get_thread_counters: KpcGetThreadCountersFn,
    get_cpu_counters: KpcGetCpuCountersFn,
}

// dlopen/dlsym access to kperf is Send+Sync since we only call from one place at a time.
unsafe impl Send for KpcFns {}
unsafe impl Sync for KpcFns {}

impl KpcFns {
    fn load() -> Result<Self, KpcError> {
        let path = c"/System/Library/PrivateFrameworks/kperf.framework/kperf";
        let handle = unsafe { dlopen(path.as_ptr(), 1) }; // RTLD_LAZY = 1
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
            get_config: load_sym!(kpc_get_config, KpcGetConfigFn),
            get_thread_counters: load_sym!(kpc_get_thread_counters, KpcGetThreadCountersFn),
            get_cpu_counters: load_sym!(kpc_get_cpu_counters, KpcGetCpuCountersFn),
        })
    }
}

extern "C" {
    fn dlopen(path: *const std::ffi::c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
    fn geteuid() -> u32;
    fn task_for_pid(target_tport: u32, pid: i32, t: *mut u32) -> i32;
    fn task_threads(target_task: u32, act_list: *mut *mut u32, act_list_cnt: *mut u32) -> i32;
    fn mach_task_self() -> u32;
    fn vm_deallocate(target_task: u32, address: usize, size: usize) -> i32;
}

fn ncpu() -> usize {
    unsafe {
        let mut ncpu: i32 = 0;
        let mut size = std::mem::size_of::<i32>();
        let name = c"hw.ncpu";
        crate::kpep::libc::sysctlbyname(
            name.as_ptr(),
            &mut ncpu as *mut i32 as *mut _,
            &mut size,
            std::ptr::null_mut(),
            0,
        );
        ncpu as usize
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
    /// Map from config slot index to the actual event that was programmed
    /// (after readback to account for kernel modifications).
    actual_config: Vec<u64>,
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
            actual_config: Vec::new(),
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
        let configurable: Vec<_> = events.iter().filter(|e| e.is_configurable()).collect();
        if configurable.len() > self.n_config {
            return Err(KpcError::TooManyEvents {
                requested: configurable.len(),
                max: self.n_config,
            });
        }

        // Assign events to slots respecting counters_mask constraints.
        let assignments = Self::assign_slots(&configurable, self.n_config);

        // Release and re-acquire counters
        unsafe { (self.fns.force_all_ctrs_set)(0) };
        std::thread::sleep(std::time::Duration::from_millis(50));

        if unsafe { (self.fns.force_all_ctrs_set)(1) } != 0 {
            return Err(KpcError::ApiError("force_all_ctrs_set", errno()));
        }

        // Disable counting while reconfiguring
        unsafe { (self.fns.set_counting)(0) };
        unsafe { (self.fns.set_thread_counting)(0) };

        // Zero config first
        let zero = [0u64; 8];
        unsafe { (self.fns.set_config)(KPC_CLASS_CONFIGURABLE_MASK, zero.as_ptr()) };

        // Program events into their assigned slots
        let mut config = [0u64; 8];
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

        // Read back actual config (kernel may modify or reject some slots)
        let mut actual = [0u64; 8];
        unsafe { (self.fns.get_config)(KPC_CLASS_CONFIGURABLE_MASK, actual.as_mut_ptr()) };
        self.actual_config = actual.to_vec();

        // Enable counting
        unsafe { (self.fns.set_counting)(KPC_ALL) };
        unsafe { (self.fns.set_thread_counting)(KPC_ALL) };

        Ok(())
    }

    /// Assign events to counter slots respecting `counters_mask` constraints.
    ///
    /// Most constrained events (fewest valid slots) are assigned first to avoid
    /// conflicts. Returns `(original_index, slot)` pairs.
    fn assign_slots(events: &[&&KpepEvent], n_slots: usize) -> Vec<(usize, usize)> {
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
            let event = &events[orig_idx];
            let slot = (0..n_slots).filter(|s| !used[*s]).find(|s| match event.counters_mask {
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

        let ret = unsafe {
            (self.fns.get_cpu_counters)(1, KPC_ALL, &mut cur_cpu, buf.as_mut_ptr())
        };
        if ret != 0 {
            return Err(KpcError::ApiError("get_cpu_counters", errno()));
        }

        // Sum across all CPUs
        let mut sums = vec![0u64; n_total];
        for cpu in 0..self.ncpu {
            for c in 0..n_total {
                sums[c] = sums[c].wrapping_add(buf[cpu * n_total + c]);
            }
        }

        Ok(CounterSnapshot {
            values: sums,
            n_fixed: self.n_fixed,
        })
    }

    /// Read counters for the current thread only.
    pub fn read_thread(&self) -> Result<CounterSnapshot, KpcError> {
        self.read_thread_by_id(0)
    }

    /// Read counters for a specific Mach thread ID (0 = current thread).
    pub fn read_thread_by_id(&self, tid: u32) -> Result<CounterSnapshot, KpcError> {
        let n_total = self.n_fixed + self.n_config;
        let mut buf = vec![0u64; n_total];

        let ret =
            unsafe { (self.fns.get_thread_counters)(tid as i32, n_total as u32, buf.as_mut_ptr()) };
        if ret != 0 {
            return Err(KpcError::ApiError("get_thread_counters", errno()));
        }

        Ok(CounterSnapshot {
            values: buf,
            n_fixed: self.n_fixed,
        })
    }

    /// Read counters summed across all threads of a process (by PID).
    ///
    /// Uses Mach `task_for_pid` and `task_threads` to enumerate the process's
    /// threads, then reads each thread's counters via `kpc_get_thread_counters`.
    pub fn read_process(&self, pid: i32) -> Result<CounterSnapshot, KpcError> {
        let n_total = self.n_fixed + self.n_config;

        let mut task: u32 = 0;
        let ret = unsafe { task_for_pid(mach_task_self(), pid, &mut task) };
        if ret != 0 {
            return Err(KpcError::ApiError("task_for_pid", ret));
        }

        let mut thread_list: *mut u32 = std::ptr::null_mut();
        let mut thread_count: u32 = 0;
        let ret = unsafe { task_threads(task, &mut thread_list, &mut thread_count) };
        if ret != 0 {
            return Err(KpcError::ApiError("task_threads", ret));
        }

        let threads = unsafe { std::slice::from_raw_parts(thread_list, thread_count as usize) };

        let mut sums = vec![0u64; n_total];
        for &tid in threads {
            let mut buf = vec![0u64; n_total];
            let ret = unsafe {
                (self.fns.get_thread_counters)(tid as i32, n_total as u32, buf.as_mut_ptr())
            };
            if ret == 0 {
                for i in 0..n_total {
                    sums[i] = sums[i].wrapping_add(buf[i]);
                }
            }
        }

        // Free the thread list allocated by task_threads
        if !thread_list.is_null() {
            unsafe {
                vm_deallocate(
                    mach_task_self(),
                    thread_list as usize,
                    thread_count as usize * std::mem::size_of::<u32>(),
                );
            }
        }

        Ok(CounterSnapshot {
            values: sums,
            n_fixed: self.n_fixed,
        })
    }

    /// Compute the delta between two snapshots.
    pub fn delta(&self, before: &CounterSnapshot, after: &CounterSnapshot) -> CounterDelta {
        let cycles = after.values[0].wrapping_sub(before.values[0]);
        let instructions = after.values[1].wrapping_sub(before.values[1]);

        let mut configurable = Vec::new();
        for i in 0..self.n_config {
            let idx = self.n_fixed + i;
            let d = after.values[idx].wrapping_sub(before.values[idx]);
            configurable.push(d);
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
    /// that was actually programmed (per readback from the kernel).
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
