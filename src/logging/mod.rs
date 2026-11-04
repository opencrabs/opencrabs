//! Logging and Debug System

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) mod crash;
#[cfg(all(unix, not(all(target_os = "linux", target_arch = "x86_64"))))]
pub(crate) mod crash_unsupported;
pub(crate) mod logger;
pub(crate) mod panic_record;
pub mod reader;
pub(crate) mod redact;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub use crash::install_crash_handler;
#[cfg(all(unix, not(all(target_os = "linux", target_arch = "x86_64"))))]
pub use crash_unsupported::install_crash_handler;
pub use logger::*;
