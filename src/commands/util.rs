//! Shared command plumbing: pagination, stdin/file input, page
//! splitting.

use std::path::PathBuf;

use anyhow::Context as _;
use serde_json::Value;

use crate::api::ApiError;
use crate::cli::PageArgs;
use crate::output::Output;

/// Split a list response into `(items, cursor)` given the array key
/// (`feed`, `notifications`, `convos`, ...).
#[must_use]
pub fn split_page(value: &Value, key: &str) -> (Vec<Value>, Option<String>) {
    let items = value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let cursor = value
        .get("cursor")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    (items, cursor)
}

/// Drive a cursor-paginated fetch per the output conventions: one
/// page (emitting the trailing cursor) by default, exhaustive under
/// `--all`.
///
/// # Errors
///
/// Propagates fetch errors.
pub async fn paginate<F, Fut>(
    out: &Output,
    page: &PageArgs,
    render: fn(&Value) -> String,
    mut fetch: F,
) -> anyhow::Result<()>
where
    F: FnMut(Option<String>, Option<u32>) -> Fut,
    Fut: Future<Output = Result<(Vec<Value>, Option<String>), ApiError>>,
{
    let mut cursor = page.cursor.clone();
    loop {
        let (items, next) = fetch(cursor.clone(), page.limit).await?;
        for item in &items {
            out.item(item, render);
        }
        if !page.all {
            out.cursor(next.as_deref());
            return Ok(());
        }
        if next.is_none() || items.is_empty() {
            return Ok(());
        }
        cursor = next;
    }
}

/// Build the standard `(limit, cursor)` query-param vector; callers
/// append endpoint-specific params.
#[must_use]
pub fn page_params(cursor: Option<String>, limit: Option<u32>) -> Vec<(&'static str, String)> {
    let mut params = Vec::new();
    if let Some(limit) = limit {
        params.push(("limit", limit.to_string()));
    }
    if let Some(cursor) = cursor {
        params.push(("cursor", cursor));
    }
    params
}

/// Resolve a text argument: `-` means read stdin.
///
/// # Errors
///
/// I/O errors reading stdin.
pub fn text_or_stdin(text: &str) -> anyhow::Result<String> {
    if text != "-" {
        return Ok(text.to_string());
    }
    let text = std::io::read_to_string(std::io::stdin()).context("reading text from stdin")?;
    Ok(text.trim_end_matches('\n').to_string())
}

/// Read a JSON document from a file, or stdin when `file` is `None`.
///
/// # Errors
///
/// I/O and JSON parse errors, with the source named.
pub fn json_input(file: Option<&PathBuf>) -> anyhow::Result<Value> {
    let (raw, source) = match file {
        Some(path) => (
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
            path.display().to_string(),
        ),
        None => (
            std::io::read_to_string(std::io::stdin()).context("reading JSON from stdin")?,
            "stdin".to_string(),
        ),
    };
    serde_json::from_str(&raw).with_context(|| format!("parsing JSON from {source}"))
}

/// Read text content from a file, or stdin when `file` is `None`.
///
/// # Errors
///
/// I/O errors, with the source named.
pub fn text_input(file: Option<&PathBuf>) -> anyhow::Result<String> {
    match file {
        Some(path) => {
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
        }
        None => std::io::read_to_string(std::io::stdin()).context("reading from stdin"),
    }
}
