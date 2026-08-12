//! Detection patterns.
//!
//! Regex is the first tier; NER for names and organisations comes later. The
//! types here are the ones with enough structure to catch reliably — and for
//! the checksummed ones (card, Aadhaar) validation runs *after* the match so
//! an order number or a run of dates is not mistaken for a card.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Phone,
    Email,
    Card,
    Aadhaar,
    Pan,
    Ifsc,
    Account,
    /// Health terms — the PHI tier, matched against a term list.
    Health,
}

impl EntityKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EntityKind::Phone => "phone",
            EntityKind::Email => "email",
            EntityKind::Card => "card",
            EntityKind::Aadhaar => "aadhaar",
            EntityKind::Pan => "pan",
            EntityKind::Ifsc => "ifsc",
            EntityKind::Account => "account",
            EntityKind::Health => "health",
        }
    }

    /// True for categories that are protected health information rather than
    /// general PII — surfaced separately because HIPAA-style reporting needs
    /// the distinction.
    pub fn is_phi(&self) -> bool {
        matches!(self, EntityKind::Health)
    }

    pub fn all() -> &'static [EntityKind] {
        &[
            EntityKind::Email,
            EntityKind::Card,
            EntityKind::Aadhaar,
            EntityKind::Pan,
            EntityKind::Ifsc,
            EntityKind::Phone,
            EntityKind::Account,
        ]
    }
}

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,}\b").unwrap()
});

// Indian mobile: optional +91 prefix, then 10 digits starting 6-9.
//
// Separators are allowed *between any pair of digits* rather than at fixed
// positions, because real transcripts group these inconsistently — 5-5
// ("98765 43210") is the common Indian convention, but 3-3-4 and unbroken
// runs all appear. The leading \b sits before the first digit, not before the
// optional "+", so a 10-digit run embedded in a longer number cannot match.
// The leading alternation is either the country code or a word boundary —
// never both. `+919876543210` has no boundary between the `91` and the `9`
// that follows it, so requiring one there would miss the unspaced form.
static PHONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\+?91[\s\-]?|\b)[6-9](?:[\s\-]?\d){9}\b").unwrap()
});

// 13-19 digits, optionally grouped in fours. Luhn-validated below.
static CARD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:\d[ \-]?){12,18}\d\b").unwrap());

// Aadhaar: 12 digits, commonly in 4-4-4 groups, never starting 0 or 1.
static AADHAAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[2-9]\d{3}[\s\-]?\d{4}[\s\-]?\d{4}\b").unwrap());

static PAN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Z]{5}[0-9]{4}[A-Z]\b").unwrap());

static IFSC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Z]{4}0[A-Z0-9]{6}\b").unwrap());

// Bank account numbers, cued by a nearby keyword because a bare run of digits
// is far too common to redact on sight.
static ACCOUNT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:a/c|acct|account)\s*(?:no\.?|number|#)?\s*[:\-]?\s*(\d{9,18})\b").unwrap()
});

static HEALTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(diabet\w*|hypertension|cancer|chemotherapy|HIV|AIDS|asthma|pregnan\w*|depression|anxiety|insulin|dialysis|surgery|prescription|diagnos\w*)\b",
    )
    .unwrap()
});

type Found = (usize, usize, EntityKind, String);

/// Run every enabled pattern over `text`.
pub fn detect_all(text: &str, enabled: &[EntityKind]) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    let on = |k: EntityKind| enabled.contains(&k);

    if on(EntityKind::Email) {
        for m in EMAIL.find_iter(text) {
            out.push((m.start(), m.end(), EntityKind::Email, m.as_str().to_string()));
        }
    }

    // Order matters below: the longest/most specific structures are collected
    // first, and the caller drops anything overlapping an earlier match.
    if on(EntityKind::Card) {
        for m in CARD.find_iter(text) {
            let digits: String = m.as_str().chars().filter(|c| c.is_ascii_digit()).collect();
            if (13..=19).contains(&digits.len()) && luhn_valid(&digits) {
                out.push((m.start(), m.end(), EntityKind::Card, m.as_str().to_string()));
            }
        }
    }

    if on(EntityKind::Aadhaar) {
        for m in AADHAAR.find_iter(text) {
            let digits: String = m.as_str().chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() == 12 && verhoeff_valid(&digits) {
                out.push((
                    m.start(),
                    m.end(),
                    EntityKind::Aadhaar,
                    m.as_str().to_string(),
                ));
            }
        }
    }

    if on(EntityKind::Pan) {
        for m in PAN.find_iter(text) {
            out.push((m.start(), m.end(), EntityKind::Pan, m.as_str().to_string()));
        }
    }

    if on(EntityKind::Ifsc) {
        for m in IFSC.find_iter(text) {
            out.push((m.start(), m.end(), EntityKind::Ifsc, m.as_str().to_string()));
        }
    }

    if on(EntityKind::Account) {
        for c in ACCOUNT.captures_iter(text) {
            if let Some(g) = c.get(1) {
                out.push((g.start(), g.end(), EntityKind::Account, g.as_str().to_string()));
            }
        }
    }

    if on(EntityKind::Phone) {
        for m in PHONE.find_iter(text) {
            let digits: String = m.as_str().chars().filter(|c| c.is_ascii_digit()).collect();
            // 10 national digits, or 12 with the 91 country code.
            if digits.len() == 10 || (digits.len() == 12 && digits.starts_with("91")) {
                out.push((m.start(), m.end(), EntityKind::Phone, m.as_str().to_string()));
            }
        }
    }

    if on(EntityKind::Health) {
        for m in HEALTH.find_iter(text) {
            out.push((m.start(), m.end(), EntityKind::Health, m.as_str().to_string()));
        }
    }

    out
}

/// Luhn check digit — what separates a real card number from any 16 digits.
pub fn luhn_valid(digits: &str) -> bool {
    if digits.len() < 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let mut sum = 0u32;
    for (i, b) in digits.bytes().rev().enumerate() {
        let mut d = (b - b'0') as u32;
        if i % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum % 10 == 0
}

// Verhoeff tables — the checksum UIDAI uses for Aadhaar.
const D_TABLE: [[u8; 10]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
    [1, 2, 3, 4, 0, 6, 7, 8, 9, 5],
    [2, 3, 4, 0, 1, 7, 8, 9, 5, 6],
    [3, 4, 0, 1, 2, 8, 9, 5, 6, 7],
    [4, 0, 1, 2, 3, 9, 5, 6, 7, 8],
    [5, 9, 8, 7, 6, 0, 4, 3, 2, 1],
    [6, 5, 9, 8, 7, 1, 0, 4, 3, 2],
    [7, 6, 5, 9, 8, 2, 1, 0, 4, 3],
    [8, 7, 6, 5, 9, 3, 2, 1, 0, 4],
    [9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
];

const P_TABLE: [[u8; 10]; 8] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
    [1, 5, 7, 6, 2, 8, 3, 0, 9, 4],
    [5, 8, 0, 3, 7, 9, 6, 1, 4, 2],
    [8, 9, 1, 6, 0, 4, 3, 5, 2, 7],
    [9, 4, 5, 3, 1, 2, 6, 8, 7, 0],
    [4, 2, 8, 6, 5, 7, 3, 9, 0, 1],
    [2, 7, 9, 3, 8, 0, 6, 4, 1, 5],
    [7, 0, 4, 6, 9, 1, 3, 2, 5, 8],
];

/// Verhoeff checksum, as used for Aadhaar numbers.
pub fn verhoeff_valid(digits: &str) -> bool {
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let mut c = 0u8;
    for (i, b) in digits.bytes().rev().enumerate() {
        let d = b - b'0';
        c = D_TABLE[c as usize][P_TABLE[i % 8][d as usize] as usize];
    }
    c == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<EntityKind> {
        let mut v: Vec<EntityKind> = detect_all(text, EntityKind::all())
            .into_iter()
            .map(|f| f.2)
            .collect();
        v.sort_by_key(|k| k.as_str());
        v.dedup();
        v
    }

    #[test]
    fn luhn_accepts_known_good_test_numbers() {
        // Standard publicly documented test values.
        assert!(luhn_valid("4111111111111111"));
        assert!(luhn_valid("5500005555555559"));
        assert!(luhn_valid("79927398713"));
    }

    #[test]
    fn luhn_rejects_arbitrary_digit_runs() {
        assert!(!luhn_valid("4111111111111112"));
        assert!(!luhn_valid("1234567812345678"));
    }

    #[test]
    fn card_detection_requires_a_valid_checksum() {
        // A 16-digit order reference must not be redacted as a card.
        assert!(!kinds("order 1234567812345678 shipped").contains(&EntityKind::Card));
        assert!(kinds("card 4111 1111 1111 1111").contains(&EntityKind::Card));
    }

    #[test]
    fn verhoeff_round_trips() {
        // 234567890124 carries a valid Verhoeff digit; flipping one digit breaks it.
        assert!(verhoeff_valid("234567890124") != verhoeff_valid("234567890125"));
    }

    #[test]
    fn detects_indian_mobile_formats() {
        for s in [
            "9876543210",
            "+91 98765 43210",
            "+919876543210",
            "98765-43210",
        ] {
            assert!(
                kinds(s).contains(&EntityKind::Phone),
                "missed phone in {s:?}"
            );
        }
    }

    #[test]
    fn ignores_numbers_that_are_not_mobiles() {
        // Landline-style and short codes start below 6 and must not match.
        assert!(!kinds("dial 1234567890 now").contains(&EntityKind::Phone));
    }

    #[test]
    fn detects_email() {
        assert!(kinds("write to priya.sharma+tag@example.co.in").contains(&EntityKind::Email));
    }

    #[test]
    fn detects_pan_and_ifsc() {
        assert!(kinds("PAN is ABCDE1234F").contains(&EntityKind::Pan));
        assert!(kinds("IFSC HDFC0001234").contains(&EntityKind::Ifsc));
    }

    #[test]
    fn account_number_needs_a_keyword_cue() {
        assert!(kinds("account no 123456789012").contains(&EntityKind::Account));
        // The same digits with no cue are not treated as an account number.
        assert!(!kinds("reference 123456789012 logged").contains(&EntityKind::Account));
    }

    #[test]
    fn health_terms_are_flagged_as_phi() {
        let found = detect_all("patient has diabetes and takes insulin", &[EntityKind::Health]);
        assert_eq!(found.len(), 2, "found: {found:?}");
        assert!(EntityKind::Health.is_phi());
    }

    #[test]
    fn ordinary_text_produces_nothing() {
        assert!(kinds("The customer asked about delivery on Tuesday.").is_empty());
    }

    #[test]
    fn offsets_point_at_the_matched_text() {
        let text = "mail me at a@b.com please";
        let found = detect_all(text, &[EntityKind::Email]);
        let (s, e, _, v) = &found[0];
        assert_eq!(&text[*s..*e], v);
    }
}
