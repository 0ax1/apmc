//! Parse Apple's kpep (kernel performance event) database.
//!
//! The kpep database files live at `/usr/share/kpep/` and describe all PMC events
//! available on a given CPU. Each file is a binary plist keyed by CPU type/family.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use plist::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum KpepError {
    #[error("kpep database not found for cpu_type=0x{cpu_type:x} cpu_subtype={cpu_subtype} cpu_family=0x{cpu_family:x}")]
    DatabaseNotFound {
        cpu_type: u32,
        cpu_subtype: u32,
        cpu_family: u32,
    },
    #[error("failed to read kpep database at {path}: {source}")]
    ReadError {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse kpep plist: {0}")]
    ParseError(#[from] plist::Error),
    #[error("unexpected plist structure: {0}")]
    StructureError(String),
    #[error("sysctl failed: {0}")]
    SysctlError(String),
}

/// A single PMC event definition from the kpep database.
#[derive(Debug, Clone)]
pub struct KpepEvent {
    /// Event name (e.g., "L1D_CACHE_MISS_LD").
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// PMC event number (the raw selector programmed into the counter config register).
    /// `None` for fixed counters that have no programmable event number.
    pub number: Option<u64>,
    /// Bitmask of which configurable counter slots can count this event.
    /// `None` means any slot.
    pub counters_mask: Option<u64>,
    /// Bitmask of which counters support PC capture (IP sampling) for this event.
    pub pc_capture_counters_mask: Option<u64>,
    /// If this is a fixed counter, its index (0 = cycles, 1 = instructions, etc.).
    pub fixed_counter: Option<u64>,
    /// Fallback event name if this event's fixed counter isn't available.
    pub fallback: Option<String>,
}

impl KpepEvent {
    /// Whether this event is a fixed (non-configurable) counter.
    pub fn is_fixed(&self) -> bool {
        self.fixed_counter.is_some()
    }

    /// Whether this event is configurable (can be programmed into a counter slot).
    pub fn is_configurable(&self) -> bool {
        self.number.is_some() && self.fixed_counter.is_none()
    }
}

/// CPU metadata from the kpep database.
#[derive(Debug, Clone)]
pub struct CpuInfo {
    /// CPU architecture (e.g., "arm64").
    pub architecture: String,
    /// Marketing name (e.g., "Apple M2").
    pub marketing_name: String,
    /// Number of fixed (hardwired) counters.
    pub fixed_counters: u64,
    /// Configurable counter capacity.
    pub config_counters: u64,
    /// Event name aliases (e.g., "Cycles" -> "FIXED_CYCLES").
    pub aliases: HashMap<String, String>,
}

/// Parsed kpep database for a specific CPU.
#[derive(Debug)]
pub struct KpepDatabase {
    /// Database name/identifier.
    pub name: String,
    /// CPU metadata.
    pub cpu: CpuInfo,
    /// All available PMC events.
    events: Vec<KpepEvent>,
}

impl KpepDatabase {
    /// Load the kpep database for the currently running CPU.
    ///
    /// Discovers the current CPU type/family via sysctl and reads the matching
    /// database file from `/usr/share/kpep/`.
    pub fn load_current_cpu() -> Result<Self, KpepError> {
        let (cpu_type, cpu_subtype, cpu_family) = read_cpu_info()?;
        let path = find_database_path(cpu_type, cpu_subtype, cpu_family)?;
        Self::load_from_path(&path)
    }

    /// Load a kpep database from a specific plist file.
    pub fn load_from_path(path: &Path) -> Result<Self, KpepError> {
        let value = Value::from_file(path).map_err(|e| KpepError::ReadError {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::Other, e),
        })?;
        Self::parse(value)
    }

    /// All events in the database.
    pub fn events(&self) -> &[KpepEvent] {
        &self.events
    }

    /// Only configurable (non-fixed) events.
    pub fn configurable_events(&self) -> impl Iterator<Item = &KpepEvent> {
        self.events.iter().filter(|e| e.is_configurable())
    }

    /// Only fixed counter events.
    pub fn fixed_events(&self) -> impl Iterator<Item = &KpepEvent> {
        self.events.iter().filter(|e| e.is_fixed())
    }

    /// Look up an event by name.
    pub fn event_by_name(&self, name: &str) -> Option<&KpepEvent> {
        // Check aliases first
        let resolved = self
            .cpu
            .aliases
            .get(name)
            .map(|s| s.as_str())
            .unwrap_or(name);
        self.events.iter().find(|e| e.name == resolved)
    }

    fn parse(root: Value) -> Result<Self, KpepError> {
        let dict = root
            .as_dictionary()
            .ok_or_else(|| KpepError::StructureError("root is not a dict".into()))?;

        let name = dict
            .get("name")
            .and_then(|v| v.as_string())
            .unwrap_or("unknown")
            .to_string();

        let system = dict
            .get("system")
            .and_then(|v| v.as_dictionary())
            .ok_or_else(|| KpepError::StructureError("missing system".into()))?;

        let cpu_dict = system
            .get("cpu")
            .and_then(|v| v.as_dictionary())
            .ok_or_else(|| KpepError::StructureError("missing system.cpu".into()))?;

        // Parse CPU metadata
        let architecture = cpu_dict
            .get("architecture")
            .and_then(|v| v.as_string())
            .unwrap_or("unknown")
            .to_string();

        let marketing_name = cpu_dict
            .get("marketing_name")
            .and_then(|v| v.as_string())
            .unwrap_or(&name)
            .to_string();

        let fixed_counters = cpu_dict
            .get("fixed_counters")
            .and_then(|v| v.as_unsigned_integer())
            .unwrap_or(0);

        let config_counters = cpu_dict
            .get("config_counters")
            .and_then(|v| v.as_unsigned_integer())
            .unwrap_or(0);

        let mut aliases = HashMap::new();
        if let Some(alias_dict) = cpu_dict.get("aliases").and_then(|v| v.as_dictionary()) {
            for (key, val) in alias_dict {
                if let Some(s) = val.as_string() {
                    aliases.insert(key.clone(), s.to_string());
                }
            }
        }

        let cpu = CpuInfo {
            architecture,
            marketing_name,
            fixed_counters,
            config_counters,
            aliases,
        };

        // Parse events
        let events_dict = cpu_dict
            .get("events")
            .and_then(|v| v.as_dictionary())
            .ok_or_else(|| KpepError::StructureError("missing system.cpu.events".into()))?;

        let mut events = Vec::with_capacity(events_dict.len());
        for (name, val) in events_dict {
            let ev_dict = match val.as_dictionary() {
                Some(d) => d,
                None => continue,
            };

            let description = ev_dict
                .get("description")
                .and_then(|v| v.as_string())
                .unwrap_or("")
                .to_string();

            let number = ev_dict
                .get("number")
                .and_then(|v| v.as_unsigned_integer());

            let counters_mask = ev_dict
                .get("counters_mask")
                .and_then(|v| v.as_unsigned_integer());

            let pc_capture_counters_mask = ev_dict
                .get("pc_capture_counters_mask")
                .and_then(|v| v.as_unsigned_integer());

            let fixed_counter = ev_dict
                .get("fixed_counter")
                .and_then(|v| v.as_unsigned_integer());

            let fallback = ev_dict
                .get("fallback")
                .and_then(|v| v.as_string())
                .map(|s| s.to_string());

            events.push(KpepEvent {
                name: name.clone(),
                description,
                number,
                counters_mask,
                pc_capture_counters_mask,
                fixed_counter,
                fallback,
            });
        }

        events.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(KpepDatabase { name, cpu, events })
    }
}

/// Read CPU type, subtype, and family from sysctl.
fn read_cpu_info() -> Result<(u32, u32, u32), KpepError> {
    fn read_sysctl_u32(name: &str) -> Result<u32, KpepError> {
        let mut val: u32 = 0;
        let mut size = std::mem::size_of::<u32>();
        let c_name =
            std::ffi::CString::new(name).map_err(|e| KpepError::SysctlError(e.to_string()))?;
        let ret = unsafe {
            libc::sysctlbyname(
                c_name.as_ptr(),
                &mut val as *mut u32 as *mut _,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if ret != 0 {
            return Err(KpepError::SysctlError(format!(
                "{}: errno {}",
                name,
                std::io::Error::last_os_error()
            )));
        }
        Ok(val)
    }

    let cpu_type = read_sysctl_u32("hw.cputype")?;
    let cpu_subtype = read_sysctl_u32("hw.cpusubtype")?;
    // cpufamily is signed but we treat as u32
    let cpu_family = read_sysctl_u32("hw.cpufamily")?;

    Ok((cpu_type, cpu_subtype, cpu_family))
}

/// Find the kpep database file matching the given CPU identifiers.
fn find_database_path(cpu_type: u32, cpu_subtype: u32, cpu_family: u32) -> Result<PathBuf, KpepError> {
    let filename = format!(
        "cpu_{:x}_{:x}_{:x}.plist",
        cpu_type, cpu_subtype, cpu_family
    );
    let path = Path::new("/usr/share/kpep").join(&filename);
    if path.exists() {
        return Ok(path);
    }

    // Fallback: try without subtype in the filename by scanning all files
    let kpep_dir = Path::new("/usr/share/kpep");
    if kpep_dir.is_dir() {
        let prefix = format!("cpu_{:x}_{:x}_", cpu_type, cpu_subtype);
        if let Ok(entries) = std::fs::read_dir(kpep_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(&prefix) && name_str.ends_with(".plist") {
                    return Ok(entry.path());
                }
            }
        }
    }

    Err(KpepError::DatabaseNotFound {
        cpu_type,
        cpu_subtype,
        cpu_family,
    })
}

// Need libc for sysctlbyname
pub(crate) mod libc {
    extern "C" {
        pub fn sysctlbyname(
            name: *const std::ffi::c_char,
            oldp: *mut std::ffi::c_void,
            oldlenp: *mut usize,
            newp: *mut std::ffi::c_void,
            newlen: usize,
        ) -> std::ffi::c_int;
    }
}
