# Phase 4a — Participant Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every call a "who handled it" and a "who called", plus the real call time, so per-person scorecards, relationship history and repeat-contact rate become computable.

**Architecture:** A new pure crate `ca-identity` turns a filename into an `Attribution` with no I/O, shaped exactly like `ca-rules` from Phase 3. `ca-store` gains a `people`/`teams` schema plus four columns on `calls`, and an idempotent backfill that respects manual overrides. `ca-insights` rolls calls up per person. The CLI and desktop both drive the same backfill.

**Tech Stack:** Rust 2021 (workspace `rust-version = 1.82`), `rusqlite` on SQLite with `foreign_keys = ON`, `regex`, `chrono`, Tauri 2, React 19 + TypeScript.

**Spec:** [`docs/superpowers/specs/2026-08-14-phase4a-participant-identity-design.md`](../specs/2026-08-14-phase4a-participant-identity-design.md)

## Global Constraints

- **Naming:** the human side is `Person`/`PersonKind`, never "agent" as a type name — `AgentKind` already means an LLM agent.
- **Provenance:** `attribution` is one of exactly `none | rule | manual | live`. A backfill must never overwrite a row whose value is `manual`.
- **`created_at` is never written by this phase.** Real call time goes in the new `occurred_at` column only.
- **Handles are not E.164.** Digits, preserving a leading `+`; names folded to lowercase with collapsed whitespace.
- **`people` is local-only**, the same class as `redaction_map`. No participant name or handle may enter a provider prompt.
- **Timezone:** filename timestamps are parsed as machine-local and stored RFC3339 UTC.
- **Aggregates are `Option`.** Absent measures stay `None` and render `—`; never `0`.
- **Migrations are append-only.** Never edit a shipped migration; `0006_people` is the next one.
- **Gates:** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --exclude call-analyzer-desktop`, `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`. Set `CARGO_INCREMENTAL=0`. `cargo` is not on PATH for bash: `export PATH="$PATH:/c/Users/yashk/.cargo/bin"`.

---

### Task 1: Core identity types

**Files:**
- Create: `crates/ca-core/src/identity.rs`
- Modify: `crates/ca-core/src/ids.rs` (append two id types)
- Modify: `crates/ca-core/src/lib.rs` (module + re-exports)
- Modify: `crates/ca-core/src/call.rs` (four new fields on `Call`)

**Interfaces:**
- Consumes: nothing.
- Produces: `PersonId`, `TeamId`, `PersonKind::{Agent, Contact}`, `AttributionSource::{None, Rule, Manual, Live}`, `Person`, and `Call::{agent_id, contact_id, occurred_at, attribution}`.

- [ ] **Step 1: Write the failing test**

Append to `crates/ca-core/src/identity.rs` (created in step 3, but write the test first in that file):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn person_kind_round_trips_through_its_string_form() {
        for k in [PersonKind::Agent, PersonKind::Contact] {
            assert_eq!(PersonKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(PersonKind::parse("nonsense"), None);
    }

    #[test]
    fn attribution_source_round_trips_and_defaults_to_none() {
        for s in [
            AttributionSource::None,
            AttributionSource::Rule,
            AttributionSource::Manual,
            AttributionSource::Live,
        ] {
            assert_eq!(AttributionSource::parse(s.as_str()), Some(s));
        }
        assert_eq!(AttributionSource::default(), AttributionSource::None);
    }

    /// The backfill decides whether to overwrite by asking this question, so it
    /// is worth pinning: only a hand-made assignment is protected.
    #[test]
    fn only_manual_attribution_is_protected_from_a_backfill() {
        assert!(AttributionSource::Manual.is_protected());
        assert!(!AttributionSource::None.is_protected());
        assert!(!AttributionSource::Rule.is_protected());
        assert!(!AttributionSource::Live.is_protected());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `export PATH="$PATH:/c/Users/yashk/.cargo/bin" && CARGO_INCREMENTAL=0 cargo test -p ca-core identity`
Expected: FAIL — `identity` module does not exist.

- [ ] **Step 3: Write the implementation**

Create `crates/ca-core/src/identity.rs` with this above the test module:

```rust
//! Who was on the call.
//!
//! Deliberately named `Person` rather than "agent": [`crate::AgentKind`]
//! already means an LLM agent, and the two appear in the same files.

use crate::ids::{PersonId, TeamId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Which side of the call a person is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonKind {
    /// The person handling the call.
    Agent,
    /// The other party.
    Contact,
}

impl PersonKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PersonKind::Agent => "agent",
            PersonKind::Contact => "contact",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "agent" => Some(PersonKind::Agent),
            "contact" => Some(PersonKind::Contact),
            _ => None,
        }
    }
}

/// How a call came to be attributed.
///
/// This is not bookkeeping. Without it a second backfill silently overwrites
/// every correction made by hand, which is the most likely way this feature
/// loses user data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionSource {
    /// Nothing matched, or attribution has not run.
    #[default]
    None,
    /// A filename pattern matched.
    Rule,
    /// A person set it in the UI.
    Manual,
    /// A live session recorded who was at the machine.
    Live,
}

impl AttributionSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            AttributionSource::None => "none",
            AttributionSource::Rule => "rule",
            AttributionSource::Manual => "manual",
            AttributionSource::Live => "live",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(AttributionSource::None),
            "rule" => Some(AttributionSource::Rule),
            "manual" => Some(AttributionSource::Manual),
            "live" => Some(AttributionSource::Live),
            _ => None,
        }
    }

    /// Whether a re-run of the attribution rules must leave this row alone.
    pub fn is_protected(&self) -> bool {
        matches!(self, AttributionSource::Manual)
    }
}

/// Someone who appears on calls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub id: PersonId,
    pub kind: PersonKind,
    /// As it should be shown: the saved contact name, or the number.
    pub display_name: String,
    /// Normalised identity key. Unique per `(kind, handle)`.
    pub handle: String,
    pub team_id: Option<TeamId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A group of agents, for rollups.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Team {
    pub id: TeamId,
    pub name: String,
    pub created_at: DateTime<Utc>,
}
```

Append to `crates/ca-core/src/ids.rs`:

```rust
id_type!(PersonId, "person");
id_type!(TeamId, "team");
```

In `crates/ca-core/src/lib.rs`, add `pub mod identity;` after `pub mod ids;` and extend the re-exports:

```rust
pub use identity::{AttributionSource, Person, PersonKind, Team};
pub use ids::{AnalysisId, CallId, FindingId, JobId, PersonId, SegmentId, TeamId};
```

In `crates/ca-core/src/call.rs`, add to `struct Call` after `pub features: Option<AcousticFeatures>,`:

```rust
    /// Who handled the call, once attributed.
    pub agent_id: Option<crate::ids::PersonId>,
    /// The other party, once attributed.
    pub contact_id: Option<crate::ids::PersonId>,
    /// When the call actually happened, as opposed to when it was ingested.
    /// `created_at` remains ingest time and is never overwritten.
    pub occurred_at: Option<DateTime<Utc>>,
    pub attribution: crate::identity::AttributionSource,
```

and initialise them in `Call::new`:

```rust
            agent_id: None,
            contact_id: None,
            occurred_at: None,
            attribution: crate::identity::AttributionSource::None,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `CARGO_INCREMENTAL=0 cargo test -p ca-core`
Expected: PASS. Fix any construction sites of `Call { .. }` elsewhere that the compiler flags.

- [ ] **Step 5: Commit**

```bash
git add crates/ca-core
git commit -m "feat(core): add person identity and attribution provenance"
```

---

### Task 2: `ca-identity` — attribution from a filename

**Files:**
- Create: `crates/ca-identity/Cargo.toml`
- Create: `crates/ca-identity/src/lib.rs`
- Create: `crates/ca-identity/src/patterns.rs`
- Modify: `Cargo.toml` (workspace members + workspace dependency)

**Interfaces:**
- Consumes: `ca_core::identity::PersonKind`.
- Produces: `attribute(display_name, source_key, patterns) -> Attribution`, `builtin_patterns() -> Vec<Pattern>`, `patterns_from_json(&str) -> Result<Vec<Pattern>>`, `normalise_handle(&str) -> String`, and the types `Attribution`, `Handle`, `Pattern`, `DateFormat`.

- [ ] **Step 1: Write the failing tests**

Create `crates/ca-identity/src/lib.rs` with only the test module for now:

```rust
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
        let c = a.contact.expect("contact");
        assert_eq!(c.kind, PersonKind::Contact);
        assert_eq!(c.display_name, "+918485823280");
        assert_eq!(c.handle, "+918485823280");
        assert_eq!(a.rule.as_deref(), Some("android_call_recorder"));
        // The phone names the counterparty, never the handler.
        assert!(a.agent.is_none());
        assert!(a.occurred_at.is_some());
    }

    #[test]
    fn a_saved_contact_name_survives_underscores_and_emoji() {
        let a = attr("Call recording Ishita AI Cloud Koradia_251007_101533.m4a");
        assert_eq!(a.contact.unwrap().display_name, "Ishita AI Cloud Koradia");

        let a = attr("Call recording ⛔ Spam Alert From Jio_250903_090102.m4a");
        assert_eq!(a.contact.unwrap().display_name, "⛔ Spam Alert From Jio");
    }

    /// The whole point of parsing: `created_at` is ingest time and useless as
    /// an axis. A two-digit year is read as 20xx.
    #[test]
    fn the_timestamp_is_the_call_time_not_the_ingest_time() {
        let a = attr("Call recording +911409098039_251107_123936.m4a");
        let when = a.occurred_at.expect("timestamp");
        let local = when.with_timezone(&chrono::Local);
        assert_eq!(local.format("%Y-%m-%d %H:%M:%S").to_string(), "2025-11-07 12:39:36");
    }

    #[test]
    fn a_contact_centre_export_names_both_sides() {
        let a = attribute("priya.sharma_+919876543210_20250114_093000.wav", "", &builtin_patterns());
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
        assert_ne!(normalise_handle("08618075571"), normalise_handle("+918618075571"));
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
        let a = attribute("whatever.wav", "C:/calls/priya.sharma/whatever.wav", &patterns);
        assert_eq!(a.agent.unwrap().display_name, "priya.sharma");
    }

    #[test]
    fn a_malformed_pattern_is_rejected_with_a_reason() {
        let err = patterns_from_json(r#"[{"name":"bad","regex":"(unclosed","on_source_key":false,"date_format":"none"}]"#)
            .expect_err("must reject");
        assert!(format!("{err}").contains("bad"), "the error names the pattern: {err}");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `CARGO_INCREMENTAL=0 cargo test -p ca-identity`
Expected: FAIL — the package does not exist yet.

- [ ] **Step 3: Write the implementation**

Create `crates/ca-identity/Cargo.toml`:

```toml
[package]
name = "ca-identity"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
ca-core.workspace = true
chrono.workspace = true
regex.workspace = true
serde.workspace = true
serde_json.workspace = true
```

In the workspace `Cargo.toml`, add `"crates/ca-identity",` to `members` after `"crates/ca-rules",` and `ca-identity = { path = "crates/ca-identity" }` to `[workspace.dependencies]`.

Create `crates/ca-identity/src/patterns.rs`:

```rust
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
/// Captures are read by name: `agent`, `contact`, `ymd`, `hms`. Any of them
/// may be absent — a pattern that names only the contact is normal.
#[derive(Debug, Clone)]
pub struct Pattern {
    pub name: String,
    pub regex: Regex,
    /// Match against the source key (the path) instead of the display name.
    pub on_source_key: bool,
    pub date_format: DateFormat,
}

#[derive(Deserialize)]
struct PatternSpec {
    name: String,
    regex: String,
    #[serde(default)]
    on_source_key: bool,
    #[serde(default = "default_format")]
    date_format: DateFormat,
}

fn default_format() -> DateFormat {
    DateFormat::None
}

/// The patterns that ship in the binary, tried in order.
///
/// The Android recorder comes first because its literal prefix makes it the
/// more specific of the two.
pub fn builtin_patterns() -> Vec<Pattern> {
    vec![
        Pattern {
            name: "android_call_recorder".into(),
            regex: Regex::new(r"^Call recording (?P<contact>.+?)_(?P<ymd>\d{6})_(?P<hms>\d{6})\.\w+$")
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
/// A bad regex names the pattern it came from: the message is the only clue
/// the user gets about which of their rules is wrong.
pub fn patterns_from_json(json: &str) -> Result<Vec<Pattern>> {
    let specs: Vec<PatternSpec> = serde_json::from_str(json)
        .map_err(|e| CaError::Config(format!("attribution patterns: {e}")))?;
    specs
        .into_iter()
        .map(|s| {
            let regex = Regex::new(&s.regex).map_err(|e| {
                CaError::Config(format!("attribution pattern {}: {e}", s.name))
            })?;
            Ok(Pattern {
                name: s.name,
                regex,
                on_source_key: s.on_source_key,
                date_format: s.date_format,
            })
        })
        .collect()
}
```

Now prepend the implementation to `crates/ca-identity/src/lib.rs`, above the test module:

```rust
//! Who was on a call, derived from what the recorder named the file.
//!
//! The core is a pure function over two strings, so the whole crate tests
//! without a database, a filesystem or a clock — the same shape as
//! `ca-rules`.

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
    /// that produced it.
    pub rule: Option<String>,
}

impl Attribution {
    /// Whether anything at all was learned.
    pub fn is_empty(&self) -> bool {
        self.agent.is_none() && self.contact.is_none() && self.occurred_at.is_none()
    }
}

/// Apply patterns in order; the first match wins.
pub fn attribute(display_name: &str, source_key: &str, patterns: &[Pattern]) -> Attribution {
    for p in patterns {
        let hay = if p.on_source_key { source_key } else { display_name };
        let Some(caps) = p.regex.captures(hay) else {
            continue;
        };
        let a = Attribution {
            agent: caps
                .name("agent")
                .map(|m| Handle::new(PersonKind::Agent, m.as_str())),
            contact: caps
                .name("contact")
                .map(|m| Handle::new(PersonKind::Contact, m.as_str())),
            occurred_at: parse_when(&caps, p.date_format),
            rule: Some(p.name.clone()),
        };
        if a.is_empty() {
            continue;
        }
        return a;
    }
    Attribution::default()
}

/// Fold a raw participant string into a stable identity key.
///
/// Numbers reduce to their digits with a leading `+` preserved; anything else
/// is lowercased with runs of whitespace collapsed. Deliberately **not**
/// E.164: inferring a country code for a bare local number would merge two
/// unrelated people under one identity.
pub fn normalise_handle(raw: &str) -> String {
    let trimmed = raw.trim();
    let is_number = !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_digit() || " +-()".contains(c))
        && trimmed.chars().any(|c| c.is_ascii_digit());

    if is_number {
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
/// the result stored as UTC. A recording moved between zones is off by the
/// offset; that is documented rather than hidden.
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
    // `earliest` rather than `single`: a DST fold must not discard the call.
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
}
```

Add `use ca_core::identity::PersonKind;` is already present; the test module also needs `chrono` in scope, which it gets via the full path `chrono::Local`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `CARGO_INCREMENTAL=0 cargo test -p ca-identity`
Expected: PASS, 10 tests.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/ca-identity
git commit -m "feat(identity): derive participants and call time from a filename"
```

---

### Task 3: Schema and people persistence

**Files:**
- Modify: `crates/ca-store/src/schema.rs` (append migration `0006_people`)
- Create: `crates/ca-store/src/people.rs`
- Modify: `crates/ca-store/src/lib.rs` (`pub mod people;`, re-exports, and `upsert_call` writes the new columns)

**Interfaces:**
- Consumes: `ca_core::identity::{Person, PersonKind, AttributionSource}`, `ca_core::ids::{PersonId, TeamId}`.
- Produces on `Store`: `upsert_person(kind, display_name, handle) -> Result<PersonId>`, `person(&PersonId) -> Result<Option<Person>>`, `list_people(kind: Option<PersonKind>, search: Option<&str>) -> Result<Vec<PersonSummary>>`, `set_call_participants(&CallId, Option<&PersonId>, Option<&PersonId>, Option<DateTime<Utc>>, AttributionSource) -> Result<()>`, `call_attribution(&CallId) -> Result<AttributionSource>`.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `crates/ca-store/src/lib.rs`:

```rust
    #[test]
    fn a_person_is_created_once_per_handle_and_kind() {
        let s = Store::in_memory().unwrap();
        let a = s.upsert_person(PersonKind::Contact, "Babulal Kavaiya", "babulal kavaiya").unwrap();
        let b = s.upsert_person(PersonKind::Contact, "BABULAL KAVAIYA", "babulal kavaiya").unwrap();
        assert_eq!(a, b, "same handle and kind is the same person");

        // The same handle on the other side of the call is a different person.
        let c = s.upsert_person(PersonKind::Agent, "Babulal Kavaiya", "babulal kavaiya").unwrap();
        assert_ne!(a, c);
    }

    #[test]
    fn attributing_a_call_stores_both_sides_the_time_and_the_provenance() {
        let s = Store::in_memory().unwrap();
        let id = s.upsert_call(&call("k1")).unwrap();
        let contact = s.upsert_person(PersonKind::Contact, "+91848", "+91848").unwrap();
        let when = chrono::Utc::now();

        s.set_call_participants(&id, None, Some(&contact), Some(when), AttributionSource::Rule)
            .unwrap();

        assert_eq!(s.call_attribution(&id).unwrap(), AttributionSource::Rule);
        let listed = s.list_people(Some(PersonKind::Contact), None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].calls, 1);
    }

    /// The recording is the record: removing someone from the roster must not
    /// delete their calls.
    #[test]
    fn deleting_a_person_leaves_the_call_in_place() {
        let s = Store::in_memory().unwrap();
        let id = s.upsert_call(&call("k2")).unwrap();
        let p = s.upsert_person(PersonKind::Agent, "Priya", "priya").unwrap();
        s.set_call_participants(&id, Some(&p), None, None, AttributionSource::Manual)
            .unwrap();

        s.lock()
            .unwrap()
            .execute("DELETE FROM people WHERE id = ?1", [p.as_str()])
            .unwrap();

        assert_eq!(s.count_calls().unwrap(), 1);
        assert_eq!(s.call_attribution(&id).unwrap(), AttributionSource::Manual);
    }
```

Add `use ca_core::identity::{AttributionSource, PersonKind};` to the test module's imports if not already reachable via `super::*`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `CARGO_INCREMENTAL=0 cargo test -p ca-store people`
Expected: FAIL — `upsert_person` not found.

- [ ] **Step 3: Write the implementation**

Append to `MIGRATIONS` in `crates/ca-store/src/schema.rs`, after `0005_channel_timeline`:

```rust
    (
        "0006_people",
        r#"
-- Who was on the call.
--
-- The corpus was keyed by file path, so it could say what a call scored but
-- never whose call it was. Every per-agent rollup and every repeat-contact
-- question needs this table.
--
-- Local-only, the same class of data as `redaction_map`: these are the names
-- and numbers the transcript pipeline redacts before a provider sees a call.
CREATE TABLE teams (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE UNIQUE INDEX idx_teams_name ON teams(name);

CREATE TABLE people (
    id           TEXT PRIMARY KEY,
    -- agent | contact
    kind         TEXT NOT NULL,
    display_name TEXT NOT NULL,
    -- Normalised identity key: digits for numbers, folded text for names.
    handle       TEXT NOT NULL,
    team_id      TEXT REFERENCES teams(id) ON DELETE SET NULL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

-- The same number may be an agent on one call and a contact on another.
CREATE UNIQUE INDEX idx_people_handle ON people(kind, handle);
CREATE INDEX idx_people_team ON people(team_id);

-- SET NULL, not CASCADE: removing someone from the roster must not erase
-- their calls. The corpus is evidence.
ALTER TABLE calls ADD COLUMN agent_id   TEXT REFERENCES people(id) ON DELETE SET NULL;
ALTER TABLE calls ADD COLUMN contact_id TEXT REFERENCES people(id) ON DELETE SET NULL;

-- When the call happened, as opposed to when it was ingested. `created_at`
-- keeps its meaning: 1,967 of the 1,972 calls in the reference corpus share
-- one ingest date, which is why a separate column exists at all.
ALTER TABLE calls ADD COLUMN occurred_at TEXT;

-- none | rule | manual | live. Load-bearing: a backfill skips `manual`, so a
-- correction made by hand survives the next run.
ALTER TABLE calls ADD COLUMN attribution TEXT NOT NULL DEFAULT 'none';

CREATE INDEX idx_calls_agent    ON calls(agent_id, occurred_at);
CREATE INDEX idx_calls_contact  ON calls(contact_id, occurred_at);
CREATE INDEX idx_calls_occurred ON calls(occurred_at DESC);
"#,
    ),
```

Create `crates/ca-store/src/people.rs`:

```rust
//! The people on either side of a call.

use ca_core::error::{CaError, Result};
use ca_core::identity::{AttributionSource, Person, PersonKind};
use ca_core::ids::{CallId, PersonId, TeamId};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

use crate::Store;

/// A person plus the counts the list view needs, so the screen is one query.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonSummary {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub handle: String,
    pub calls: usize,
    /// First and last call by `occurred_at`, falling back to nothing when the
    /// call was never attributed a time.
    pub first_call: Option<String>,
    pub last_call: Option<String>,
}

impl Store {
    /// Find or create a person. Identity is `(kind, handle)`.
    pub fn upsert_person(
        &self,
        kind: PersonKind,
        display_name: &str,
        handle: &str,
    ) -> Result<PersonId> {
        let conn = self.lock()?;
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM people WHERE kind = ?1 AND handle = ?2",
                params![kind.as_str(), handle],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| CaError::Storage(format!("lookup person: {e}")))?;

        if let Some(id) = existing {
            return Ok(PersonId::from(id));
        }

        let id = PersonId::new();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO people (id, kind, display_name, handle, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?5)",
            params![id.as_str(), kind.as_str(), display_name, handle, now],
        )
        .map_err(|e| CaError::Storage(format!("insert person: {e}")))?;
        Ok(id)
    }

    pub fn person(&self, id: &PersonId) -> Result<Option<Person>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, kind, display_name, handle, team_id, created_at, updated_at
               FROM people WHERE id = ?1",
            [id.as_str()],
            |r| {
                let kind: String = r.get(1)?;
                let team: Option<String> = r.get(4)?;
                let created: String = r.get(5)?;
                let updated: String = r.get(6)?;
                Ok(Person {
                    id: PersonId::from(r.get::<_, String>(0)?),
                    kind: PersonKind::parse(&kind).unwrap_or(PersonKind::Contact),
                    display_name: r.get(2)?,
                    handle: r.get(3)?,
                    team_id: team.map(TeamId::from),
                    created_at: parse_time(&created),
                    updated_at: parse_time(&updated),
                })
            },
        )
        .optional()
        .map_err(|e| CaError::Storage(format!("load person: {e}")))
    }

    /// People, with their call counts, newest activity first.
    pub fn list_people(
        &self,
        kind: Option<PersonKind>,
        search: Option<&str>,
    ) -> Result<Vec<PersonSummary>> {
        let conn = self.lock()?;
        let kind_filter = kind.map(|k| k.as_str().to_string()).unwrap_or_default();
        let like = format!("%{}%", search.unwrap_or(""));

        let mut stmt = conn
            .prepare(
                "SELECT p.id, p.kind, p.display_name, p.handle,
                        (SELECT count(*) FROM calls c
                          WHERE c.agent_id = p.id OR c.contact_id = p.id),
                        (SELECT min(c.occurred_at) FROM calls c
                          WHERE c.agent_id = p.id OR c.contact_id = p.id),
                        (SELECT max(c.occurred_at) FROM calls c
                          WHERE c.agent_id = p.id OR c.contact_id = p.id)
                   FROM people p
                  WHERE (?1 = '' OR p.kind = ?1)
                    AND (?2 = '%%' OR p.display_name LIKE ?2 OR p.handle LIKE ?2)
                  ORDER BY 5 DESC, p.display_name ASC",
            )
            .map_err(|e| CaError::Storage(format!("prepare people: {e}")))?;

        let rows = stmt
            .query_map(params![kind_filter, like], |r| {
                Ok(PersonSummary {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    display_name: r.get(2)?,
                    handle: r.get(3)?,
                    calls: r.get::<_, i64>(4)?.max(0) as usize,
                    first_call: r.get(5)?,
                    last_call: r.get(6)?,
                })
            })
            .map_err(|e| CaError::Storage(format!("query people: {e}")))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| CaError::Storage(format!("read people: {e}")))
    }

    /// Attach participants and the real call time to a call.
    pub fn set_call_participants(
        &self,
        call_id: &CallId,
        agent: Option<&PersonId>,
        contact: Option<&PersonId>,
        occurred_at: Option<DateTime<Utc>>,
        source: AttributionSource,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE calls
                SET agent_id = ?2, contact_id = ?3, occurred_at = ?4,
                    attribution = ?5, updated_at = ?6
              WHERE id = ?1",
            params![
                call_id.as_str(),
                agent.map(|p| p.as_str()),
                contact.map(|p| p.as_str()),
                occurred_at.map(|t| t.to_rfc3339()),
                source.as_str(),
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| CaError::Storage(format!("set participants: {e}")))?;
        Ok(())
    }

    pub fn call_attribution(&self, call_id: &CallId) -> Result<AttributionSource> {
        let conn = self.lock()?;
        let s: Option<String> = conn
            .query_row(
                "SELECT attribution FROM calls WHERE id = ?1",
                [call_id.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| CaError::Storage(format!("call attribution: {e}")))?;
        Ok(s.and_then(|s| AttributionSource::parse(&s)).unwrap_or_default())
    }
}

fn parse_time(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
```

In `crates/ca-store/src/lib.rs` add `pub mod people;` to the module list and `pub use people::PersonSummary;` to the re-exports.

- [ ] **Step 4: Run tests to verify they pass**

Run: `CARGO_INCREMENTAL=0 cargo test -p ca-store`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ca-store
git commit -m "feat(store): add people, teams and call attribution columns"
```

---

### Task 4: The backfill

**Files:**
- Create: `crates/ca-store/src/attribute.rs`
- Modify: `crates/ca-store/src/lib.rs` (module + re-export)
- Modify: `crates/ca-store/Cargo.toml` (add `ca-identity`)

**Interfaces:**
- Consumes: `ca_identity::{attribute, builtin_patterns, Pattern}`, `Store::{upsert_person, set_call_participants, call_attribution}`.
- Produces: `Store::attribute_calls(&[Pattern], AttributeOptions) -> Result<AttributeReport>` and the types `AttributeOptions { dry_run, force }`, `AttributeReport { scanned, attributed, skipped_manual, unmatched }`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/ca-store/src/lib.rs`:

```rust
    fn named_call(name: &str) -> Call {
        Call::new(format!("C:/calls/{name}"), "local", name)
    }

    #[test]
    fn the_backfill_attributes_recognisable_names_and_leaves_the_rest_alone() {
        let s = Store::in_memory().unwrap();
        s.upsert_call(&named_call("Call recording +918485823280_251112_142326.m4a")).unwrap();
        s.upsert_call(&named_call("Call recording +918485823280_251114_204226.m4a")).unwrap();
        s.upsert_call(&named_call("holiday video.mp4")).unwrap();

        let r = s
            .attribute_calls(&ca_identity::builtin_patterns(), Default::default())
            .unwrap();

        assert_eq!(r.scanned, 3);
        assert_eq!(r.attributed, 2);
        assert_eq!(r.unmatched, 1);
        // Two calls, one contact.
        assert_eq!(s.list_people(Some(PersonKind::Contact), None).unwrap().len(), 1);
        assert_eq!(s.list_people(None, None).unwrap()[0].calls, 2);
    }

    #[test]
    fn the_backfill_is_idempotent() {
        let s = Store::in_memory().unwrap();
        s.upsert_call(&named_call("Call recording +918485823280_251112_142326.m4a")).unwrap();

        let first = s.attribute_calls(&ca_identity::builtin_patterns(), Default::default()).unwrap();
        let second = s.attribute_calls(&ca_identity::builtin_patterns(), Default::default()).unwrap();

        assert_eq!(first.attributed, 1);
        assert_eq!(second.attributed, 1, "re-running re-derives the same answer");
        assert_eq!(s.list_people(None, None).unwrap().len(), 1, "no duplicate people");
    }

    /// The reason `attribution` exists at all.
    #[test]
    fn a_manual_assignment_survives_a_later_backfill() {
        let s = Store::in_memory().unwrap();
        let id = s.upsert_call(&named_call("Call recording +918485823280_251112_142326.m4a")).unwrap();
        let chosen = s.upsert_person(PersonKind::Contact, "Real Name", "real name").unwrap();
        s.set_call_participants(&id, None, Some(&chosen), None, AttributionSource::Manual).unwrap();

        let r = s.attribute_calls(&ca_identity::builtin_patterns(), Default::default()).unwrap();

        assert_eq!(r.skipped_manual, 1);
        assert_eq!(r.attributed, 0);
        assert_eq!(s.call_attribution(&id).unwrap(), AttributionSource::Manual);
    }

    #[test]
    fn force_overrides_a_manual_assignment_and_dry_run_changes_nothing() {
        let s = Store::in_memory().unwrap();
        let id = s.upsert_call(&named_call("Call recording +918485823280_251112_142326.m4a")).unwrap();
        s.set_call_participants(&id, None, None, None, AttributionSource::Manual).unwrap();

        let dry = s
            .attribute_calls(
                &ca_identity::builtin_patterns(),
                AttributeOptions { dry_run: true, force: true },
            )
            .unwrap();
        assert_eq!(dry.attributed, 1);
        assert_eq!(s.call_attribution(&id).unwrap(), AttributionSource::Manual, "dry run wrote nothing");

        s.attribute_calls(
            &ca_identity::builtin_patterns(),
            AttributeOptions { dry_run: false, force: true },
        )
        .unwrap();
        assert_eq!(s.call_attribution(&id).unwrap(), AttributionSource::Rule);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `CARGO_INCREMENTAL=0 cargo test -p ca-store attribute`
Expected: FAIL — `attribute_calls` not found.

- [ ] **Step 3: Write the implementation**

Add to `crates/ca-store/Cargo.toml` under `[dependencies]`: `ca-identity.workspace = true`.

Create `crates/ca-store/src/attribute.rs`:

```rust
//! Deriving participants for calls that are already in the corpus.

use ca_core::error::{CaError, Result};
use ca_core::identity::AttributionSource;
use ca_core::ids::CallId;
use ca_identity::{attribute, Pattern};

use crate::Store;

/// How a backfill run should behave.
#[derive(Debug, Clone, Copy, Default)]
pub struct AttributeOptions {
    /// Report what would change without writing anything.
    pub dry_run: bool,
    /// Overwrite hand-made assignments too. Off by default, deliberately.
    pub force: bool,
}

/// What a backfill run did.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributeReport {
    pub scanned: usize,
    pub attributed: usize,
    pub skipped_manual: usize,
    /// Matched no pattern. These stay fully usable; they just do not roll up.
    pub unmatched: usize,
}

impl Store {
    /// Re-derive participants for every call from its filename.
    ///
    /// Idempotent: identity is `(kind, handle)`, so a second run finds the same
    /// people rather than creating new ones. Rows attributed by hand are left
    /// alone unless `force` is set — without that rule the second run would
    /// silently discard every correction the user made.
    pub fn attribute_calls(
        &self,
        patterns: &[Pattern],
        opts: AttributeOptions,
    ) -> Result<AttributeReport> {
        let rows: Vec<(String, String, String, String)> = {
            let conn = self.lock()?;
            let mut stmt = conn
                .prepare("SELECT id, display_name, source_key, attribution FROM calls")
                .map_err(|e| CaError::Storage(format!("prepare attribute: {e}")))?;
            let mapped = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(|e| CaError::Storage(format!("query attribute: {e}")))?;
            mapped
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| CaError::Storage(format!("read attribute: {e}")))?
        };

        let mut report = AttributeReport::default();
        for (id, display_name, source_key, current) in rows {
            report.scanned += 1;
            let current = AttributionSource::parse(&current).unwrap_or_default();
            if current.is_protected() && !opts.force {
                report.skipped_manual += 1;
                continue;
            }

            let found = attribute(&display_name, &source_key, patterns);
            if found.is_empty() {
                report.unmatched += 1;
                continue;
            }
            report.attributed += 1;
            if opts.dry_run {
                continue;
            }

            let agent = found
                .agent
                .map(|h| self.upsert_person(h.kind, &h.display_name, &h.handle))
                .transpose()?;
            let contact = found
                .contact
                .map(|h| self.upsert_person(h.kind, &h.display_name, &h.handle))
                .transpose()?;

            self.set_call_participants(
                &CallId::from(id),
                agent.as_ref(),
                contact.as_ref(),
                found.occurred_at,
                AttributionSource::Rule,
            )?;
        }
        Ok(report)
    }
}
```

In `crates/ca-store/src/lib.rs` add `pub mod attribute;` and `pub use attribute::{AttributeOptions, AttributeReport};`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `CARGO_INCREMENTAL=0 cargo test -p ca-store`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ca-store Cargo.toml
git commit -m "feat(store): backfill call participants without clobbering manual edits"
```

---

### Task 5: Per-person rollups

**Files:**
- Create: `crates/ca-insights/src/person.rs` (the arithmetic)
- Modify: `crates/ca-store/src/people.rs` (the query that feeds it)
- Modify: `crates/ca-insights/src/lib.rs` (module + re-exports, `CallFacts` gains two fields)
- Modify: `crates/ca-store/src/corpus.rs` (populate the two new `CallFacts` fields)

**Interfaces:**
- Consumes: `CallFacts` from `ca-insights`, `Store::corpus_facts`.
- Produces: `PersonFacts`, `person_facts(&[CallFacts], person_id) -> PersonFacts`, `repeat_contact_rate(&[CallRef], window_days: i64) -> Option<f32>`, `monthly_trend(&[CallFacts], measure) -> Vec<MonthPoint>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/ca-insights/src/person.rs` with the tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap()
    }

    fn call(person: &str, when: DateTime<Utc>, qa: Option<f32>) -> CallRef {
        CallRef { call_id: format!("c{}", when.timestamp()), contact_id: Some(person.into()), occurred_at: Some(when), qa_overall: qa, sentiment: None, compliance_passed: None }
    }

    /// The failure this guards is a fabricated number: with nothing analysed,
    /// a mean of 0 looks exactly like a genuinely terrible agent.
    #[test]
    fn a_person_with_no_analysed_calls_has_no_scores_rather_than_zeroes() {
        let calls = vec![call("p1", at(2025, 1, 5), None), call("p1", at(2025, 2, 5), None)];
        let f = person_facts("p1", &calls);
        assert_eq!(f.calls, 2, "the calls themselves are real and counted");
        assert_eq!(f.mean_qa, None);
        assert_eq!(f.mean_sentiment, None);
        assert_eq!(f.compliance_pass_rate, None);
    }

    #[test]
    fn means_ignore_unanalysed_calls_instead_of_counting_them_as_zero() {
        let calls = vec![
            call("p1", at(2025, 1, 5), Some(80.0)),
            call("p1", at(2025, 1, 6), None),
            call("p1", at(2025, 1, 7), Some(60.0)),
        ];
        let f = person_facts("p1", &calls);
        assert_eq!(f.calls, 3);
        assert_eq!(f.mean_qa, Some(70.0));
    }

    #[test]
    fn first_and_last_call_come_from_the_real_call_time() {
        let calls = vec![call("p1", at(2025, 3, 9), None), call("p1", at(2025, 1, 2), None)];
        let f = person_facts("p1", &calls);
        assert_eq!(f.first_call, Some(at(2025, 1, 2)));
        assert_eq!(f.last_call, Some(at(2025, 3, 9)));
    }

    #[test]
    fn a_contact_calling_twice_inside_the_window_counts_as_a_repeat() {
        let calls = vec![call("p1", at(2025, 5, 1), None), call("p1", at(2025, 5, 10), None)];
        assert_eq!(repeat_contact_rate(&calls, 30), Some(0.5));
    }

    /// Without a window "repeat" drifts upward forever and reads as a
    /// worsening trend when nothing has changed.
    #[test]
    fn a_call_outside_the_window_is_not_a_repeat() {
        let calls = vec![call("p1", at(2025, 1, 1), None), call("p1", at(2025, 6, 1), None)];
        assert_eq!(repeat_contact_rate(&calls, 30), Some(0.0));
    }

    #[test]
    fn calls_without_a_time_cannot_answer_the_repeat_question() {
        let mut c = call("p1", at(2025, 1, 1), None);
        c.occurred_at = None;
        assert_eq!(repeat_contact_rate(&[c], 30), None);
    }

    #[test]
    fn the_trend_buckets_by_month_and_skips_months_with_no_measure() {
        let calls = vec![
            call("p1", at(2025, 1, 5), Some(50.0)),
            call("p1", at(2025, 1, 20), Some(70.0)),
            call("p1", at(2025, 2, 5), None),
            call("p1", at(2025, 3, 5), Some(90.0)),
        ];
        let t = monthly_trend(&calls, |c| c.qa_overall);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0], MonthPoint { month: "2025-01".into(), value: 60.0, calls: 2 });
        assert_eq!(t[1], MonthPoint { month: "2025-03".into(), value: 90.0, calls: 1 });
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `CARGO_INCREMENTAL=0 cargo test -p ca-insights person`
Expected: FAIL — module not declared.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/ca-insights/src/person.rs`:

```rust
//! Rolling calls up per person.
//!
//! Every aggregate is an `Option`. With almost nothing analysed, a mean
//! rendered as `0` is a fabricated number wearing the same clothes as a real
//! one — absent has to stay visibly absent.

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};

/// The minimum a call needs to expose for a person rollup.
#[derive(Debug, Clone, PartialEq)]
pub struct CallRef {
    pub call_id: String,
    pub contact_id: Option<String>,
    pub occurred_at: Option<DateTime<Utc>>,
    pub qa_overall: Option<f32>,
    pub sentiment: Option<f32>,
    pub compliance_passed: Option<bool>,
}

/// What one person's calls add up to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonFacts {
    pub person_id: String,
    pub calls: usize,
    pub mean_qa: Option<f32>,
    pub mean_sentiment: Option<f32>,
    pub compliance_pass_rate: Option<f32>,
    pub first_call: Option<DateTime<Utc>>,
    pub last_call: Option<DateTime<Utc>>,
}

/// One month of a trend line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonthPoint {
    /// `YYYY-MM`.
    pub month: String,
    pub value: f32,
    pub calls: usize,
}

fn mean(values: impl Iterator<Item = f32>) -> Option<f32> {
    let (sum, n) = values.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / n as f32)
}

pub fn person_facts(person_id: &str, calls: &[CallRef]) -> PersonFacts {
    PersonFacts {
        person_id: person_id.to_string(),
        calls: calls.len(),
        mean_qa: mean(calls.iter().filter_map(|c| c.qa_overall)),
        mean_sentiment: mean(calls.iter().filter_map(|c| c.sentiment)),
        compliance_pass_rate: mean(
            calls
                .iter()
                .filter_map(|c| c.compliance_passed)
                .map(|p| if p { 1.0 } else { 0.0 }),
        ),
        first_call: calls.iter().filter_map(|c| c.occurred_at).min(),
        last_call: calls.iter().filter_map(|c| c.occurred_at).max(),
    }
}

/// Share of calls whose contact also called within the preceding `window_days`.
///
/// Both bounds are explicit on purpose. A lifetime definition of "repeat"
/// climbs forever and looks like a worsening trend when nothing has changed.
/// Returns `None` when no call carries a time, because the question is then
/// unanswerable rather than zero.
pub fn repeat_contact_rate(calls: &[CallRef], window_days: i64) -> Option<f32> {
    let mut timed: Vec<(&str, DateTime<Utc>)> = calls
        .iter()
        .filter_map(|c| Some((c.contact_id.as_deref()?, c.occurred_at?)))
        .collect();
    if timed.is_empty() {
        return None;
    }
    timed.sort_by_key(|(_, t)| *t);

    let window = chrono::Duration::days(window_days);
    let mut repeats = 0usize;
    for (i, (who, when)) in timed.iter().enumerate() {
        if timed[..i]
            .iter()
            .any(|(prev, prev_t)| prev == who && *when - *prev_t <= window)
        {
            repeats += 1;
        }
    }
    Some(repeats as f32 / timed.len() as f32)
}

/// Monthly means of one measure. Months where no call carried the measure are
/// omitted rather than plotted as zero.
pub fn monthly_trend(calls: &[CallRef], measure: fn(&CallRef) -> Option<f32>) -> Vec<MonthPoint> {
    let mut buckets: std::collections::BTreeMap<String, Vec<f32>> = Default::default();
    for c in calls {
        let (Some(when), Some(v)) = (c.occurred_at, measure(c)) else {
            continue;
        };
        buckets
            .entry(format!("{:04}-{:02}", when.year(), when.month()))
            .or_default()
            .push(v);
    }
    buckets
        .into_iter()
        .filter_map(|(month, vs)| {
            let calls = vs.len();
            mean(vs.into_iter()).map(|value| MonthPoint { month, value, calls })
        })
        .collect()
}
```

In `crates/ca-insights/src/lib.rs`, add `pub mod person;` and `pub use person::{monthly_trend, person_facts, repeat_contact_rate, CallRef, MonthPoint, PersonFacts};`. Add `agent_id: Option<String>` and `contact_id: Option<String>` to `CallFacts`, and populate both in `crates/ca-store/src/corpus.rs` by selecting `c.agent_id, c.contact_id` in the corpus query.

- [ ] **Step 4: Run tests to verify they pass**

Run: `CARGO_INCREMENTAL=0 cargo test -p ca-insights -p ca-store`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ca-insights crates/ca-store
git commit -m "feat(insights): roll calls up per person with honest absent measures"
```

---

### Task 6: Attribute at ingest and on live sessions

**Files:**
- Modify: `crates/ca-ingest/src/lib.rs` (attribute newly registered calls)
- Modify: `crates/ca-ingest/Cargo.toml` (add `ca-identity`)
- Modify: `crates/ca-live/src/session.rs` (attribute the agent side from settings)

**Interfaces:**
- Consumes: `ca_identity::{attribute, builtin_patterns}`, `Store::{upsert_person, set_call_participants}`.
- Produces: no new public API; newly ingested calls arrive already attributed.

- [ ] **Step 1: Write the failing test**

Add to the ingest crate's tests:

```rust
    #[test]
    fn a_newly_discovered_call_is_attributed_without_a_separate_pass() {
        // Register one file through the normal discovery path, then assert the
        // contact exists. A user should not have to run a backfill to see the
        // people on calls they just ingested.
        let store = Store::in_memory().unwrap();
        let call = Call::new(
            "C:/calls/Call recording +918485823280_251112_142326.m4a",
            "local",
            "Call recording +918485823280_251112_142326.m4a",
        );
        let id = register(&store, &call).unwrap();

        assert_eq!(store.call_attribution(&id).unwrap(), AttributionSource::Rule);
        assert_eq!(store.list_people(Some(PersonKind::Contact), None).unwrap().len(), 1);
    }
```

Adapt `register` to whatever the existing ingest entry point is named — read `crates/ca-ingest/src/lib.rs` and reuse the function that calls `upsert_call`.

- [ ] **Step 2: Run test to verify it fails**

Run: `CARGO_INCREMENTAL=0 cargo test -p ca-ingest`
Expected: FAIL — attribution is `none`.

- [ ] **Step 3: Write the implementation**

Wherever `ca-ingest` calls `store.upsert_call(&call)`, follow it with:

```rust
    // Attribute on the way in, so a fresh ingest never needs a backfill.
    let found = ca_identity::attribute(&call.display_name, &call.source_key, &ca_identity::builtin_patterns());
    if !found.is_empty() {
        let agent = found
            .agent
            .map(|h| store.upsert_person(h.kind, &h.display_name, &h.handle))
            .transpose()?;
        let contact = found
            .contact
            .map(|h| store.upsert_person(h.kind, &h.display_name, &h.handle))
            .transpose()?;
        store.set_call_participants(&id, agent.as_ref(), contact.as_ref(), found.occurred_at, AttributionSource::Rule)?;
    }
```

In `crates/ca-live/src/session.rs`, after the session registers its `Call`, attribute the agent side from the `local_agent_handle` setting when present, with `AttributionSource::Live` and `occurred_at` set to session start. Skip silently when the setting is unset — a live session with no configured agent must still record.

- [ ] **Step 4: Run tests to verify they pass**

Run: `CARGO_INCREMENTAL=0 cargo test -p ca-ingest -p ca-live`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ca-ingest crates/ca-live
git commit -m "feat(ingest,live): attribute calls as they are registered"
```

---

### Task 7: CLI — `ca attribute` and `ca people`

**Files:**
- Modify: `crates/ca-cli/src/main.rs` (two subcommands + handlers)
- Modify: `crates/ca-cli/Cargo.toml` (add `ca-identity`)

**Interfaces:**
- Consumes: `Store::{attribute_calls, list_people}`, `AttributeOptions`.
- Produces: `ca attribute [--dry-run] [--force]`, `ca people [--kind agent|contact] [--search TEXT] [--limit N]`.

- [ ] **Step 1: Add the subcommands**

In `enum Command`, after `Stats`:

```rust
    /// Derive who was on each call from its filename.
    ///
    /// Explicit rather than automatic: this rewrites every row in the corpus,
    /// which is not something to do behind the user's back at startup.
    Attribute {
        /// Report what would change and write nothing.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// Also overwrite assignments made by hand. Off by default.
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// List the people on calls.
    People {
        /// agent or contact. Both if omitted.
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        search: Option<String>,
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },
```

- [ ] **Step 2: Write the handlers**

In the `match cli.command` block:

```rust
        Command::Attribute { dry_run, force } => {
            let report = store.attribute_calls(
                &ca_identity::builtin_patterns(),
                ca_store::AttributeOptions { dry_run, force },
            )?;
            println!(
                "{} scanned, {} attributed, {} skipped (manual), {} unmatched{}",
                report.scanned,
                report.attributed,
                report.skipped_manual,
                report.unmatched,
                if dry_run { "  [dry run, nothing written]" } else { "" },
            );
        }
        Command::People { kind, search, limit } => {
            let kind = kind.as_deref().and_then(ca_core::identity::PersonKind::parse);
            let people = store.list_people(kind, search.as_deref())?;
            println!("{:<28} {:<8} {:>6}  {}", "NAME", "KIND", "CALLS", "LAST CALL");
            for p in people.iter().take(limit) {
                println!(
                    "{:<28} {:<8} {:>6}  {}",
                    truncate(&p.display_name, 28),
                    p.kind,
                    p.calls,
                    p.last_call.as_deref().unwrap_or("—"),
                );
            }
            println!("\n{} people", people.len());
        }
```

Add a small `truncate` helper if the file does not already have one:

```rust
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}
```

- [ ] **Step 3: Verify against the real corpus**

Run:
```bash
export PATH="$PATH:/c/Users/yashk/.cargo/bin"
CARGO_INCREMENTAL=0 cargo run -p ca-cli -- attribute --dry-run
```
Expected: `1972 scanned, 1972 attributed, 0 skipped (manual), 0 unmatched  [dry run, nothing written]`.

Then run it for real and check the people list:
```bash
CARGO_INCREMENTAL=0 cargo run -p ca-cli -- attribute
CARGO_INCREMENTAL=0 cargo run -p ca-cli -- people --kind contact --limit 10
```
Expected: 771 contacts, the busiest with 196 calls.

- [ ] **Step 4: Commit**

```bash
git add crates/ca-cli
git commit -m "feat(cli): add attribute and people commands"
```

---

### Task 8: Desktop commands

**Files:**
- Create: `apps/desktop/src-tauri/src/people.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs` (`mod people;` + register four commands)

**Interfaces:**
- Consumes: `Store::{list_people, person, attribute_calls, set_call_participants, upsert_person}`, `ca_insights::{person_facts, monthly_trend, repeat_contact_rate}`.
- Produces: `list_people(kind, search)`, `get_person(person_id)`, `attribute_calls(dry_run, force)`, `assign_call_person(call_id, agent_id, contact_id)`.

- [ ] **Step 1: Write the commands**

Create `apps/desktop/src-tauri/src/people.rs`:

```rust
//! Participant identity, for the People screen and the call detail assignment.

use ca_core::identity::{AttributionSource, PersonKind};
use ca_core::ids::{CallId, PersonId};
use ca_store::{AttributeOptions, AttributeReport, PersonSummary, Store};
use std::sync::Arc;
use tauri::State;

/// A person plus everything the detail screen draws.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonDetail {
    pub summary: PersonSummary,
    pub facts: ca_insights::PersonFacts,
    pub qa_trend: Vec<ca_insights::MonthPoint>,
    pub sentiment_trend: Vec<ca_insights::MonthPoint>,
    /// `None` when no call carries a time — the question is unanswerable, not
    /// zero.
    pub repeat_contact_rate: Option<f32>,
}

#[tauri::command]
pub fn list_people(
    store: State<'_, Arc<Store>>,
    kind: Option<String>,
    search: Option<String>,
) -> Result<Vec<PersonSummary>, String> {
    let kind = kind.as_deref().and_then(PersonKind::parse);
    store
        .list_people(kind, search.as_deref())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_person(store: State<'_, Arc<Store>>, person_id: String) -> Result<PersonDetail, String> {
    // Implementation: load the summary, pull that person's CallRefs out of the
    // corpus, then call person_facts / monthly_trend / repeat_contact_rate.
    // Return an error string when the person does not exist.
    todo_implement(store, person_id)
}

#[tauri::command]
pub fn attribute_calls(
    store: State<'_, Arc<Store>>,
    dry_run: bool,
    force: bool,
) -> Result<AttributeReport, String> {
    store
        .attribute_calls(
            &ca_identity::builtin_patterns(),
            AttributeOptions { dry_run, force },
        )
        .map_err(|e| e.to_string())
}

/// Assign participants by hand. Sets provenance to `manual`, which is what
/// protects the choice from the next backfill.
#[tauri::command]
pub fn assign_call_person(
    store: State<'_, Arc<Store>>,
    call_id: String,
    agent_id: Option<String>,
    contact_id: Option<String>,
) -> Result<(), String> {
    let call = CallId::from(call_id);
    store
        .set_call_participants(
            &call,
            agent_id.map(PersonId::from).as_ref(),
            contact_id.map(PersonId::from).as_ref(),
            None,
            AttributionSource::Manual,
        )
        .map_err(|e| e.to_string())
}
```

Replace `todo_implement` with the real body — it is spelled out here rather than left vague:

```rust
    let id = PersonId::from(person_id);
    let summary = store
        .list_people(None, None)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|p| p.id == id.as_str())
        .ok_or_else(|| format!("no such person: {}", id.as_str()))?;

    let refs = store.person_call_refs(&id).map_err(|e| e.to_string())?;
    Ok(PersonDetail {
        facts: ca_insights::person_facts(id.as_str(), &refs),
        qa_trend: ca_insights::monthly_trend(&refs, |c| c.qa_overall),
        sentiment_trend: ca_insights::monthly_trend(&refs, |c| c.sentiment),
        repeat_contact_rate: ca_insights::repeat_contact_rate(&refs, 30),
        summary,
    })
```

This needs one more store method — add it to `crates/ca-store/src/people.rs`:

```rust
    /// Every call this person was on, in the shape the rollups take.
    pub fn person_call_refs(&self, id: &PersonId) -> Result<Vec<ca_insights::CallRef>> {
        // SELECT c.id, c.contact_id, c.occurred_at, plus the three measures
        // via the same json_extract sub-selects `list_calls` uses, filtered to
        // WHERE c.agent_id = ?1 OR c.contact_id = ?1.
    }
```

Write that query out in full following the pattern in `Store::list_calls`, and add `ca-insights.workspace = true` to `crates/ca-store/Cargo.toml`.

- [ ] **Step 2: Register the commands**

In `apps/desktop/src-tauri/src/lib.rs`, add `mod people;` beside `mod live;` and extend `generate_handler!` with:

```rust
            people::list_people,
            people::get_person,
            people::attribute_calls,
            people::assign_call_person,
```

- [ ] **Step 3: Verify it compiles**

Run: `CARGO_INCREMENTAL=0 cargo check -p call-analyzer-desktop`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add apps/desktop/src-tauri crates/ca-store
git commit -m "feat(desktop): expose people, attribution and manual assignment"
```

---

### Task 9: UI — People screen

**Files:**
- Modify: `apps/desktop/ui/src/api.ts` (types + four wrappers)
- Create: `apps/desktop/ui/src/screens/PeopleScreen.tsx`
- Create: `apps/desktop/ui/src/screens/PeopleScreen.css`
- Modify: `apps/desktop/ui/src/App.tsx` (tab + route)
- Modify: `apps/desktop/ui/src/dev/mockBackend.ts` (fixtures for the four commands)

**Interfaces:**
- Consumes: the four Tauri commands from Task 8.
- Produces: `PersonSummary`, `PersonDetail`, `MonthPoint`, `AttributeReport` TypeScript types; a `people` tab.

- [ ] **Step 1: Add the API types and wrappers**

Append to `apps/desktop/ui/src/api.ts`:

```ts
export interface PersonSummary {
  id: string
  kind: 'agent' | 'contact'
  displayName: string
  handle: string
  calls: number
  firstCall: string | null
  lastCall: string | null
}

export interface MonthPoint {
  month: string
  value: number
  calls: number
}

export interface PersonFacts {
  personId: string
  calls: number
  /** null means no analysed call, which is not the same as a score of zero. */
  meanQa: number | null
  meanSentiment: number | null
  compliancePassRate: number | null
  firstCall: string | null
  lastCall: string | null
}

export interface PersonDetail {
  summary: PersonSummary
  facts: PersonFacts
  qaTrend: MonthPoint[]
  sentimentTrend: MonthPoint[]
  repeatContactRate: number | null
}

export interface AttributeReport {
  scanned: number
  attributed: number
  skippedManual: number
  unmatched: number
}

export const listPeople = (kind?: string, search?: string) =>
  invoke<PersonSummary[]>('list_people', { kind, search })
export const getPerson = (personId: string) =>
  invoke<PersonDetail>('get_person', { personId })
export const attributeCalls = (dryRun: boolean, force: boolean) =>
  invoke<AttributeReport>('attribute_calls', { dryRun, force })
export const assignCallPerson = (
  callId: string,
  agentId: string | null,
  contactId: string | null,
) => invoke<void>('assign_call_person', { callId, agentId, contactId })
```

Match the existing `invoke` wrapper's name and import style already used in this file.

- [ ] **Step 2: Build the screen**

Create `PeopleScreen.tsx` with: a kind filter (All / Agents / Contacts), a search box, a table of `displayName | kind | calls | last call`, and a "Re-run attribution" button that calls `attributeCalls(false, false)` and reports the returned counts.

**Absent measures must render `—`, never `0`.** Write a single helper and use it everywhere:

```tsx
/** A score that has not been computed is not a score of zero. */
function score(v: number | null | undefined, digits = 0): string {
  return v === null || v === undefined ? '—' : v.toFixed(digits)
}
```

Clicking a row opens the person detail (Task 10).

- [ ] **Step 3: Add the tab**

In `App.tsx`, add `'people'` to the `Tab` union and `{ id: 'people', label: 'People', icon: '☺' }` to `TABS` between `calls` and `insights`, then render `<PeopleScreen onOpenCall={openCall} />` in the switch.

- [ ] **Step 4: Add mock fixtures**

In `dev/mockBackend.ts`, add handlers for `list_people`, `get_person`, `attribute_calls` and `assign_call_person`. Include at least one person with `calls: 196` and **all measures null**, so the empty-state rendering is exercised, and one with a full 12-month trend.

- [ ] **Step 5: Verify**

```bash
cd apps/desktop/ui && ./node_modules/.bin/tsc -b && ./node_modules/.bin/oxlint && ./node_modules/.bin/vite build
```
Expected: clean. Then `npm run dev` and confirm in a browser that the null-measure person shows `—` rather than `0`.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/ui
git commit -m "feat(ui): add the people screen"
```

---

### Task 10: UI — person detail, repeat-contact badge, manual assignment

**Files:**
- Create: `apps/desktop/ui/src/screens/PersonDetailScreen.tsx`
- Create: `apps/desktop/ui/src/screens/PersonDetailScreen.css`
- Modify: `apps/desktop/ui/src/screens/CallDetailScreen.tsx` (assignment control + repeat badge)
- Modify: `apps/desktop/ui/src/screens/CallsScreen.tsx` (Agent and Contact columns)

**Interfaces:**
- Consumes: `getPerson`, `assignCallPerson`, `listPeople`, and the Phase 3 `Sparkline` component.
- Produces: no new API.

- [ ] **Step 1: Build the person detail**

Header: display name, handle, kind, call count, first and last call. Below it, QA and sentiment trends drawn with the existing `Sparkline` — which **requires at least three points**, so render "not enough data to trend" below that threshold rather than a degenerate line. Then the person's calls, each row opening the call.

- [ ] **Step 2: Add the repeat-contact badge to the call detail**

When a call's contact has earlier calls within 30 days, show `Nth call from this contact` linking to the person. This is the part that makes 4a visible on day one — 1,456 of the 1,972 calls qualify.

- [ ] **Step 3: Add the assignment control**

A dropdown per side (agent, contact) listing people of that kind plus a free-text entry that creates one. On change, call `assignCallPerson`, which sets provenance to `manual`. Show the matched rule name next to a rule-attributed person so a wrong attribution is traceable.

- [ ] **Step 4: Add the columns to the calls list**

Agent and Contact columns, both `—` when unattributed.

- [ ] **Step 5: Verify**

Run the three UI gates, then `npm run dev` and walk: People → a person → one of their calls → reassign the contact → back to People and confirm the counts moved.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/ui
git commit -m "feat(ui): person detail, repeat-contact badge and manual assignment"
```

---

### Task 11: The privacy guard

**Files:**
- Create or modify: a test in `crates/ca-agents/src/lib.rs` (or the analyzer module that renders prompts)

**Interfaces:**
- Consumes: the analyzer's prompt-rendering path and `ca_privacy`.
- Produces: a regression test, no new API.

- [ ] **Step 1: Write the test**

```rust
    /// `people` holds exactly the names and numbers the transcript pipeline
    /// redacts. Identity must not become a new way for them to reach a
    /// provider.
    #[test]
    fn a_participant_name_never_reaches_a_rendered_prompt() {
        let prompt = render_prompt_for_test(/* a call attributed to */ "Babulal Kavaiya", "+918485823280");
        assert!(!prompt.contains("Babulal"), "contact name leaked into the prompt");
        assert!(!prompt.contains("918485823280"), "contact number leaked into the prompt");
    }
```

Wire `render_prompt_for_test` to whatever the real prompt-building function is — read `crates/ca-agents/src/` and call the actual renderer with a call that has participants attached, rather than inventing a helper.

- [ ] **Step 2: Run it**

Run: `CARGO_INCREMENTAL=0 cargo test -p ca-agents`
Expected: PASS. If it fails, the leak is real — fix the renderer, not the test.

- [ ] **Step 3: Commit**

```bash
git add crates/ca-agents
git commit -m "test(agents): assert participant identity never reaches a provider"
```

---

### Task 12: Documentation and gates

**Files:**
- Create: `docs/PHASE4A_IDENTITY.md`
- Modify: `README.md` (features + roadmap)

- [ ] **Step 1: Write `docs/PHASE4A_IDENTITY.md`**

Following the shape of `docs/PHASE3_LIVE.md`: what it does, the corpus measurements that drove it, the attribution patterns table, how to run the backfill, the timezone and handle-normalisation assumptions stated plainly, known limitations (name-form and number-form of one contact are two people; no merge yet), and the privacy position.

- [ ] **Step 2: Update `README.md`**

Add a "Phase 4a — Participant identity ✅" feature block, and change the roadmap line to:

```markdown
- [ ] Phase 4: Multi-tenant & cloud deployment
  - [x] 4a: Participant identity (agents, contacts, teams, repeat-contact rate)
  - [ ] 4b: Roles and scoped views
  - [ ] 4c: Sync server
  - [ ] 4d: Tenant isolation
  - [ ] 4e: Deployment
```

- [ ] **Step 3: Run every gate**

```bash
export PATH="$PATH:/c/Users/yashk/.cargo/bin"
export CARGO_INCREMENTAL=0
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude call-analyzer-desktop
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo check -p call-analyzer-desktop
cd apps/desktop/ui && ./node_modules/.bin/tsc -b && ./node_modules/.bin/oxlint && ./node_modules/.bin/vite build
```

Check free disk before starting — `target/` has filled the drive mid-build before.

- [ ] **Step 4: Commit**

```bash
git add docs README.md
git commit -m "docs: describe participant identity"
```

---

## Self-Review

**Spec coverage:** naming (T1) · schema and provenance (T3) · `ca-identity` and patterns (T2) · handle normalisation (T2) · time (T2) · data flow: ingest, backfill, live (T4, T6, T7) · aggregation and *empty is not zero* (T5, T9) · UI incl. repeat-contact badge (T9, T10) · privacy (T11) · error handling: unmatched calls stay usable (T4, T9) · testing (every task) · out of scope respected — no merge, no rules editor, no server.

**Placeholders:** the one `todo_implement` marker in Task 8 is immediately followed by the real body; `person_call_refs` is specified by its exact query shape and the function it should mirror.

**Type consistency:** `AttributionSource` (Rust) ↔ `attribution` column ↔ no TS equivalent needed. `PersonSummary` fields match between `people.rs` and `api.ts` under camelCase. `CallRef` is defined once in `ca-insights` and used by `ca-store::person_call_refs` and the Tauri layer. `monthly_trend` takes `fn(&CallRef) -> Option<f32>` in both its definition and its three call sites.
