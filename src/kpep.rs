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
            source: std::io::Error::other(e),
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

            let number = ev_dict.get("number").and_then(|v| v.as_unsigned_integer());

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
fn find_database_path(
    cpu_type: u32,
    cpu_subtype: u32,
    cpu_family: u32,
) -> Result<PathBuf, KpepError> {
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

// Need libc for sysctlbyname.
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

#[cfg(test)]
mod tests {
    use super::*;
    use plist::{Dictionary, Value};

    fn make_fixed_event(name: &str, fixed_counter: u64) -> KpepEvent {
        KpepEvent {
            name: name.to_string(),
            description: "fixed counter".to_string(),
            number: None,
            counters_mask: None,
            pc_capture_counters_mask: None,
            fixed_counter: Some(fixed_counter),
            fallback: None,
        }
    }

    fn make_config_event(name: &str, number: u64) -> KpepEvent {
        KpepEvent {
            name: name.to_string(),
            description: "configurable event".to_string(),
            number: Some(number),
            counters_mask: None,
            pc_capture_counters_mask: None,
            fixed_counter: None,
            fallback: None,
        }
    }

    #[test]
    fn fixed_event_classification() {
        let event = make_fixed_event("FIXED_CYCLES", 0);
        assert!(event.is_fixed());
        assert!(!event.is_configurable());
    }

    #[test]
    fn configurable_event_classification() {
        let event = make_config_event("L1D_MISS", 42);
        assert!(event.is_configurable());
        assert!(!event.is_fixed());
    }

    #[test]
    fn event_with_both_fixed_and_number_is_fixed() {
        let event = KpepEvent {
            name: "DUAL".to_string(),
            description: String::new(),
            number: Some(42),
            counters_mask: None,
            pc_capture_counters_mask: None,
            fixed_counter: Some(0),
            fallback: None,
        };
        assert!(event.is_fixed());
        assert!(!event.is_configurable());
    }

    #[test]
    fn event_with_neither_is_neither() {
        let event = KpepEvent {
            name: "EMPTY".to_string(),
            description: String::new(),
            number: None,
            counters_mask: None,
            pc_capture_counters_mask: None,
            fixed_counter: None,
            fallback: None,
        };
        assert!(!event.is_fixed());
        assert!(!event.is_configurable());
    }

    /// Build a minimal kpep plist Value for testing the parser.
    fn build_test_plist(
        events: Vec<(&str, Option<u64>, Option<u64>)>,
        aliases: Vec<(&str, &str)>,
    ) -> Value {
        let mut events_dict = Dictionary::new();
        for (name, number, fixed_counter) in events {
            let mut ev = Dictionary::new();
            ev.insert(
                "description".to_string(),
                Value::String(format!("{name} desc")),
            );
            if let Some(n) = number {
                ev.insert("number".to_string(), Value::Integer(n.into()));
            }
            if let Some(fc) = fixed_counter {
                ev.insert("fixed_counter".to_string(), Value::Integer(fc.into()));
            }
            events_dict.insert(name.to_string(), Value::Dictionary(ev));
        }

        let mut alias_dict = Dictionary::new();
        for (alias, target) in aliases {
            alias_dict.insert(alias.to_string(), Value::String(target.to_string()));
        }

        let mut cpu = Dictionary::new();
        cpu.insert(
            "architecture".to_string(),
            Value::String("arm64".to_string()),
        );
        cpu.insert(
            "marketing_name".to_string(),
            Value::String("Test CPU".to_string()),
        );
        cpu.insert("fixed_counters".to_string(), Value::Integer(2.into()));
        cpu.insert("config_counters".to_string(), Value::Integer(8.into()));
        cpu.insert("events".to_string(), Value::Dictionary(events_dict));
        cpu.insert("aliases".to_string(), Value::Dictionary(alias_dict));

        let mut system = Dictionary::new();
        system.insert("cpu".to_string(), Value::Dictionary(cpu));

        let mut root = Dictionary::new();
        root.insert("name".to_string(), Value::String("test_db".to_string()));
        root.insert("system".to_string(), Value::Dictionary(system));

        Value::Dictionary(root)
    }

    #[test]
    fn parse_minimal_database() {
        let plist = build_test_plist(vec![("TEST_EVENT", Some(42), None)], vec![]);
        let db = KpepDatabase::parse(plist).unwrap();

        assert_eq!(db.name, "test_db");
        assert_eq!(db.cpu.marketing_name, "Test CPU");
        assert_eq!(db.cpu.architecture, "arm64");
        assert_eq!(db.cpu.fixed_counters, 2);
        assert_eq!(db.cpu.config_counters, 8);
        assert_eq!(db.events().len(), 1);
        assert_eq!(db.events()[0].name, "TEST_EVENT");
        assert_eq!(db.events()[0].number, Some(42));
    }

    #[test]
    fn parse_mixed_fixed_and_configurable() {
        let plist = build_test_plist(
            vec![
                ("FIXED_CYCLES", None, Some(0)),
                ("FIXED_INSTRUCTIONS", None, Some(1)),
                ("L1D_MISS", Some(10), None),
                ("BRANCH_MISS", Some(20), None),
            ],
            vec![],
        );
        let db = KpepDatabase::parse(plist).unwrap();

        let fixed: Vec<_> = db.fixed_events().collect();
        let config: Vec<_> = db.configurable_events().collect();

        assert_eq!(fixed.len(), 2);
        assert_eq!(config.len(), 2);
    }

    #[test]
    fn event_by_name_direct_lookup() {
        let plist = build_test_plist(vec![("L1D_MISS", Some(10), None)], vec![]);
        let db = KpepDatabase::parse(plist).unwrap();

        assert!(db.event_by_name("L1D_MISS").is_some());
        assert!(db.event_by_name("NONEXISTENT").is_none());
    }

    #[test]
    fn event_by_name_resolves_alias() {
        let plist = build_test_plist(
            vec![("FIXED_CYCLES", None, Some(0))],
            vec![("Cycles", "FIXED_CYCLES")],
        );
        let db = KpepDatabase::parse(plist).unwrap();

        let event = db.event_by_name("Cycles").unwrap();
        assert_eq!(event.name, "FIXED_CYCLES");
    }

    #[test]
    fn events_sorted_by_name() {
        let plist = build_test_plist(
            vec![
                ("ZEBRA", Some(3), None),
                ("ALPHA", Some(1), None),
                ("MIDDLE", Some(2), None),
            ],
            vec![],
        );
        let db = KpepDatabase::parse(plist).unwrap();

        let names: Vec<_> = db.events().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["ALPHA", "MIDDLE", "ZEBRA"]);
    }

    #[test]
    fn parse_rejects_non_dict_root() {
        let result = KpepDatabase::parse(Value::String("not a dict".to_string()));
        assert!(result.is_err());
    }

    #[test]
    fn load_current_cpu_succeeds() {
        // This test runs on macOS Apple Silicon only. It verifies the full
        // path: sysctl → find plist → parse events.
        let db = KpepDatabase::load_current_cpu().unwrap();
        assert!(!db.events().is_empty());
        assert!(db.cpu.fixed_counters > 0);
        assert!(db.cpu.config_counters > 0);
    }
}
