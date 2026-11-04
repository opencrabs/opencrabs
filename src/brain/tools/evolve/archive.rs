//! In-memory extraction of the released binary from its archive
//! (`.tar.gz` on Linux/macOS, `.zip` on Windows).

use super::super::error::{Result, ToolError};

/// Extract a named file from a .tar.gz archive in memory.
pub(super) fn extract_from_tar_gz(data: &[u8], file_name: &str) -> Result<Vec<u8>> {
    use std::io::Read;

    let decoder = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive
        .entries()
        .map_err(|e| ToolError::Execution(format!("Failed to read archive: {}", e)))?
    {
        let mut entry =
            entry.map_err(|e| ToolError::Execution(format!("Failed to read entry: {}", e)))?;

        let path = entry
            .path()
            .map_err(|e| ToolError::Execution(format!("Invalid path in archive: {}", e)))?
            .to_path_buf();

        if path.file_name().and_then(|n| n.to_str()) == Some(file_name) {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| ToolError::Execution(format!("Failed to extract: {}", e)))?;
            return Ok(buf);
        }
    }

    Err(ToolError::Execution(format!(
        "'{}' not found in archive",
        file_name
    )))
}

/// Extract a named file from a .zip archive in memory.
pub(super) fn extract_from_zip(data: &[u8], file_name: &str) -> Result<Vec<u8>> {
    use std::io::Read;

    let reader = std::io::Cursor::new(data);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| ToolError::Execution(format!("Failed to read zip: {}", e)))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| ToolError::Execution(format!("Failed to read zip entry: {}", e)))?;

        let name = file
            .name()
            .map_err(|e| ToolError::Execution(format!("Failed to read zip entry name: {}", e)))?
            .into_owned();
        if name.ends_with(file_name) {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)
                .map_err(|e| ToolError::Execution(format!("Failed to extract: {}", e)))?;
            return Ok(buf);
        }
    }

    Err(ToolError::Execution(format!(
        "'{}' not found in zip",
        file_name
    )))
}
