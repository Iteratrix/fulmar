//! Best-effort `OpenGraph` metadata extraction for link cards
//! (`app.bsky.embed.external`).
//!
//! Deliberately not a full HTML parser: OG tags are flat `<meta>`
//! elements with a stable shape, and a tolerant regex pass over the
//! document head covers real-world pages without dragging in an HTML5
//! parsing stack. Failure degrades gracefully — a card with the URL
//! as its title still posts.

use std::sync::LazyLock;

use regex::Regex;

/// Extracted card fields. `title` falls back to the URL itself when
/// the page offers nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageCard {
    pub title: String,
    pub description: String,
    /// Absolute image URL, when the page declares `og:image`.
    pub image: Option<String>,
}

static TITLE_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").expect("title regex"));

/// Extract card metadata from HTML. `base_url` resolves relative
/// `og:image` references.
#[must_use]
pub fn extract(html: &str, base_url: &str) -> PageCard {
    let title = meta_content(html, "og:title")
        .or_else(|| meta_content(html, "twitter:title"))
        .or_else(|| TITLE_TAG.captures(html).map(|c| c[1].trim().to_string()))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| base_url.to_string());
    let description = meta_content(html, "og:description")
        .or_else(|| meta_content(html, "description"))
        .or_else(|| meta_content(html, "twitter:description"))
        .unwrap_or_default();
    let image = meta_content(html, "og:image")
        .or_else(|| meta_content(html, "twitter:image"))
        .and_then(|src| absolutize(&src, base_url));
    PageCard {
        title: decode_entities(&title),
        description: decode_entities(&description),
        image,
    }
}

/// Find `<meta property|name="KEY" content="...">` in either
/// attribute order.
fn meta_content(html: &str, key: &str) -> Option<String> {
    let key = regex::escape(key);
    let property_first = format!(
        r#"(?is)<meta\b[^>]*?(?:property|name)\s*=\s*["']{key}["'][^>]*?content\s*=\s*["']([^"']*)["']"#
    );
    let content_first = format!(
        r#"(?is)<meta\b[^>]*?content\s*=\s*["']([^"']*)["'][^>]*?(?:property|name)\s*=\s*["']{key}["']"#
    );
    for pattern in [property_first, content_first] {
        let Ok(re) = Regex::new(&pattern) else {
            continue;
        };
        if let Some(caps) = re.captures(html) {
            let value = caps[1].trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// Resolve an `og:image` reference to an absolute URL against the
/// page URL. Returns `None` for unresolvable references.
fn absolutize(src: &str, base_url: &str) -> Option<String> {
    if src.starts_with("http://") || src.starts_with("https://") {
        return Some(src.to_string());
    }
    let base = reqwest::Url::parse(base_url).ok()?;
    Some(base.join(src).ok()?.to_string())
}

/// The handful of entities that actually appear in OG content.
fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<html><head>
        <title>Fallback Title</title>
        <meta property="og:title" content="OG Title &amp; More" />
        <meta property="og:description" content="A description." />
        <meta property="og:image" content="https://cdn.example.com/img.png" />
    </head><body></body></html>"#;

    #[test]
    fn extracts_og_fields() {
        let card = extract(PAGE, "https://example.com/article");
        assert_eq!(card.title, "OG Title & More");
        assert_eq!(card.description, "A description.");
        assert_eq!(
            card.image.as_deref(),
            Some("https://cdn.example.com/img.png")
        );
    }

    #[test]
    fn falls_back_to_title_tag_then_url() {
        let html = "<html><head><title>Just A Title</title></head></html>";
        let card = extract(html, "https://example.com/x");
        assert_eq!(card.title, "Just A Title");
        assert_eq!(card.description, "");
        assert_eq!(card.image, None);

        let card = extract("<html></html>", "https://example.com/x");
        assert_eq!(card.title, "https://example.com/x");
    }

    #[test]
    fn handles_reversed_attribute_order() {
        let html = r#"<meta content="Reversed" property="og:title">"#;
        let card = extract(html, "https://example.com");
        assert_eq!(card.title, "Reversed");
    }

    #[test]
    fn handles_name_attribute_and_single_quotes() {
        let html = "<meta name='description' content='named desc'>";
        let card = extract(html, "https://example.com");
        assert_eq!(card.description, "named desc");
    }

    #[test]
    fn resolves_relative_image_urls() {
        let html = r#"<meta property="og:image" content="/img/cover.jpg">"#;
        let card = extract(html, "https://example.com/deep/page");
        assert_eq!(
            card.image.as_deref(),
            Some("https://example.com/img/cover.jpg")
        );

        let html = r#"<meta property="og:image" content="//cdn.example.com/c.png">"#;
        let card = extract(html, "https://example.com/page");
        assert_eq!(card.image.as_deref(), Some("https://cdn.example.com/c.png"));
    }

    #[test]
    fn case_insensitive_tags() {
        let html = r#"<META PROPERTY="og:title" CONTENT="Shouty">"#;
        let card = extract(html, "https://example.com");
        assert_eq!(card.title, "Shouty");
    }
}
