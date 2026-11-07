//! Admin web UI handlers.

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use mime_guess::from_path;

use crate::web::StaticFiles;

/// Serve the admin SPA (single-page application).
///
/// All paths under /admin/ return index.html for client-side routing.
/// The outer route without a wildcard passes `None`.
pub async fn serve_admin(path: Option<axum::extract::Path<String>>) -> Response {
    let path = path.map(|p| p.0).unwrap_or_default();
    let path = path.as_str();
    let file_path = if path.is_empty() || path == "/" {
        "index.html".to_string()
    } else {
        // Strip leading slash
        let p = path.trim_start_matches('/');
        // If the path doesn't have an extension, it's a client route -> index.html
        if !p.contains('.') {
            "index.html".to_string()
        } else {
            p.to_string()
        }
    };

    serve_static_file(&file_path)
}

/// Serve a static file from the embedded assets.
fn serve_static_file(path: &str) -> Response {
    match StaticFiles::get(path) {
        Some(content) => {
            let mime = from_path(path).first_or_octet_stream();
            let body = Body::from(content.data.into_owned());
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime.to_string()),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                ],
                body,
            )
                .into_response()
        }
        None => {
            // Fallback to index.html for SPA routing
            match StaticFiles::get("index.html") {
                Some(content) => {
                    let body = Body::from(content.data.into_owned());
                    (
                        StatusCode::OK,
                        [
                            (header::CONTENT_TYPE, "text/html".to_string()),
                            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                        ],
                        body,
                    )
                        .into_response()
                }
                None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
            }
        }
    }
}

/// Redirect `/admin` to `/admin/` so relative asset URLs resolve correctly.
pub async fn redirect_to_admin() -> Response {
    (
        StatusCode::PERMANENT_REDIRECT,
        [(header::LOCATION, "/admin/")],
        Body::empty(),
    )
        .into_response()
}

/// Root redirect to admin UI.
pub async fn root_redirect() -> Response {
    (
        StatusCode::FOUND,
        [(header::LOCATION, "/admin/")],
        Body::empty(),
    )
        .into_response()
}
