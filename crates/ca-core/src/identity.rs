//! Who was on the call.
//!
//! Deliberately named `Person` rather than "agent": [`crate::AgentKind`]
//! already means an LLM agent, and the two appear in the same files.
//!
//! Everything here is local-only, the same class of data as the redaction map.
//! Contact names and phone numbers are precisely what the transcript pipeline
//! scrubs before a provider sees a call, so identity must not become a second
//! way for them to leave the machine.

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
/// This is not bookkeeping. Without it, a second backfill silently overwrites
/// every correction made by hand — the most likely way this feature loses user
/// data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionSource {
    /// Nothing matched, or attribution has not run.
    #[default]
    None,
    /// A filename pattern matched.
    Rule,
    /// Someone set it by hand.
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
