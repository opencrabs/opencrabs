//! Userbot document download: an explicit operator action, not part of the
//! passive watch loop.
//!
//! The watch loop stays receive-only by design; this module exists so an
//! operator at the CLI can pull a specific document through the same MTProto
//! session (the Bot API caps downloads at 20 MB; MTProto streams up to 2 GB).
//! Ported from the tgdl spike that recovered a 105 MB channel document on
//! 2026-10-12; wiring matches login.rs (grammers 0.10, verified against
//! vendored source).

use std::path::Path;

use anyhow::{Context, Result};
use grammers_client::Client;
use grammers_client::media::Media;
use grammers_client::message::Message;
use grammers_session::types::PeerRef;
use tokio::io::AsyncWriteExt;

use super::login::connect;
use crate::config::types::Config;

/// Messages walked before a discovery scan gives up.
///
/// `--list` and `latest` used to walk a channel's entire history when its
/// recent window held no documents. The scan stops here instead.
const SCAN_LIMIT: usize = 500;

/// Progress is reported every this many bytes, once per step.
const PROGRESS_STEP: u64 = 8 * 1024 * 1024;

/// Reduce an uploader-supplied document name to a single path component.
///
/// The uploader picks the filename and Telegram passes it through verbatim, so
/// the name cannot be joined onto the output directory as-is: a document named
/// `../../.ssh/authorized_keys` would otherwise land outside `--outdir`. Only
/// the final component is kept, which is what every downloader does with a
/// remote name, and a name that leaves nothing usable falls back to a fixed
/// one.
fn safe_file_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|part| part.to_str())
        .map(str::trim)
        .filter(|part| !part.is_empty());
    match base {
        Some(base) => base.to_string(),
        None => "unnamed.bin".to_string(),
    }
}

/// Stream the document into `path`, returning the byte count.
async fn stream_to_file(client: &Client, media: &Media, path: &Path, size: u64) -> Result<u64> {
    let mut file = tokio::fs::File::create(path)
        .await
        .with_context(|| format!("creating {}", path.display()))?;
    let mut stream = client.iter_download(media);
    let mut got: u64 = 0;
    let mut next_progress = PROGRESS_STEP;
    while let Some(chunk) = stream.next().await? {
        file.write_all(&chunk)
            .await
            .with_context(|| format!("writing {}", path.display()))?;
        got += chunk.len() as u64;
        // Progress for files where the wait is felt, once per step.
        if size >= PROGRESS_STEP && got >= next_progress {
            eprintln!("  {got}/{size} bytes");
            next_progress = (got / PROGRESS_STEP + 1) * PROGRESS_STEP;
        }
    }
    file.flush()
        .await
        .with_context(|| format!("flushing {}", path.display()))?;
    Ok(got)
}

/// Resolve `@username` or a numeric chat/channel id into a usable peer ref.
/// Numeric ids come from the dialog list (access hash included); usernames
/// resolve directly.
async fn resolve_peer(client: &Client, who: &str) -> Result<PeerRef> {
    if let Ok(id) = who.parse::<i64>() {
        let mut dialogs = client.iter_dialogs();
        while let Some(dialog) = dialogs.next().await? {
            let peer_id = dialog.peer_id();
            if peer_id.bot_api_dialog_id() == Some(id) || peer_id.bare_id() == Some(id) {
                return Ok(dialog.peer_ref());
            }
        }
        anyhow::bail!("peer {id} not found in the userbot's dialogs");
    }
    let username = who.strip_prefix('@').unwrap_or(who);
    let peer = client
        .resolve_username(username)
        .await
        .with_context(|| format!("resolving {who}"))?
        .ok_or_else(|| anyhow::anyhow!("cannot resolve peer: {who}"))?;
    peer.to_ref()
        .await
        .map_err(|error| anyhow::anyhow!("peer ref for {who} failed: {error}"))?
        .ok_or_else(|| anyhow::anyhow!("peer {who} has no usable access hash yet"))
}

/// Document name and size when the message carries one.
fn document_info(message: &Message) -> Option<(String, u64)> {
    if let Some(Media::Document(doc)) = message.media() {
        let name = doc.name().unwrap_or("unnamed.bin").to_string();
        Some((name, doc.size().unwrap_or(0) as u64))
    } else {
        None
    }
}

/// Print the most recent document-carrying messages for discovery.
async fn list_documents(client: &Client, peer: PeerRef, limit: usize) -> Result<()> {
    let mut iter = client.iter_messages(peer).limit(SCAN_LIMIT);
    let mut shown = 0usize;
    while let Some(message) = iter.next().await? {
        if shown >= limit {
            break;
        }
        if let Some((name, size)) = document_info(&message) {
            let text_head: String = message.text().chars().take(80).collect();
            println!(
                "msg_id={} size={} name={:?} text={:?}",
                message.id(),
                size,
                name,
                text_head
            );
            shown += 1;
        }
    }
    if shown == 0 {
        println!("(no documents in scanned window)");
    }
    Ok(())
}

/// Fetch one document message: explicit id, or the newest one in history.
async fn find_document(client: &Client, peer: PeerRef, message: &str) -> Result<Message> {
    if message == "latest" {
        let mut iter = client.iter_messages(peer).limit(SCAN_LIMIT);
        while let Some(m) = iter.next().await? {
            if document_info(&m).is_some() {
                return Ok(m);
            }
        }
        anyhow::bail!("no document found in the last {SCAN_LIMIT} messages");
    }
    let wanted: i32 = message
        .parse()
        .with_context(|| format!("bad message id: {message}"))?;
    let found = client.get_messages_by_id(peer, &[wanted]).await?;
    found
        .into_iter()
        .next()
        .flatten()
        .ok_or_else(|| anyhow::anyhow!("message {wanted} not found"))
}

/// `opencrabs channel userbot-download <peer> <msg_id|latest> [--outdir DIR]`
/// entry point. `--list` prints recent document messages instead.
pub(crate) async fn cmd_userbot_download(
    config: &Config,
    peer: &str,
    message: &str,
    outdir: Option<&str>,
    list: bool,
) -> Result<()> {
    let cfg = &config.channels.telegram.userbot;
    let (client, _session, updates, runner) = connect(cfg).await?;
    if !client.is_authorized().await? {
        anyhow::bail!("userbot is not authorized; run `opencrabs channel userbot-login`");
    }

    let result = run(&client, peer, message, outdir, list).await;

    drop(updates);
    drop(runner);
    result
}

async fn run(
    client: &Client,
    peer: &str,
    message: &str,
    outdir: Option<&str>,
    list: bool,
) -> Result<()> {
    let peer_ref = resolve_peer(client, peer).await?;

    if list {
        return list_documents(client, peer_ref, 30).await;
    }

    let dir = Path::new(outdir.unwrap_or("."));
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("creating output directory {}", dir.display()))?;

    let message = find_document(client, peer_ref, message).await?;
    let media = message
        .media()
        .ok_or_else(|| anyhow::anyhow!("message {} has no media", message.id()))?;
    let (uploader_name, size) = document_info(&message)
        .ok_or_else(|| anyhow::anyhow!("message {} media is not a document", message.id()))?;

    let name = safe_file_name(&uploader_name);
    if name != uploader_name {
        eprintln!("note: uploader name {uploader_name:?} reduced to {name:?}");
    }
    let path = dir.join(&name);
    // `safe_file_name` returns a single component, so this join cannot escape
    // `dir`. The check keeps that true if it is ever changed.
    if path.parent() != Some(dir) {
        anyhow::bail!("refusing to write outside {}", dir.display());
    }
    // Stream into a `.part` file and rename on success, so a failed download
    // leaves nothing that looks like a finished one.
    let part = path.with_file_name(format!("{name}.part"));

    eprintln!(
        "downloading msg {} -> {} ({} bytes)",
        message.id(),
        path.display(),
        size
    );

    let got = match stream_to_file(client, &media, &part, size).await {
        Ok(got) => got,
        Err(error) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(error);
        }
    };
    tokio::fs::rename(&part, &path)
        .await
        .with_context(|| format!("renaming {} to {}", part.display(), path.display()))?;
    println!("OK {} {got} bytes", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_file_name_keeps_a_plain_name() {
        assert_eq!(safe_file_name("report.pdf"), "report.pdf");
    }

    #[test]
    fn safe_file_name_reduces_paths_to_their_last_component() {
        assert_eq!(safe_file_name("a/b/report.pdf"), "report.pdf");
        assert_eq!(
            safe_file_name("../../.ssh/authorized_keys"),
            "authorized_keys"
        );
        assert_eq!(safe_file_name("/etc/passwd"), "passwd");
    }

    #[test]
    fn safe_file_name_replaces_names_with_nothing_usable() {
        assert_eq!(safe_file_name(""), "unnamed.bin");
        assert_eq!(safe_file_name(".."), "unnamed.bin");
        assert_eq!(safe_file_name("."), "unnamed.bin");
        assert_eq!(safe_file_name("a/.."), "unnamed.bin");
        assert_eq!(safe_file_name("   "), "unnamed.bin");
    }

    /// The traversal this exists for: whatever the uploader names the
    /// document, the joined path stays inside the output directory.
    #[test]
    fn joined_path_stays_inside_the_output_directory() {
        // A real output directory, portable across platforms: no POSIX
        // literal, so the Windows footgun scan stays clean.
        let dir = std::env::temp_dir().join("outdir");
        for uploader_name in [
            "report.pdf",
            "../../.ssh/authorized_keys",
            "/etc/passwd",
            "..",
            "",
        ] {
            let joined = dir.join(safe_file_name(uploader_name));
            assert_eq!(
                joined.parent(),
                Some(dir.as_path()),
                "uploader name {uploader_name:?} escaped the output directory"
            );
        }
    }
}
