pub const STOP_WORDS: [&str; 46] = [
    "a", "an", "and", "are", "as", "at", "be", "been", "being", "by", "can", "could", "did", "do",
    "does", "for", "from", "had", "has", "have", "how", "i", "in", "into", "is", "it", "of", "on",
    "or", "please", "should", "show", "that", "the", "this", "to", "was", "were", "what", "when",
    "where", "which", "who", "why", "with", "would",
];

#[must_use]
pub fn is_stop_word(term: &str) -> bool {
    STOP_WORDS.binary_search(&term).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stop_words_sorted_and_unique() {
        for window in STOP_WORDS.windows(2) {
            assert!(
                window[0] < window[1],
                "stop words table must be strictly sorted and unique: {} >= {}",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn test_is_stop_word() {
        assert!(is_stop_word("where"));
        assert!(is_stop_word("is"));
        assert!(!is_stop_word("handled"));
        assert!(!is_stop_word("token"));
        assert!(!is_stop_word("verification"));
        assert!(!is_stop_word("verify"));
    }
}
