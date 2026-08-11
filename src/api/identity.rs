//! DID and handle resolution.
//!
//! The DID document is the source of truth for where an account's
//! data lives. `fulmar login` resolves the PDS endpoint from it and
//! stores the result in the session file, which is what makes custom
//! PDSes work without any flag. `did:plc` resolves via the PLC
//! directory (plain HTTP, not XRPC); `did:web` via
//! `/.well-known/did.json` on the DID's host.

use reqwest::Client as HttpClient;
use serde_json::Value;

use super::ApiError;
use crate::identifiers::Did;

/// PLC directory base URL.
pub const PLC_DIRECTORY: &str = "https://plc.directory";

/// Extract the PDS endpoint (`#atproto_pds` service) from a DID
/// document, if present.
#[must_use]
pub fn pds_endpoint(did_doc: &Value) -> Option<String> {
    let services = did_doc.get("service")?.as_array()?;
    services.iter().find_map(|svc| {
        let id = svc.get("id")?.as_str()?;
        if !id.ends_with("#atproto_pds") {
            return None;
        }
        Some(svc.get("serviceEndpoint")?.as_str()?.to_string())
    })
}

/// Fetch a DID document from the appropriate directory. `plc_url` is
/// the PLC directory base (overridable for tests and self-hosted
/// mirrors); `did:web` documents always come from the DID's own host.
///
/// # Errors
///
/// [`ApiError::Http`] on network failure, [`ApiError::Api`] on a
/// non-2xx directory response, or [`ApiError::Unexpected`] for a DID
/// method other than `plc`/`web`.
pub async fn fetch_did_doc(http: &HttpClient, plc_url: &str, did: &Did) -> Result<Value, ApiError> {
    let url = did_doc_url(plc_url, did)?;
    let resp = http.get(&url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(ApiError::Api {
            status: status.as_u16(),
            kind: "DidResolutionFailed".to_string(),
            message: format!("{url} returned {status}"),
        });
    }
    Ok(resp.json().await?)
}

/// Resolve a DID's PDS endpoint via its DID document.
///
/// # Errors
///
/// As [`fetch_did_doc`], plus [`ApiError::Unexpected`] when the
/// document lists no `#atproto_pds` service.
pub async fn resolve_pds_via_directory(
    http: &HttpClient,
    plc_url: &str,
    did: &Did,
) -> Result<String, ApiError> {
    let doc = fetch_did_doc(http, plc_url, did).await?;
    pds_endpoint(&doc).ok_or_else(|| {
        ApiError::Unexpected(format!(
            "DID document for {did} lists no #atproto_pds service"
        ))
    })
}

fn did_doc_url(plc_url: &str, did: &Did) -> Result<String, ApiError> {
    let s = did.as_str();
    if s.starts_with("did:plc:") {
        let plc_url = plc_url.trim_end_matches('/');
        return Ok(format!("{plc_url}/{s}"));
    }
    if let Some(host) = s.strip_prefix("did:web:") {
        return Ok(format!("https://{host}/.well-known/did.json"));
    }
    Err(ApiError::Unexpected(format!(
        "unsupported DID method for resolution: {s}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identifiers::Did;

    #[test]
    fn pds_endpoint_finds_atproto_pds_service() {
        let doc = serde_json::json!({
            "service": [
                { "id": "#other", "type": "X", "serviceEndpoint": "https://nope.example" },
                {
                    "id": "#atproto_pds",
                    "type": "AtprotoPersonalDataServer",
                    "serviceEndpoint": "https://pip.host.bsky.network"
                }
            ]
        });
        assert_eq!(
            pds_endpoint(&doc).as_deref(),
            Some("https://pip.host.bsky.network")
        );
    }

    #[test]
    fn pds_endpoint_handles_fully_qualified_ids() {
        let doc = serde_json::json!({
            "service": [{
                "id": "did:plc:abc#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": "https://pds.example.com"
            }]
        });
        assert_eq!(
            pds_endpoint(&doc).as_deref(),
            Some("https://pds.example.com")
        );
    }

    #[test]
    fn pds_endpoint_none_when_absent() {
        let doc = serde_json::json!({ "service": [] });
        assert_eq!(pds_endpoint(&doc), None);
        let doc = serde_json::json!({});
        assert_eq!(pds_endpoint(&doc), None);
    }

    #[test]
    fn did_doc_url_routes_by_method() {
        let plc = Did::from_trusted("did:plc:abc123");
        assert_eq!(
            did_doc_url(PLC_DIRECTORY, &plc).expect("plc"),
            "https://plc.directory/did:plc:abc123"
        );
        let web = Did::from_trusted("did:web:pds.example.com");
        assert_eq!(
            did_doc_url(PLC_DIRECTORY, &web).expect("web"),
            "https://pds.example.com/.well-known/did.json"
        );
    }
}
