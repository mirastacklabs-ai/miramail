//! Token budgets (task A3a): a cheap offline estimator and the three caps (per turn, per
//! conversation, per day). Estimates deliberately over-count (about 3 bytes per token) so the caps
//! err on the side of protecting the user's wallet; provider-reported usage replaces them once known.

use crate::types::{ChatMessage, Part};

pub const DEFAULT_PER_TURN: u64 = 30_000;
pub const DEFAULT_PER_CONVERSATION: u64 = 300_000;
pub const DEFAULT_PER_DAY: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// Max estimated input tokens of one request.
    pub per_turn: u64,
    /// Max input + output tokens over the life of one conversation.
    pub per_conversation: u64,
    /// Max input + output tokens per UTC day over all conversations.
    pub per_day: u64,
}

impl Default for Caps {
    fn default() -> Self {
        Caps { per_turn: DEFAULT_PER_TURN, per_conversation: DEFAULT_PER_CONVERSATION, per_day: DEFAULT_PER_DAY }
    }
}

/// Upper-biased token estimate: ceil(bytes / 3).
pub fn estimate_tokens(s: &str) -> u64 {
    (s.len() as u64).div_ceil(3)
}

pub fn estimate_message(m: &ChatMessage) -> u64 {
    // +4 per message for role / framing overhead.
    4 + m
        .parts
        .iter()
        .map(|p| match p {
            Part::Text { text } => estimate_tokens(text),
            Part::ToolCall { name, args, .. } => estimate_tokens(name) + estimate_tokens(&args.to_string()),
            Part::ToolResult { content, .. } => estimate_tokens(content),
            Part::Opaque { raw, .. } => estimate_tokens(&raw.to_string()),
        })
        .sum::<u64>()
}

/// Which cap was hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapHit {
    Turn,
    Conversation,
    Day,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetError {
    pub cap: CapHit,
    pub message: String,
}

fn fmt_n(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Check a request of `request_est` input tokens against the caps. `conv_used` / `day_used` are the
/// tokens already counted. The per-turn cap is checked first.
pub fn check(caps: &Caps, request_est: u64, conv_used: u64, day_used: u64) -> Result<(), BudgetError> {
    if request_est > caps.per_turn {
        return Err(BudgetError {
            cap: CapHit::Turn,
            message: format!(
                "This message is too long for one request (about {} tokens; your per-request limit is {}). Shorten it or raise the limit in Settings > AI.",
                fmt_n(request_est),
                fmt_n(caps.per_turn)
            ),
        });
    }
    if conv_used + request_est > caps.per_conversation {
        return Err(BudgetError {
            cap: CapHit::Conversation,
            message: format!(
                "This conversation has reached its token limit ({} tokens). Start a new conversation, or raise the limit in Settings > AI.",
                fmt_n(caps.per_conversation)
            ),
        });
    }
    if day_used + request_est > caps.per_day {
        return Err(BudgetError {
            cap: CapHit::Day,
            message: format!(
                "You have reached today's AI token limit ({} tokens). It resets at midnight UTC, or raise the limit in Settings > AI.",
                fmt_n(caps.per_day)
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_is_upper_biased_and_unicode_safe() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens("abcd"), 2);
        assert!(estimate_tokens("🦀🦀🦀") >= 4);
    }

    #[test]
    fn caps_in_order() {
        let c = Caps { per_turn: 100, per_conversation: 1000, per_day: 2000 };
        assert!(check(&c, 100, 0, 0).is_ok());
        assert_eq!(check(&c, 101, 0, 0).unwrap_err().cap, CapHit::Turn);
        assert_eq!(check(&c, 50, 951, 0).unwrap_err().cap, CapHit::Conversation);
        assert_eq!(check(&c, 50, 0, 1951).unwrap_err().cap, CapHit::Day);
        assert!(check(&c, 50, 950, 1950).is_ok());
    }

    #[test]
    fn numbers_are_grouped() {
        assert_eq!(fmt_n(1_234_567), "1,234,567");
        assert_eq!(fmt_n(999), "999");
        assert_eq!(fmt_n(1000), "1,000");
    }
}
