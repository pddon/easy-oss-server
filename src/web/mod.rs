//! Web frontend and admin UI.

pub mod admin_handler;

use rust_embed::RustEmbed;

/// Embedded static files for the admin web UI.
#[derive(RustEmbed)]
#[folder = "src/web/static/"]
pub struct StaticFiles;
