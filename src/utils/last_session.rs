//! The conversation the user was in, so the next launch opens it again.
//!
//! One implementation for every surface. The TUI has kept this file for a
//! while; the GUI read nothing, so a restart dropped the user into whatever
//! session the rail listed first even though the turn's rows were already in
//! the database (#2444). Two readers and two writers is exactly how the
//! surfaces drift, so both call these functions.

use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Name of the file in the OpenCrabs home. No extension: it holds a bare id.
const LAST_SESSION_FILE: &str = "last_session";

fn file_in(base: &Path) -> PathBuf {
    base.join(LAST_SESSION_FILE)
}

/// The last session id recorded under `base`, or `None` when nothing was
/// recorded or the file no longer parses. A malformed file is not an error
/// worth surfacing: the caller falls back to the newest session, exactly as
/// it would on a first run.
fn read_in(base: &Path) -> Option<Uuid> {
    let content = std::fs::read_to_string(file_in(base)).ok()?;
    Uuid::parse_str(content.trim()).ok()
}

/// Record `session_id` under `base`. A write failure is logged, never fatal:
/// losing the pointer costs the next launch one fallback, not the turn.
fn write_in(base: &Path, session_id: Uuid) {
    if let Err(e) = std::fs::write(file_in(base), session_id.to_string()) {
        tracing::warn!("Failed to persist last_session: {}", e);
    }
}

/// The last session id for the active profile.
pub fn read() -> Option<Uuid> {
    read_in(&crate::config::opencrabs_home())
}

/// Record the session the user just opened for the active profile.
pub fn write(session_id: Uuid) {
    write_in(&crate::config::opencrabs_home(), session_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch home under the system temp dir. The live `~/.opencrabs` is
    /// never touched: pointing the writer at it here would move the owner's
    /// own restore pointer as a side effect of `cargo test`.
    fn scratch_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "opencrabs-last-session-{tag}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("scratch home");
        dir
    }

    /// The id round-trips, and anything that is not a uuid reads as nothing,
    /// so a damaged file cannot break a launch.
    #[test]
    fn a_written_id_reads_back_and_garbage_reads_as_none() {
        let home = scratch_home("roundtrip");
        let id = Uuid::new_v4();
        write_in(&home, id);
        assert_eq!(read_in(&home), Some(id));

        std::fs::write(file_in(&home), "not-a-session\n").unwrap();
        assert_eq!(
            read_in(&home),
            None,
            "a damaged pointer falls back, never errors"
        );

        std::fs::write(file_in(&home), "  \n").unwrap();
        assert_eq!(read_in(&home), None, "an empty file names no session");

        let missing = scratch_home("missing");
        assert_eq!(read_in(&missing), None, "no file, no session");
    }

    /// The public pair resolves through the active home, so a caller cannot
    /// read one profile's pointer and write another's.
    #[test]
    fn the_public_pair_uses_the_active_home() {
        let home = scratch_home("public");
        let id = Uuid::new_v4();
        crate::config::profile::with_home_override(home.clone(), || {
            write(id);
            assert_eq!(read(), Some(id));
        });
        assert!(
            file_in(&home).exists(),
            "the write landed in the overridden home, not the live one"
        );
    }
}
