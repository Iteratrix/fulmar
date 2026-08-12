//! fulmar — a complete Bluesky / AT Protocol CLI.
//!
//! Library layer backing the `fulmar` binary. The design brief lives
//! in `docs/cli-design.md`; the API routing map in
//! `docs/api-inventory.md`.

pub mod api;
pub mod cli;
pub mod commands;
pub mod facets;
pub mod identifiers;
pub mod output;
pub mod session;

pub use identifiers::{AtUri, Cid, Did, Handle, IdentifierError, RKey};
pub use session::{SessionError, SessionFile, SessionStore};
