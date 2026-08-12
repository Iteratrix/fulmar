//! Rich-text facet detection: links, mentions, tags, cashtags.
//!
//! A byte-faithful port of the official TypeScript detection
//! (`@atproto/api` `rich-text/detection.ts`, captured 2026-08 in
//! `docs/rust-atproto-crates.md` §4), including the newer cashtag
//! support that existing Rust ports lack. Detection is pure and
//! synchronous; mention handles are resolved to DIDs by the caller
//! (unresolvable mentions are dropped, matching the official client).
//!
//! All offsets are UTF-8 byte offsets, as the `app.bsky.richtext.facet`
//! lexicon requires. The TS reference computes UTF-16 indices and
//! converts; using the `regex` crate on `&str` yields UTF-8 offsets
//! natively, which removes that conversion bug class by construction
//! — the property tests still assert boundary correctness on
//! multibyte text.
//!
//! Two TS lookarounds are restructured because the `regex` crate has
//! none:
//! - the tag rule `(?!️)` (content must not start with an emoji
//!   variation selector) becomes a post-match check;
//! - the cashtag delimiter lookahead becomes a next-character
//!   inspection. Equivalent because every shorter backtracked match
//!   would end in `[A-Za-z0-9]`, which is never a valid delimiter.

use std::sync::LazyLock;

use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;

/// One detected facet, pre-resolution. `byte_start..byte_end` spans
/// the visible text of the facet (`@handle`, the URL text as typed,
/// `#tag`, `$SYM`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedFacet {
    pub byte_start: usize,
    pub byte_end: usize,
    pub feature: FacetFeature,
}

/// What a facet points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FacetFeature {
    /// `@handle` — needs handle→DID resolution before building the
    /// wire facet; drop it if resolution fails.
    Mention { handle: String },
    /// A link. `uri` carries the `https://` prefix even when the
    /// text was a bare domain.
    Link { uri: String },
    /// `#tag` or `$CASHTAG` (cashtags become tag facets whose value
    /// keeps the `$`, uppercased).
    Tag { tag: String },
}

static MENTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|[\s(])@([a-zA-Z0-9.-]+)\b").expect("mention regex"));

static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(^|[\s(])((https?://\S+)|((?<domain>[a-z][a-z0-9]*(?:\.[a-z0-9]+)+)\S*))")
        .expect("url regex")
});

static TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(^|\s)[#\x{FF03}](",
        r"[^\s\x{00AD}\x{2060}\x{200A}\x{200B}\x{200C}\x{200D}\x{20E2}]*",
        r"[^\d\s\p{P}\x{00AD}\x{2060}\x{200A}\x{200B}\x{200C}\x{200D}\x{20E2}]+",
        r"[^\s\x{00AD}\x{2060}\x{200A}\x{200B}\x{200C}\x{200D}\x{20E2}]*",
        r")?"
    ))
    .expect("tag regex")
});

static CASHTAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|[\s(])\$([A-Za-z][A-Za-z0-9]{0,4})").expect("cashtag regex"));

static TRAILING_PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\p{P}+$").expect("trailing punctuation regex"));

/// Detect all facets in `text`, sorted by byte position.
#[must_use]
pub fn detect_facets(text: &str) -> Vec<DetectedFacet> {
    let mut out = Vec::new();
    detect_mentions(text, &mut out);
    detect_links(text, &mut out);
    detect_tags(text, &mut out);
    detect_cashtags(text, &mut out);
    out.sort_by_key(|f| (f.byte_start, f.byte_end));
    out
}

/// Grapheme-cluster count, the unit Bluesky's 300-limit for post text
/// (and the 64-limit for tags) is measured in. `"👨‍👩‍👧‍👦"` is 1.
#[must_use]
pub fn grapheme_len(text: &str) -> usize {
    text.graphemes(true).count()
}

/// TS `isValidDomain`: the final label must be a known public suffix
/// (the reference uses the npm `tlds` list; the Public Suffix List is
/// equivalent for this purpose), or the domain ends in `.test`
/// (reserved for testing). Case-insensitive.
fn is_valid_domain(domain: &str) -> bool {
    let lower = domain.to_lowercase();
    if lower.rsplit_once('.').is_some_and(|(_, tld)| tld == "test") {
        return true;
    }
    let Some(suffix) = psl::suffix(lower.as_bytes()) else {
        return false;
    };
    suffix.is_known() && lower.len() > suffix.as_bytes().len()
}

fn detect_mentions(text: &str, out: &mut Vec<DetectedFacet>) {
    for caps in MENTION.captures_iter(text) {
        let Some(handle) = caps.get(2) else { continue };
        if !is_valid_domain(handle.as_str()) {
            continue;
        }
        out.push(DetectedFacet {
            byte_start: handle.start() - 1,
            byte_end: handle.end(),
            feature: FacetFeature::Mention {
                handle: handle.as_str().to_string(),
            },
        });
    }
}

fn detect_links(text: &str, out: &mut Vec<DetectedFacet>) {
    for caps in URL.captures_iter(text) {
        let Some(matched) = caps.get(2) else { continue };
        let mut uri = matched.as_str().to_string();
        let mut end = matched.end();

        if !uri.to_lowercase().starts_with("http") {
            let Some(domain) = caps.name("domain") else {
                continue;
            };
            if !is_valid_domain(domain.as_str()) {
                continue;
            }
            uri = format!("https://{uri}");
        }

        if uri.ends_with(['.', ',', ';', ':', '!', '?']) {
            uri.pop();
            end -= 1;
        }
        if uri.ends_with(')') && !uri.contains('(') {
            uri.pop();
            end -= 1;
        }

        out.push(DetectedFacet {
            byte_start: matched.start(),
            byte_end: end,
            feature: FacetFeature::Link { uri },
        });
    }
}

fn detect_tags(text: &str, out: &mut Vec<DetectedFacet>) {
    for caps in TAG.captures_iter(text) {
        let Some(content) = caps.get(2) else { continue };
        let raw = content.as_str();
        if raw.starts_with('\u{FE0F}') {
            continue;
        }
        let stripped = match TRAILING_PUNCTUATION.find(raw) {
            Some(punct) => &raw[..punct.start()],
            None => raw,
        };
        if stripped.is_empty() || grapheme_len(stripped) > 64 {
            continue;
        }
        let lead_end = caps.get(1).map_or(0, |m| m.end());
        out.push(DetectedFacet {
            byte_start: lead_end,
            byte_end: content.start() + stripped.len(),
            feature: FacetFeature::Tag {
                tag: stripped.to_string(),
            },
        });
    }
}

fn detect_cashtags(text: &str, out: &mut Vec<DetectedFacet>) {
    for caps in CASHTAG.captures_iter(text) {
        let Some(symbol) = caps.get(2) else { continue };
        let next = text[symbol.end()..].chars().next();
        let delimited = match next {
            None => true,
            Some(c) => {
                c.is_whitespace()
                    || matches!(
                        c,
                        '.' | ',' | ';' | ':' | '!' | '?' | ')' | '"' | '\'' | '\u{2019}'
                    )
            }
        };
        if !delimited {
            continue;
        }
        out.push(DetectedFacet {
            byte_start: symbol.start() - 1,
            byte_end: symbol.end(),
            feature: FacetFeature::Tag {
                tag: format!("${}", symbol.as_str().to_uppercase()),
            },
        });
    }
}

/// Build the wire-format facet array (`app.bsky.richtext.facet`) from
/// detected facets whose mentions have already been resolved.
/// `resolve` maps a handle to `Some(did)` or `None` (facet dropped —
/// the official client behaves the same for unresolvable mentions).
pub fn to_wire<F>(facets: &[DetectedFacet], mut resolve: F) -> Vec<serde_json::Value>
where
    F: FnMut(&str) -> Option<String>,
{
    facets
        .iter()
        .filter_map(|f| {
            let feature = match &f.feature {
                FacetFeature::Mention { handle } => {
                    let did = resolve(handle)?;
                    serde_json::json!({ "$type": "app.bsky.richtext.facet#mention", "did": did })
                }
                FacetFeature::Link { uri } => {
                    serde_json::json!({ "$type": "app.bsky.richtext.facet#link", "uri": uri })
                }
                FacetFeature::Tag { tag } => {
                    serde_json::json!({ "$type": "app.bsky.richtext.facet#tag", "tag": tag })
                }
            };
            Some(serde_json::json!({
                "index": { "byteStart": f.byte_start, "byteEnd": f.byte_end },
                "features": [feature],
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(text: &str) -> Vec<(String, usize, usize)> {
        detect_facets(text)
            .into_iter()
            .filter_map(|f| match f.feature {
                FacetFeature::Link { uri } => Some((uri, f.byte_start, f.byte_end)),
                FacetFeature::Mention { .. } | FacetFeature::Tag { .. } => None,
            })
            .collect()
    }

    fn tags(text: &str) -> Vec<String> {
        detect_facets(text)
            .into_iter()
            .filter_map(|f| match f.feature {
                FacetFeature::Tag { tag } => Some(tag),
                FacetFeature::Mention { .. } | FacetFeature::Link { .. } => None,
            })
            .collect()
    }

    fn mentions(text: &str) -> Vec<(String, usize, usize)> {
        detect_facets(text)
            .into_iter()
            .filter_map(|f| match f.feature {
                FacetFeature::Mention { handle } => Some((handle, f.byte_start, f.byte_end)),
                FacetFeature::Link { .. } | FacetFeature::Tag { .. } => None,
            })
            .collect()
    }

    #[test]
    fn plain_text_has_no_facets() {
        assert!(detect_facets("just some words here").is_empty());
    }

    #[test]
    fn mention_at_start_and_after_space() {
        assert_eq!(
            mentions("@alice.bsky.social hi"),
            vec![("alice.bsky.social".to_string(), 0, 18)]
        );
        assert_eq!(
            mentions("hi @alice.bsky.social"),
            vec![("alice.bsky.social".to_string(), 3, 21)]
        );
    }

    #[test]
    fn mention_with_invalid_tld_is_skipped() {
        assert!(mentions("@alice.notarealtldxyz hi").is_empty());
    }

    #[test]
    fn mention_dot_test_is_allowed() {
        assert_eq!(mentions("@alice.test hi").len(), 1);
    }

    #[test]
    fn mention_mid_word_is_not_a_mention() {
        assert!(mentions("email@example.com").is_empty());
    }

    #[test]
    fn mention_after_multibyte_prefix_has_correct_offsets() {
        let text = "✨ @alice.bsky.social";
        let got = mentions(text);
        assert_eq!(got.len(), 1);
        let (_, start, end) = got[0];
        assert_eq!(&text[start..end], "@alice.bsky.social");
    }

    #[test]
    fn scheme_url_detected_without_validation() {
        assert_eq!(
            links("see https://example.com/path?q=1"),
            vec![("https://example.com/path?q=1".to_string(), 4, 32)]
        );
    }

    #[test]
    fn bare_domain_gets_scheme_prepended() {
        let got = links("go to example.com now");
        assert_eq!(got, vec![("https://example.com".to_string(), 6, 17)]);
    }

    #[test]
    fn bare_domain_with_bad_tld_is_skipped() {
        assert!(links("see example.notarealtldxyz now").is_empty());
    }

    #[test]
    fn link_strips_one_trailing_punctuation() {
        let got = links("read https://example.com/foo.");
        assert_eq!(got[0].0, "https://example.com/foo");
        let text = "read https://example.com/foo.";
        assert_eq!(&text[got[0].1..got[0].2], "https://example.com/foo");
    }

    #[test]
    fn link_keeps_paren_when_uri_contains_open_paren() {
        let got = links("see https://en.wikipedia.org/wiki/Foo_(bar) ok");
        assert_eq!(got[0].0, "https://en.wikipedia.org/wiki/Foo_(bar)");
    }

    #[test]
    fn link_strips_paren_when_uri_has_no_open_paren() {
        let got = links("(https://example.com)");
        assert_eq!(got[0].0, "https://example.com");
    }

    #[test]
    fn link_strips_punct_then_paren() {
        let got = links("see example.com/foo).");
        assert_eq!(got[0].0, "https://example.com/foo");
    }

    #[test]
    fn tag_basic_and_offsets() {
        let text = "hello #world";
        let facets = detect_facets(text);
        assert_eq!(facets.len(), 1);
        assert_eq!(&text[facets[0].byte_start..facets[0].byte_end], "#world");
        assert_eq!(tags(text), vec!["world"]);
    }

    #[test]
    fn tag_strips_trailing_punctuation() {
        assert_eq!(tags("wow #cool!!!"), vec!["cool"]);
    }

    #[test]
    fn tag_all_digits_is_skipped() {
        assert!(tags("#1234 nope").is_empty());
    }

    #[test]
    fn tag_with_digits_and_letters_is_kept() {
        assert_eq!(tags("#web3 yes"), vec!["web3"]);
    }

    #[test]
    fn tag_fullwidth_hash_works() {
        assert_eq!(tags("＃日本語"), vec!["日本語"]);
    }

    #[test]
    fn tag_variation_selector_start_is_skipped() {
        assert!(tags("#\u{FE0F}x").is_empty());
    }

    #[test]
    fn tag_over_64_graphemes_is_skipped() {
        let long = "x".repeat(65);
        assert!(tags(&format!("#{long}")).is_empty());
        let ok = "x".repeat(64);
        assert_eq!(tags(&format!("#{ok}")).len(), 1);
    }

    #[test]
    fn tag_grapheme_limit_counts_graphemes_not_bytes() {
        let tag: String = "👨‍👩‍👧‍👦".repeat(64);
        assert_eq!(
            tags(&format!("#{tag}")).len(),
            1,
            "64 family emoji = 64 graphemes"
        );
    }

    #[test]
    fn tag_mid_word_hash_is_not_a_tag() {
        assert!(tags("c#never").is_empty());
    }

    #[test]
    fn cashtag_basic() {
        let text = "buy $TSLA now";
        let facets = detect_facets(text);
        assert_eq!(tags(text), vec!["$TSLA"]);
        assert_eq!(&text[facets[0].byte_start..facets[0].byte_end], "$TSLA");
    }

    #[test]
    fn cashtag_lowercase_is_uppercased() {
        assert_eq!(tags("watch $gme."), vec!["$GME"]);
    }

    #[test]
    fn cashtag_too_long_is_skipped() {
        assert!(tags("not $TOOLONG okay").is_empty());
    }

    #[test]
    fn cashtag_needs_delimiter() {
        assert!(tags("$TSLA5X").is_empty());
        assert_eq!(tags("$TSLA"), vec!["$TSLA"]);
        assert_eq!(tags("($TSLA)"), vec!["$TSLA"]);
    }

    #[test]
    fn cashtag_must_start_with_letter() {
        assert!(tags("$5X").is_empty());
    }

    #[test]
    fn mixed_text_all_kinds() {
        let text = "@alice.bsky.social check example.com #news $ABC";
        let facets = detect_facets(text);
        assert_eq!(facets.len(), 4);
        assert!(
            facets
                .windows(2)
                .all(|w| w[0].byte_start <= w[1].byte_start)
        );
    }

    #[test]
    fn to_wire_drops_unresolvable_mentions() {
        let facets = detect_facets("@alice.bsky.social and @bob.bsky.social");
        let wire = to_wire(&facets, |handle| {
            if handle.starts_with("alice") {
                Some("did:plc:alice".to_string())
            } else {
                None
            }
        });
        assert_eq!(wire.len(), 1);
        assert_eq!(
            wire[0]["features"][0]["did"],
            serde_json::json!("did:plc:alice")
        );
    }

    #[test]
    fn grapheme_len_counts_clusters() {
        assert_eq!(grapheme_len("abc"), 3);
        assert_eq!(grapheme_len("👨‍👩‍👧‍👦"), 1);
        assert_eq!(grapheme_len("é"), 1);
    }
}

#[cfg(test)]
mod properties {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Every facet's byte range must land on char boundaries and
        /// slice cleanly out of the input — the classic multibyte
        /// off-by-N bug this module exists to prevent.
        #[test]
        fn offsets_are_char_boundaries(text in "\\PC{0,200}") {
            for f in detect_facets(&text) {
                prop_assert!(f.byte_start < f.byte_end);
                prop_assert!(f.byte_end <= text.len());
                prop_assert!(text.is_char_boundary(f.byte_start));
                prop_assert!(text.is_char_boundary(f.byte_end));
            }
        }

        /// A known mention embedded after arbitrary unicode + a space
        /// must be found, and its byte range must slice back to
        /// exactly the mention text.
        #[test]
        fn mention_survives_arbitrary_prefix(prefix in "\\PC{0,80}") {
            let text = format!("{prefix} @alice.bsky.social");
            let facets = detect_facets(&text);
            let mention: Vec<_> = facets
                .iter()
                .filter(|f| match &f.feature {
                    FacetFeature::Mention { handle } => handle == "alice.bsky.social",
                    FacetFeature::Link { .. } | FacetFeature::Tag { .. } => false,
                })
                .collect();
            prop_assert_eq!(mention.len(), 1, "text: {:?}", text);
            let f = mention[0];
            prop_assert_eq!(&text[f.byte_start..f.byte_end], "@alice.bsky.social");
        }

        /// Same for a scheme-ful link: the byte range must cover the
        /// URL as typed regardless of what unicode precedes it.
        #[test]
        fn link_survives_arbitrary_prefix(prefix in "\\PC{0,80}") {
            let text = format!("{prefix} https://example.com/x");
            let facets = detect_facets(&text);
            let link: Vec<_> = facets
                .iter()
                .filter(|f| match &f.feature {
                    FacetFeature::Link { uri } => uri == "https://example.com/x",
                    FacetFeature::Mention { .. } | FacetFeature::Tag { .. } => false,
                })
                .collect();
            prop_assert_eq!(link.len(), 1, "text: {:?}", text);
            let f = link[0];
            prop_assert_eq!(&text[f.byte_start..f.byte_end], "https://example.com/x");
        }

        /// Detection is deterministic.
        #[test]
        fn detection_is_deterministic(text in "\\PC{0,200}") {
            prop_assert_eq!(detect_facets(&text), detect_facets(&text));
        }

        /// Tag facets always slice to `#`/`＃` + the reported tag
        /// (modulo stripped trailing punctuation).
        #[test]
        fn tag_slices_match_reported_tag(text in "\\PC{0,200}") {
            for f in detect_facets(&text) {
                let FacetFeature::Tag { tag } = &f.feature else { continue };
                if tag.starts_with('$') { continue; }
                let slice = &text[f.byte_start..f.byte_end];
                let content = slice
                    .strip_prefix('#')
                    .or_else(|| slice.strip_prefix('\u{FF03}'));
                prop_assert_eq!(content, Some(tag.as_str()), "slice: {:?}", slice);
            }
        }
    }
}
