pub const STEM_PAIRS: [(&str, &str); 38] = [
    ("authenticated", "authenticate"),
    ("authenticating", "authenticate"),
    ("authentication", "authenticate"),
    ("authorization", "authorize"),
    ("configuration", "configure"),
    ("configured", "configure"),
    ("configuring", "configure"),
    ("deserialization", "deserialize"),
    ("deserialized", "deserialize"),
    ("deserializing", "deserialize"),
    ("generated", "generate"),
    ("generating", "generate"),
    ("generation", "generate"),
    ("handled", "handle"),
    ("handling", "handle"),
    ("indexed", "index"),
    ("indexing", "index"),
    ("initialization", "initialize"),
    ("initialized", "initialize"),
    ("initializing", "initialize"),
    ("normalization", "normalize"),
    ("normalized", "normalize"),
    ("normalizing", "normalize"),
    ("parsed", "parse"),
    ("parsing", "parse"),
    ("resolution", "resolve"),
    ("resolved", "resolve"),
    ("resolving", "resolve"),
    ("routed", "route"),
    ("routing", "route"),
    ("serialization", "serialize"),
    ("serialized", "serialize"),
    ("serializing", "serialize"),
    ("validated", "validate"),
    ("validating", "validate"),
    ("validation", "validate"),
    ("verification", "verify"),
    ("verifications", "verify"),
];

#[must_use]
pub fn stem_lookup(term: &str) -> Option<&'static str> {
    STEM_PAIRS
        .binary_search_by_key(&term, |&(surface, _)| surface)
        .ok()
        .map(|idx| STEM_PAIRS[idx].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stem_pairs_sorted_and_unique() {
        for window in STEM_PAIRS.windows(2) {
            assert!(
                window[0].0 < window[1].0,
                "stem pairs must be strictly sorted and unique: {} >= {}",
                window[0].0,
                window[1].0
            );
        }
    }

    #[test]
    fn test_stem_lookup() {
        assert_eq!(stem_lookup("verification"), Some("verify"));
        assert_eq!(stem_lookup("verifications"), Some("verify"));
        assert_eq!(stem_lookup("authenticating"), Some("authenticate"));
        assert_eq!(stem_lookup("verify"), None);
        assert_eq!(stem_lookup("unknown"), None);
    }
}
