//! Redact-before-cloud.
//!
//! Every transcript passes through here before any network call. Detected
//! personal data is replaced with stable tokens (`[PHONE_1]`), and the mapping
//! back to real values stays on this machine. Cloud providers only ever see
//! tokenised text; the UI rehydrates real values for display.
//!
//! Two properties matter more than raw recall:
//!
//! - **Stability.** The same value always maps to the same token within a
//!   call, so "call me on [PHONE_1]" and "[PHONE_1] is my mobile" remain
//!   recognisably about one number. A model cannot reason about a caller who
//!   is renamed on every mention.
//! - **Precision on checksummed types.** Card and Aadhaar numbers are
//!   validated, not merely pattern-matched, so an order reference or a date
//!   range is not silently mangled into `[CARD_1]`.

pub mod patterns;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub use patterns::EntityKind;

/// One detected span in the source text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    pub kind: EntityKind,
    /// Byte offsets into the original text.
    pub start: usize,
    pub end: usize,
    pub value: String,
    pub token: String,
}

/// Reversible mapping for one call. Persisted encrypted, never transmitted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RedactionMap {
    /// token -> original value
    pub entries: HashMap<String, String>,
}

impl RedactionMap {
    pub fn restore(&self, redacted: &str) -> String {
        // Longest token first so `[PHONE_10]` is not clobbered by `[PHONE_1]`.
        let mut tokens: Vec<&String> = self.entries.keys().collect();
        tokens.sort_by_key(|t| std::cmp::Reverse(t.len()));

        let mut out = redacted.to_string();
        for token in tokens {
            if let Some(value) = self.entries.get(token) {
                out = out.replace(token.as_str(), value);
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Debug, Clone)]
pub struct RedactionResult {
    pub text: String,
    pub detections: Vec<Detection>,
}

/// Assigns stable tokens across every string it processes.
///
/// Reuse one instance for a whole call so numbering is consistent across
/// segments; create a new one per call so tokens restart at 1.
pub struct Redactor {
    /// value -> token, so a repeated value keeps its original token.
    assigned: HashMap<String, String>,
    counters: HashMap<EntityKind, usize>,
    map: RedactionMap,
    enabled: Vec<EntityKind>,
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new(EntityKind::all().to_vec())
    }
}

impl Redactor {
    pub fn new(enabled: Vec<EntityKind>) -> Self {
        Self {
            assigned: HashMap::new(),
            counters: HashMap::new(),
            map: RedactionMap::default(),
            enabled,
        }
    }

    /// The accumulated reversible map. Keep this local and encrypted.
    pub fn map(&self) -> &RedactionMap {
        &self.map
    }

    pub fn into_map(self) -> RedactionMap {
        self.map
    }

    /// Replace personal data in `text` with stable tokens.
    pub fn redact(&mut self, text: &str) -> RedactionResult {
        let mut found = patterns::detect_all(text, &self.enabled);

        // Longest-first at each position so a card number is not partially
        // consumed by the shorter phone pattern sitting inside it.
        found.sort_by(|a, b| a.0.cmp(&b.0).then((b.1 - b.0).cmp(&(a.1 - a.0))));

        let mut detections = Vec::new();
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0usize;

        for (start, end, kind, value) in found {
            // Skip anything overlapping a span we already replaced.
            if start < cursor {
                continue;
            }
            let token = self.token_for(kind, &value);
            out.push_str(&text[cursor..start]);
            out.push_str(&token);
            detections.push(Detection {
                kind,
                start,
                end,
                value,
                token,
            });
            cursor = end;
        }
        out.push_str(&text[cursor..]);

        RedactionResult {
            text: out,
            detections,
        }
    }

    fn token_for(&mut self, kind: EntityKind, value: &str) -> String {
        // Key on kind too: the same digits could legitimately be seen as two
        // different entity types in different contexts.
        let key = format!("{}:{}", kind.as_str(), value);
        if let Some(existing) = self.assigned.get(&key) {
            return existing.clone();
        }
        let n = self.counters.entry(kind).or_insert(0);
        *n += 1;
        let token = format!("[{}_{}]", kind.as_str().to_uppercase(), n);
        self.assigned.insert(key, token.clone());
        self.map.entries.insert(token.clone(), value.to_string());
        token
    }
}

/// Convenience for a single string with default settings.
pub fn redact_once(text: &str) -> (String, RedactionMap) {
    let mut r = Redactor::default();
    let out = r.redact(text);
    (out.text, r.into_map())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_a_phone_number() {
        let (out, map) = redact_once("Call me on +91 98765 43210 tomorrow");
        assert!(out.contains("[PHONE_1]"), "got: {out}");
        assert!(!out.contains("98765"), "raw digits leaked: {out}");
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn the_same_value_keeps_the_same_token() {
        let mut r = Redactor::default();
        let a = r.redact("my number is 9876543210");
        let b = r.redact("yes, 9876543210 is correct");
        let tok_a = a.detections[0].token.clone();
        let tok_b = b.detections[0].token.clone();
        assert_eq!(tok_a, tok_b, "same number must keep one identity");
    }

    #[test]
    fn different_values_get_different_tokens() {
        let mut r = Redactor::default();
        let out = r.redact("call 9876543210 or 9123456789");
        assert!(
            out.text.contains("[PHONE_1]") && out.text.contains("[PHONE_2]"),
            "{}",
            out.text
        );
    }

    #[test]
    fn restore_recovers_the_original_text() {
        let original = "Reach me at priya@example.com or 9876543210.";
        let (redacted, map) = redact_once(original);
        assert_ne!(redacted, original);
        assert_eq!(map.restore(&redacted), original);
    }

    #[test]
    fn restore_handles_double_digit_token_numbers() {
        // [PHONE_1] must not clobber [PHONE_10] during restore.
        let mut r = Redactor::default();
        let mut text = String::new();
        for i in 0..12 {
            text.push_str(&format!("98765432{:02} ", i));
        }
        let out = r.redact(&text);
        let map = r.into_map();
        assert!(map.len() >= 11, "expected many numbers, got {}", map.len());
        assert_eq!(map.restore(&out.text), text);
    }

    #[test]
    fn text_without_pii_is_unchanged() {
        let s = "The customer asked about the delivery schedule.";
        let (out, map) = redact_once(s);
        assert_eq!(out, s);
        assert!(map.is_empty());
    }

    #[test]
    fn disabled_kinds_are_left_alone() {
        let mut r = Redactor::new(vec![EntityKind::Email]);
        let out = r.redact("call 9876543210 or mail a@b.com");
        assert!(out.text.contains("9876543210"), "phone should be untouched");
        assert!(out.text.contains("[EMAIL_1]"), "{}", out.text);
    }

    #[test]
    fn multiple_entity_types_in_one_line() {
        let (out, _) = redact_once("PAN ABCDE1234F, email x@y.com, mobile 9876543210");
        assert!(out.contains("[PAN_1]"), "{out}");
        assert!(out.contains("[EMAIL_1]"), "{out}");
        assert!(out.contains("[PHONE_1]"), "{out}");
    }

    #[test]
    fn detections_carry_usable_offsets() {
        let text = "my email is a@b.com ok";
        let mut r = Redactor::default();
        let out = r.redact(text);
        let d = &out.detections[0];
        assert_eq!(&text[d.start..d.end], d.value);
    }

    #[test]
    fn empty_input_is_handled() {
        let (out, map) = redact_once("");
        assert!(out.is_empty() && map.is_empty());
    }
}
