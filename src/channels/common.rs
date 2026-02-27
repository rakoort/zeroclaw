//! Shared utilities for channel implementations.

/// Split a message at natural boundaries (double newlines, then single
/// newlines, then spaces, then hard cut) to fit within `max_len` bytes.
pub fn split_message(content: &str, max_len: usize) -> Vec<String> {
    if content.is_empty() {
        return Vec::new();
    }
    if content.len() <= max_len {
        return vec![content.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = content;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }

        let slice = &remaining[..max_len];
        let min_pos = max_len / 2;

        // Try double newline, then single newline, then space.
        // Only accept a boundary if it falls past the midpoint of the window
        // to avoid wastefully short chunks.
        let split_at = slice
            .rfind("\n\n")
            .filter(|&i| i > min_pos)
            .map(|i| i + 2)
            .or_else(|| slice.rfind('\n').filter(|&i| i > min_pos).map(|i| i + 1))
            .or_else(|| slice.rfind(' ').filter(|&i| i > min_pos).map(|i| i + 1))
            .unwrap_or(max_len);

        let (chunk, rest) = remaining.split_at(split_at);
        let trimmed = chunk.trim_end();
        if !trimmed.is_empty() {
            chunks.push(trimmed.to_string());
        }
        remaining = rest.trim_start();
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_message_returns_single_chunk() {
        let result = split_message("hello", 100);
        assert_eq!(result, vec!["hello"]);
    }

    #[test]
    fn empty_message_returns_empty_vec() {
        let result = split_message("", 100);
        assert!(result.is_empty());
    }

    #[test]
    fn splits_at_double_newline_first() {
        let msg = "part one\n\npart two";
        let result = split_message(msg, 12);
        assert_eq!(result, vec!["part one", "part two"]);
    }

    #[test]
    fn splits_at_single_newline_when_no_double() {
        let msg = "line one\nline two";
        let result = split_message(msg, 12);
        assert_eq!(result, vec!["line one", "line two"]);
    }

    #[test]
    fn splits_at_space_when_no_newline() {
        let msg = "word1 word2 word3";
        let result = split_message(msg, 11);
        assert_eq!(result, vec!["word1 word2", "word3"]);
    }

    #[test]
    fn hard_splits_when_no_boundary() {
        let msg = "abcdefghij";
        let result = split_message(msg, 5);
        assert_eq!(result, vec!["abcde", "fghij"]);
    }

    #[test]
    fn respects_discord_limit() {
        let long_msg = "x".repeat(4500);
        let chunks = split_message(&long_msg, 2000);
        for chunk in &chunks {
            assert!(chunk.len() <= 2000);
        }
        let rejoined: String = chunks.join("");
        assert_eq!(rejoined.len(), 4500);
    }

    #[test]
    fn respects_telegram_limit() {
        let long_msg = "x".repeat(8000);
        let chunks = split_message(&long_msg, 4096);
        for chunk in &chunks {
            assert!(chunk.len() <= 4096);
        }
    }
}
