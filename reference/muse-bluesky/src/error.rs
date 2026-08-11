//! Errors surfaced by the Bluesky client.

#[derive(thiserror::Error, Debug)]
pub enum BlueskyError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("decoding response: {0}")]
    Decode(#[from] serde_json::Error),
    /// Server returned a non-2xx status with a structured error body.
    /// `kind` mirrors AT Protocol's `error` field (e.g.
    /// `InvalidToken`); `message` is the operator-readable detail.
    #[error("api {status} {kind}: {message}")]
    Api {
        status: u16,
        kind: String,
        message: String,
    },
    /// Session refresh failed — usually means both the access and
    /// refresh JWTs are expired or revoked. Caller has to re-auth
    /// from credentials.
    #[error("session expired and refresh failed")]
    SessionExpired,
    /// Tried to use a method that requires login before calling
    /// `BlueskyClient::login`.
    #[error("not authenticated")]
    NotAuthenticated,
    /// Server returned 2xx but the response body was missing a
    /// field we expected (e.g. `convo` on a `getConvoForMembers`
    /// response). Means either the lex changed or the request
    /// matched something we don't know how to parse.
    #[error("unexpected response shape: {0}")]
    Unexpected(String),
    /// Bluesky returned a value in a field that didn't pass identifier
    /// validation (e.g. a handle that doesn't contain a dot, or a CID
    /// that doesn't start with `bafy`). Indicates the AT Protocol lex
    /// changed or we hit a non-standard PDS.
    #[error("malformed identifier from bluesky: {0}")]
    Identifier(#[from] crate::identifiers::IdentifierError),
}
