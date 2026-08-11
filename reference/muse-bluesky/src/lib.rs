//! AT Protocol client for Bluesky integration.
//!
//! Hand-rolled over reqwest + serde — the AT Protocol XRPC surface
//! is straightforward HTTP+JSON, and avoiding a heavy SDK dep keeps
//! compile times and surface area small. Switch to atrium/jacquard
//! later if we hit a feature that justifies it (`DPoP`, `OAuth`,
//! complex lex types).
//!
//! M1 scope: createSession + refreshSession, post a status update,
//! list notifications (mentions, replies, etc.), get timeline, get
//! a post thread. DM polling, follow/unfollow, repost/like, profile
//! lookups land in subsequent commits.

mod client;
mod config;
mod error;
mod identifiers;
mod traits;
mod types;

pub use client::BlueskyClient;
pub use config::BlueskyConfig;
pub use error::BlueskyError;
pub use identifiers::{AtUri, Cid, Did, Handle, IdentifierError, RKey};
pub use traits::Bluesky;
pub use types::{
    ComposeOptions, DmConvo, DmMember, DmMessage, ImageAttachment, Notification, PostRef,
    PostThread, PostView, ProfileRelationship, ProfileView, Session, TimelinePost, WhitewindEntry,
    rfc3339_strictly_after,
};
