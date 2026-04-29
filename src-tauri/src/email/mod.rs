//! Email delivery + disposition handling. See SPEC.md §8.
//!
//! - `link_signer`: HMAC-signed disposition tokens (60-day soft expiry,
//!   stable per-install secret — pinned plan decision #1).
//! - `template`: HTML + plaintext rendering via Tera.
//! - `gmail`: send via Google's Gmail v1 API, scope `gmail.send` only.
//! - `oauth`: PKCE + loopback redirect flow, refresh token in keychain.
//! - `disposition_server`: localhost axum server for action links.

pub mod disposition_server;
pub mod gmail;
pub mod link_signer;
pub mod oauth;
pub mod oauth_flow;
pub mod template;

pub use disposition_server::{
    start as start_disposition_server, DispositionServerConfig, RunningServer,
};
pub use gmail::{EmailSender, GmailSender, MockEmailSender};
pub use link_signer::{DispositionAction, LinkSigner, SignedToken};
pub use oauth::{
    begin_auth, complete_auth, has_stored_refresh_token, refresh_access_token,
    revoke_stored_refresh_token, AccessToken, AuthInit, OAuthConfig,
};
pub use template::{render as render_email, EmailRenderInput, RenderedEmail};
