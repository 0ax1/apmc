//! Apple Silicon hardware performance counter access via kpc/kpep.
//!
//! This crate provides:
//! - **kpep**: Parse Apple's PMC event database to discover all available hardware events
//!   for the current CPU.
//! - **kpc**: Configure and read hardware performance counters via Apple's private kpc API.
//!
//! # Requirements
//! - macOS on Apple Silicon (M1/M2/M3/M4)
//! - Root privileges (`sudo`) for counter access
//! - SIP disabled for full configurable counter access
//!
//! # Example
//! ```no_run
//! use apmc::{KpcManager, kpep::KpepDatabase};
//!
//! let db = KpepDatabase::load_current_cpu().unwrap();
//! for event in db.events() {
//!     println!("{}: {}", event.name, event.description);
//! }
//! ```

pub mod kpc;
pub mod kpep;

pub use kpc::KpcManager;
pub use kpep::KpepDatabase;
