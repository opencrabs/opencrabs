//! Userbot document download: an explicit operator action, not part of the
//! passive watch loop.
//!
//! The watch loop stays receive-only by design; this module exists so an
//! operator at the CLI can pull a specific document through the same MTProto
//! session (the Bot API caps downloads at 20 MB; MTProto streams up to 2 GB).
//! Ported from the tgdl spike that recovered a 105 MB channel document on
//! 2026-10-12; wiring matches login.rs (grammers 0.10, verified against
//! vendored source).

use anyhow::{Context, Result};
use grammers_client::Client;
use grammers_client::media::Media;
use grammers_client::message::Message;
use grammers_session::types::PeerRef;
use tokio::io::AsyncWriteExt;

use super::login::connect;
use crate::config::types::Config;

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
    let mut iter = client.iter_messages(peer);
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
        let mut iter = client.iter_messages(peer);
        while let Some(m) = iter.next().await? {
            if document_info(&m).is_some() {
                return Ok(m);
            }
        }
        anyhow::bail!("no document found in recent history");
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

    let dir = outdir.unwrap_or(".");
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("creating output directory {dir}"))?;

    let message = find_document(client, peer_ref, message).await?;
    let media = message
        .media()
        .ok_or_else(|| anyhow::anyhow!("message {} has no media", message.id()))?;
    let (name, size) = document_info(&message)
        .ok_or_else(|| anyhow::anyhow!("message {} media is not a document", message.id()))?;
    let path = format!("{dir}/{name}");
    eprintln!(
        "downloading msg {} -> {} ({} bytes)",
        message.id(),
        path,
        size
    );

    let mut file = tokio::fs::File::create(&path)
        .await
        .with_context(|| format!("creating {path}"))?;
    let mut stream = client.iter_download(&media);
    let mut got: u64 = 0;
    while let Some(chunk) = stream.next().await? {
        file.write_all(&chunk).await?;
        got += chunk.len() as u64;
        // Progress every 8 MB, but only for files where the wait is felt.
        if size >= 8 * 1024 * 1024 && got % (8 * 1024 * 1024) < 262_144 {
            eprintln!("  {got}/{size} bytes");
        }
    }
    file.flush().await?;
    println!("OK {path} {got} bytes");
    Ok(())
}
