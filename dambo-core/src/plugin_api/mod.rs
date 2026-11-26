pub mod auth;
mod handlers;
#[cfg(test)]
mod tests;

pub use auth::{
    ADMIN_SESSION_TTL, AUTH_FAILURE_WINDOW, AUTH_MAX_FAILURES, AdminSessionStore, ApiError,
    MAX_ADMIN_SESSIONS, ManageSession, OptionalPeerIp, authorize_and_maybe_mint_manage_cookie,
    build_admin_set_cookie, build_admin_set_cookie_for_mode, configured_admin_token,
    extract_admin_cookie, is_peer_loopback, is_peer_loopback_or_private, validate_host_header,
};
pub use handlers::{API_MAX_BODY_BYTES, MAX_MSET_ENTRIES, api_router};
