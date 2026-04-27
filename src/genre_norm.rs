//! Genre tag normalization for the recommender.
//!
//! Real-world tag bags are filthy: `"Rock"`, `"rock"`, `"Rock; Indie"`,
//! `"(17)Rock"` (the legacy ID3v1 numeric prefix lofty preserves verbatim),
//! `"Rock / Pop"`. Computing genre overlap as Jaccard set distance only
//! works once these all collapse onto the same canonical token.
//!
//! The rules (deliberately tiny — every transform pays its weight):
//! - Strip a leading `"(<digits>)"` prefix.
//! - Split on `;` and `/` (the two most common multi-genre joiners).
//! - Trim each fragment, lowercase it, drop empties.
//! - Dedupe.
//!
//! Returned as a `Vec<String>` rather than a `HashSet` so callers can iterate
//! deterministically when they need a stable rendering — Jaccard math at the
//! call site collects into a set anyway.

/// Normalize a single genre tag string into one or more canonical tokens.
/// `None` and tags that yield no tokens both produce an empty `Vec` so
/// callers don't need to special-case the absent-tag path.
pub fn normalize(tag: Option<&str>) -> Vec<String> {
    let raw = match tag {
        Some(s) => s.trim(),
        None => return Vec::new(),
    };
    if raw.is_empty() {
        return Vec::new();
    }

    // Strip leading "(NN)" — the ID3v1 numeric-genre prefix that ID3v2
    // taggers sometimes leave intact when migrating files. The closing
    // paren must follow at least one digit; anything else is left alone
    // (artists like "(Sandy) Alex G" should not be eaten).
    let without_prefix = strip_legacy_prefix(raw);

    let mut out: Vec<String> = Vec::new();
    for part in without_prefix.split(['/', ';']) {
        let cleaned = part.trim().to_lowercase();
        if cleaned.is_empty() {
            continue;
        }
        if !out.iter().any(|existing| existing == &cleaned) {
            out.push(cleaned);
        }
    }
    out
}

fn strip_legacy_prefix(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'(') {
        return s;
    }
    // Walk past digits.
    let mut i = 1;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    // Must have consumed at least one digit, then a closing paren.
    if i > 1 && bytes.get(i) == Some(&b')') {
        // Skip the ')' and any whitespace immediately after.
        let rest = &s[i + 1..];
        rest.trim_start()
    } else {
        s
    }
}

/// Jaccard similarity between two normalized genre token bags. Both inputs
/// are expected to be the output of `normalize` (or unioned outputs across
/// multiple tracks). Returns `0.0` when both bags are empty rather than
/// `NaN`, so the caller's weighted sum stays finite.
pub fn jaccard(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let mut intersection = 0usize;
    for x in a {
        if b.iter().any(|y| y == x) {
            intersection += 1;
        }
    }
    let union = a.len() + b.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f32 / union as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_none_or_empty_yields_empty() {
        assert!(normalize(None).is_empty());
        assert!(normalize(Some("")).is_empty());
        assert!(normalize(Some("   ")).is_empty());
    }

    #[test]
    fn normalize_single_tag_lowercases_and_trims() {
        assert_eq!(normalize(Some("  Rock ")), vec!["rock"]);
    }

    #[test]
    fn normalize_strips_legacy_id3v1_prefix() {
        assert_eq!(normalize(Some("(17)Rock")), vec!["rock"]);
        assert_eq!(normalize(Some("(0) Blues")), vec!["blues"]);
    }

    #[test]
    fn normalize_does_not_eat_paren_in_artist_like_text() {
        // No digits inside the parens → leave it alone (collapses to one
        // lowercased token, parens preserved).
        assert_eq!(normalize(Some("(Sandy) Alex G")), vec!["(sandy) alex g"]);
    }

    #[test]
    fn normalize_splits_on_semicolon_and_slash() {
        assert_eq!(
            normalize(Some("Rock; Indie / Lo-Fi")),
            vec!["rock", "indie", "lo-fi"]
        );
    }

    #[test]
    fn normalize_dedupes_repeated_tokens() {
        assert_eq!(normalize(Some("Rock; rock; ROCK")), vec!["rock"]);
    }

    #[test]
    fn normalize_skips_empty_fragments() {
        assert_eq!(normalize(Some("Rock; ; Indie/")), vec!["rock", "indie"]);
    }

    #[test]
    fn jaccard_identical_sets_is_one() {
        let a = normalize(Some("Rock; Indie"));
        let b = normalize(Some("indie / rock"));
        assert!((jaccard(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn jaccard_disjoint_sets_is_zero() {
        let a = normalize(Some("Rock"));
        let b = normalize(Some("Jazz"));
        assert_eq!(jaccard(&a, &b), 0.0);
    }

    #[test]
    fn jaccard_partial_overlap() {
        // {rock, indie} vs {rock, pop} → 1 / 3.
        let a = normalize(Some("Rock; Indie"));
        let b = normalize(Some("Rock; Pop"));
        let j = jaccard(&a, &b);
        assert!((j - (1.0 / 3.0)).abs() < 1e-6, "got {j}");
    }

    #[test]
    fn jaccard_both_empty_is_zero_not_nan() {
        let empty: Vec<String> = Vec::new();
        let j = jaccard(&empty, &empty);
        assert!(j.is_finite());
        assert_eq!(j, 0.0);
    }

    #[test]
    fn jaccard_one_empty_is_zero() {
        let a = normalize(Some("Rock"));
        let empty: Vec<String> = Vec::new();
        assert_eq!(jaccard(&a, &empty), 0.0);
        assert_eq!(jaccard(&empty, &a), 0.0);
    }

    #[test]
    fn normalize_handles_non_ascii_via_unicode_lowercase() {
        // Tags from non-English metadata flow through `str::to_lowercase`,
        // which applies Unicode case folding. Pin current behaviour so a
        // future "ASCII-only" optimisation can't silently break matching
        // for users with German/Greek/Turkish library tags.
        assert_eq!(normalize(Some("Métal")), vec!["métal"]);
        assert_eq!(normalize(Some("ROCK / MÉTAL")), vec!["rock", "métal"]);
        // Distinct tokens after folding stay distinct.
        let toks = normalize(Some("Über; uber"));
        assert_eq!(toks.len(), 2);
        assert!(toks.contains(&"über".to_string()));
        assert!(toks.contains(&"uber".to_string()));
        // Non-Latin scripts: lowercase is a no-op for CJK, but the parser
        // still trims and dedupes.
        assert_eq!(normalize(Some("演歌; 演歌")), vec!["演歌"]);
    }
}
