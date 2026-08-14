//! Who was on a call, derived from what the recorder named the file.
//!
//! The core is a pure function over two strings, so the whole crate tests
//! without a database, a filesystem or a network — the same shape as
//! `ca-rules`.
//!
//! Attribution is always best-effort. A filename that matches nothing yields
//! an empty [`Attribution`] rather than a guess, and the call stays exactly as
//! usable as it was before.

mod patterns;

pub use patterns::{builtin_patterns, patterns_from_json, DateFormat, Pattern};

use ca_core::identity::PersonKind;
use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
use regex::Captures;

/// One side of a call, before it has been reconciled with the `people` table.
#[derive(Debug, Clone, PartialEq)]
pub struct Handle {
    pub kind: PersonKind,
    /// As written in the filename.
    pub display_name: String,
    /// Normalised identity key.
    pub handle: String,
}

impl Handle {
    fn new(kind: PersonKind, raw: &str) -> Self {
        Self {
            kind,
            display_name: raw.trim().to_string(),
            handle: normalise_handle(raw),
        }
    }
}

/// What a filename said about a call.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Attribution {
    pub agent: Option<Handle>,
    pub contact: Option<Handle>,
    pub occurred_at: Option<DateTime<Utc>>,
    /// Which pattern matched, so a wrong attribution is traceable to the rule
    /// that produced it rather than being a mystery.
    pub rule: Option<String>,
}

impl Attribution {
    /// Whether anything at all was learned.
    pub fn is_empty(&self) -> bool {
        self.agent.is_none() && self.contact.is_none() && self.occurred_at.is_none()
    }
}

/// Apply patterns in order; the first one that learns something wins.
///
/// A pattern that matches but captures nothing useful is skipped rather than
/// returned, so a broad rule sitting early in the list cannot mask a precise
/// one behind it.
pub fn attribute(display_name: &str, source_key: &str, patterns: &[Pattern]) -> Attribution {
    for p in patterns {
        let hay = if p.on_source_key {
            source_key
        } else {
            display_name
        };
        let Some(caps) = p.regex.captures(hay) else {
            continue;
        };
        let found = Attribution {
            agent: caps
                .name("agent")
                .map(|m| Handle::new(PersonKind::Agent, m.as_str())),
            contact: caps
                .name("contact")
                .map(|m| Handle::new(PersonKind::Contact, m.as_str())),
            occurred_at: parse_when(&caps, p.date_format),
            rule: Some(p.name.clone()),
        };
        if found.is_empty() {
            continue;
        }
        return found;
    }
    Attribution::default()
}

/// Fold a raw participant string into a stable identity key.
///
/// Numbers reduce to their digits with a leading `+` preserved; anything else
/// is lowercased with runs of whitespace collapsed.
///
/// Deliberately **not** E.164. Turning `08618075571` into an international
/// number means guessing a country code, and a wrong guess merges two
/// unrelated people under one identity — a worse failure than leaving the
/// number in the form it arrived in.
pub fn normalise_handle(raw: &str) -> String {
    let trimmed = raw.trim();
    let looks_numeric = !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_digit() || " +-()".contains(c))
        && trimmed.chars().any(|c| c.is_ascii_digit());

    if looks_numeric {
        let plus = trimmed.starts_with('+');
        let digits: String = trimmed.chars().filter(char::is_ascii_digit).collect();
        return if plus { format!("+{digits}") } else { digits };
    }

    trimmed
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Filenames carry local wall-clock time with no zone, so local is assumed and
/// the result stored as UTC. A recording moved between machines in different
/// zones is off by the offset; that is documented rather than hidden.
fn parse_when(caps: &Captures, fmt: DateFormat) -> Option<DateTime<Utc>> {
    let (ymd, hms) = (caps.name("ymd")?.as_str(), caps.name("hms")?.as_str());
    let (y, mo, d) = match fmt {
        DateFormat::None => return None,
        DateFormat::YyMmDd if ymd.len() == 6 => (
            2000 + ymd[0..2].parse::<i32>().ok()?,
            ymd[2..4].parse().ok()?,
            ymd[4..6].parse().ok()?,
        ),
        DateFormat::YyyyMmDd if ymd.len() == 8 => (
            ymd[0..4].parse().ok()?,
            ymd[4..6].parse().ok()?,
            ymd[6..8].parse().ok()?,
        ),
        _ => return None,
    };
    if hms.len() != 6 {
        return None;
    }
    let naive = NaiveDate::from_ymd_opt(y, mo, d)?.and_hms_opt(
        hms[0..2].parse().ok()?,
        hms[2..4].parse().ok()?,
        hms[4..6].parse().ok()?,
    )?;
    // `earliest` rather than `single`: on the hour a daylight-saving fold
    // repeats, `single` returns nothing and the call would lose its time.
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attr(name: &str) -> Attribution {
        attribute(name, "", &builtin_patterns())
    }

    /// The shape that covers 100% of the current corpus.
    #[test]
    fn reads_the_contact_and_the_time_from_an_android_recording() {
        let a = attr("Call recording +918485823280_251112_142326.m4a");
        let c = a.contact.clone().expect("contact");
        assert_eq!(c.kind, PersonKind::Contact);
        assert_eq!(c.display_name, "+918485823280");
        assert_eq!(c.handle, "+918485823280");
        assert_eq!(a.rule.as_deref(), Some("android_call_recorder"));
        // A phone names the counterparty, never the handler.
        assert!(a.agent.is_none());
        assert!(a.occurred_at.is_some());
    }

    #[test]
    fn a_saved_contact_name_survives_spaces_and_emoji() {
        let a = attr("Call recording Ishita AI Cloud Koradia_251007_101533.m4a");
        assert_eq!(a.contact.unwrap().display_name, "Ishita AI Cloud Koradia");

        let a = attr("Call recording \u{26d4} Spam Alert From Jio_250903_090102.m4a");
        assert_eq!(
            a.contact.unwrap().display_name,
            "\u{26d4} Spam Alert From Jio"
        );
    }

    /// The whole point of parsing: `created_at` is ingest time and useless as
    /// an axis. A two-digit year is read as 20yy.
    #[test]
    fn the_timestamp_is_the_call_time_not_the_ingest_time() {
        let a = attr("Call recording +911409098039_251107_123936.m4a");
        let when = a.occurred_at.expect("timestamp");
        let local = when.with_timezone(&Local);
        assert_eq!(
            local.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2025-11-07 12:39:36"
        );
    }

    #[test]
    fn a_contact_centre_export_names_both_sides() {
        let a = attribute(
            "priya.sharma_+919876543210_20250114_093000.wav",
            "",
            &builtin_patterns(),
        );
        assert_eq!(a.agent.as_ref().unwrap().display_name, "priya.sharma");
        assert_eq!(a.agent.as_ref().unwrap().kind, PersonKind::Agent);
        assert_eq!(a.contact.as_ref().unwrap().display_name, "+919876543210");
        assert_eq!(a.rule.as_deref(), Some("agent_customer_date"));
    }

    #[test]
    fn an_unrecognised_name_attributes_nothing_rather_than_guessing() {
        let a = attr("meeting notes.m4a");
        assert!(a.agent.is_none() && a.contact.is_none());
        assert!(a.occurred_at.is_none());
        assert!(a.rule.is_none());
    }

    #[test]
    fn an_impossible_date_yields_no_timestamp_but_still_yields_the_contact() {
        let a = attr("Call recording +919999999999_259947_996199.m4a");
        assert!(a.contact.is_some());
        assert!(a.occurred_at.is_none());
    }

    #[test]
    fn numbers_reduce_to_digits_and_names_fold() {
        assert_eq!(normalise_handle("+91 8485-823280"), "+918485823280");
        assert_eq!(normalise_handle("08618075571"), "08618075571");
        assert_eq!(normalise_handle("  Babulal   Kavaiya "), "babulal kavaiya");
        assert_eq!(normalise_handle("BABULAL KAVAIYA"), "babulal kavaiya");
    }

    /// Guessing a country code would merge two unrelated people under one
    /// identity, which is worse than leaving the number as it arrived.
    #[test]
    fn a_local_number_is_not_promoted_to_an_international_one() {
        assert_ne!(
            normalise_handle("08618075571"),
            normalise_handle("+918618075571")
        );
    }

    #[test]
    fn a_pattern_can_match_the_source_key_so_folder_per_agent_needs_no_special_case() {
        let json = r#"[{
            "name": "folder_per_agent",
            "regex": "[/\\\\](?P<agent>[^/\\\\]+)[/\\\\][^/\\\\]+$",
            "on_source_key": true,
            "date_format": "none"
        }]"#;
        let patterns = patterns_from_json(json).expect("valid patterns");
        let a = attribute(
            "whatever.wav",
            "C:/calls/priya.sharma/whatever.wav",
            &patterns,
        );
        assert_eq!(a.agent.unwrap().display_name, "priya.sharma");
    }

    #[test]
    fn a_malformed_pattern_is_rejected_with_a_reason() {
        let err = patterns_from_json(
            r#"[{"name":"bad","regex":"(unclosed","on_source_key":false,"date_format":"none"}]"#,
        )
        .expect_err("must reject");
        assert!(
            format!("{err}").contains("bad"),
            "the error names the pattern: {err}"
        );
    }
}
