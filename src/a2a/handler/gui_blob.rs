//! `gui/blob`: the bytes behind a [`BlobRef`], raw over HTTP.
//!
//! `GET  ?path=<crab path>`  needs `a2a:read`, answers the file's bytes.
//! `POST ?name=<file name>`  needs `a2a:send`, takes the body as an
//! attachment and answers its [`BlobRef`] as JSON.
//!
//! Both sit behind `require_bearer`, which attaches [`GrantedScopes`]. Every
//! rule about which path may be served lives in `backend::blob`; this file
//! only checks the scope, picks the primary crab and maps a [`BlobError`] to
//! its status. A refusal carries the status text only, never the path or the
//! reason, so a caller probing the disk learns nothing beyond 403.
//!
//! [`BlobRef`]: crate::backend::BlobRef

use axum::{
    Extension, Json,
    body::Bytes,
    extract::Query,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use crate::a2a::server::GrantedScopes;
use crate::backend::blob::{self, BlobError};
use crate::remote::scopes::{SCOPE_READ, SCOPE_SEND, allows};

/// `GET` query: the path a `BlobRef` carried.
#[derive(Debug, Deserialize)]
pub struct BlobQuery {
    pub path: String,
}

/// `POST` query: the name the sender gave the attachment.
#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    pub name: String,
}

/// A bare status, its canonical reason as the body.
fn status_only(status: StatusCode) -> Response {
    (status, status.canonical_reason().unwrap_or_default()).into_response()
}

/// The status a blob failure answers with.
fn refusal(e: &BlobError) -> Response {
    status_only(StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
}

/// `GET /a2a/blob`: serve the file at `path` when the allowlist lets it out.
pub async fn handle_get(
    Extension(granted): Extension<GrantedScopes>,
    Query(query): Query<BlobQuery>,
) -> Response {
    if !allows(&granted.0, SCOPE_READ) {
        return status_only(StatusCode::FORBIDDEN);
    }
    let Some(ctx) = crate::backend::primary() else {
        return status_only(StatusCode::SERVICE_UNAVAILABLE);
    };
    let roots = blob::roots(&blob::home_of(&ctx));
    match tokio::task::spawn_blocking(move || blob::read(&roots, &query.path)).await {
        Ok(Ok(bytes)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        )
            .into_response(),
        Ok(Err(e)) => refusal(&e),
        Err(_) => status_only(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// `POST /a2a/blob`: store the body as an attachment and name it.
pub async fn handle_put(
    Extension(granted): Extension<GrantedScopes>,
    Query(query): Query<UploadQuery>,
    body: Bytes,
) -> Response {
    if !allows(&granted.0, SCOPE_SEND) {
        return status_only(StatusCode::FORBIDDEN);
    }
    let Some(ctx) = crate::backend::primary() else {
        return status_only(StatusCode::SERVICE_UNAVAILABLE);
    };
    let home = blob::home_of(&ctx);
    match tokio::task::spawn_blocking(move || blob::store_upload(&home, &query.name, &body)).await
    {
        Ok(Ok(blob_ref)) => (StatusCode::OK, Json(blob_ref)).into_response(),
        Ok(Err(e)) => refusal(&e),
        Err(_) => status_only(StatusCode::INTERNAL_SERVER_ERROR),
    }
}
