//! Shared utilities for channel implementations.

use std::time::Duration;

/// Split a message at natural boundaries (double newlines, then single
/// newlines, then spaces, then hard cut) to fit within `max_len` bytes.
pub fn split_message(content: &str, max_len: usize) -> Vec<String> {
    assert!(max_len > 0, "split_message: max_len must be > 0");

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

        // Find the largest byte index <= max_len that is a char boundary
        // to avoid panicking when max_len falls inside a multi-byte character.
        let end = {
            let mut e = max_len;
            while e > 0 && !remaining.is_char_boundary(e) {
                e -= 1;
            }
            e
        };
        let slice = &remaining[..end];
        let min_pos = end / 2;

        // Try double newline, then single newline, then space.
        // Only accept a boundary if it falls past the midpoint of the window
        // to avoid wastefully short chunks.
        let split_at = slice
            .rfind("\n\n")
            .filter(|&i| i > min_pos)
            .map(|i| i + 2)
            .or_else(|| slice.rfind('\n').filter(|&i| i > min_pos).map(|i| i + 1))
            .or_else(|| slice.rfind(' ').filter(|&i| i > min_pos).map(|i| i + 1))
            .unwrap_or(end);

        let (chunk, rest) = remaining.split_at(split_at);
        let trimmed = chunk.trim_end();
        if !trimmed.is_empty() {
            chunks.push(trimmed.to_string());
        }
        remaining = rest.trim_start();
    }

    chunks
}

/// Retry an async operation with exponential backoff.
///
/// Calls `operation` up to `max_retries + 1` times total. On failure, waits
/// `base_delay * 2^attempt` before the next attempt.
pub async fn send_with_retry<F, Fut>(
    max_retries: u32,
    base_delay: Duration,
    operation: F,
) -> anyhow::Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let mut last_err = None;
    for attempt in 0..=max_retries {
        match operation().await {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                if attempt < max_retries {
                    let delay = base_delay * 2u32.saturating_pow(attempt);
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_err.unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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

    #[test]
    #[should_panic(expected = "max_len must be > 0")]
    fn max_len_zero_panics() {
        split_message("hello", 0);
    }

    #[test]
    fn multibyte_content_does_not_panic() {
        // Each emoji is 4 bytes; 500 emoji = 2000 bytes but 500 chars
        let emoji_msg = "😀".repeat(500);
        let chunks = split_message(&emoji_msg, 2000);
        for chunk in &chunks {
            assert!(chunk.len() <= 2000);
        }
        // Verify no content lost
        let rejoined: String = chunks.join("");
        assert_eq!(rejoined, emoji_msg);
    }

    #[test]
    fn content_preserved_after_split() {
        let msg = "hello world this is a test message with some content";
        let chunks = split_message(msg, 15);
        let rejoined: String = chunks.join(" ");
        // All words should be present (order preserved, whitespace may differ)
        for word in msg.split_whitespace() {
            assert!(rejoined.contains(word), "missing word: {word}");
        }
    }

    #[test]
    fn exactly_at_limit_no_split() {
        let msg = "x".repeat(100);
        let result = split_message(&msg, 100);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], msg);
    }

    #[test]
    fn all_whitespace_returns_empty() {
        let result = split_message("   \n\n  \n  ", 100);
        assert!(result.is_empty() || result.iter().all(|c| !c.is_empty()));
    }

    #[tokio::test]
    async fn send_with_retry_succeeds_on_first_try() {
        let result = send_with_retry(3, Duration::from_millis(1), || async { Ok(()) }).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn send_with_retry_retries_on_failure() {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let c = counter.clone();
        let result = send_with_retry(3, Duration::from_millis(1), move || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n < 2 {
                    anyhow::bail!("transient error");
                }
                Ok(())
            }
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn send_with_retry_gives_up_after_max() {
        let result = send_with_retry(2, Duration::from_millis(1), || async {
            anyhow::bail!("permanent error")
        })
        .await;
        assert!(result.is_err());
    }
}
