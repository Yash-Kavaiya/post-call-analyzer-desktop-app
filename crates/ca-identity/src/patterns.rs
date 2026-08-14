//! Filename patterns, and how to read a timestamp out of one.

use ca_core::error::{CaError, Result};
use regex::Regex;
use serde::Deserialize;

/// How to read the `ymd` capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateFormat {
    /// No timestamp in this pattern.
    None,
    /// `yymmdd`, read as 20yy.
    YyMmDd,
    /// `yyyymmdd`.
    YyyyMmDd,
}

/// One attribution rule.
///
/// Captures are read by name: `agent`, `contact`, `ymd`, `hms`. Any of them may
/// be absent — a pattern that names only the contact is normal, because a
/// personal call recorder has only one side to name.
#[derive(Debug, Clone)]
pub struct Pattern {
    pub name: String,
    pub regex: Regex,
    /// Match against the source key (the path) rather than the display name.
    pub on_source_key: bool,
    pub date_format: DateFormat,
}

#[derive(Deserialize)]
struct PatternSpec {
    name: String,
    regex: String,
    #[serde(default)]
    on_source_key: bool,
    #[serde(default = "no_date")]
    date_format: DateFormat,
}

fn no_date() -> DateFormat {
    DateFormat::None
}

/// The patterns that ship in the binary, tried in order.
///
/// The Android recorder comes first: its literal prefix makes it the more
/// specific of the two, and a generic pattern placed first would swallow names
/// it has no business claiming.
pub fn builtin_patterns() -> Vec<Pattern> {
    vec![
        Pattern {
            name: "android_call_recorder".into(),
            regex: Regex::new(
                r"^Call recording (?P<contact>.+?)_(?P<ymd>\d{6})_(?P<hms>\d{6})\.\w+$",
            )
            .expect("builtin pattern compiles"),
            on_source_key: false,
            date_format: DateFormat::YyMmDd,
        },
        Pattern {
            name: "agent_customer_date".into(),
            regex: Regex::new(
                r"^(?P<agent>[^_]+)_(?P<contact>[^_]+)_(?P<ymd>\d{8})[_-](?P<hms>\d{6})",
            )
            .expect("builtin pattern compiles"),
            on_source_key: false,
            date_format: DateFormat::YyyyMmDd,
        },
    ]
}

/// Parse user-supplied patterns.
///
/// A bad regex names the pattern it came from: that name is the only clue the
/// user gets about which of their rules is the broken one.
pub fn patterns_from_json(json: &str) -> Result<Vec<Pattern>> {
    let specs: Vec<PatternSpec> = serde_json::from_str(json)
        .map_err(|e| CaError::Config(format!("attribution patterns: {e}")))?;
    specs
        .into_iter()
        .map(|s| {
            let regex = Regex::new(&s.regex)
                .map_err(|e| CaError::Config(format!("attribution pattern {}: {e}", s.name)))?;
            Ok(Pattern {
                name: s.name,
                regex,
                on_source_key: s.on_source_key,
                date_format: s.date_format,
            })
        })
        .collect()
}
