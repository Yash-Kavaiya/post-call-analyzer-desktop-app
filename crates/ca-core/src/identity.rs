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

/// What someone is allowed to see.
///
/// **This is a view filter, not a security boundary.** Everything runs against
/// one local SQLite file that the signed-in operating-system user can read
/// directly, so a role here narrows what the app *shows* — it cannot stop
/// anyone with the file from reading all of it. Real enforcement needs the
/// server, where the scope is applied before data leaves the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessRole {
    /// Sees only their own calls.
    #[default]
    Agent,
    /// Sees every call handled by their team.
    Supervisor,
    /// Sees everything.
    Admin,
}

impl AccessRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            AccessRole::Agent => "agent",
            AccessRole::Supervisor => "supervisor",
            AccessRole::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "agent" => Some(AccessRole::Agent),
            "supervisor" => Some(AccessRole::Supervisor),
            "admin" => Some(AccessRole::Admin),
            _ => None,
        }
    }

    /// Whether this role may change another person's role or team.
    pub fn can_administer(&self) -> bool {
        matches!(self, AccessRole::Admin)
    }
}

/// Which calls a query may return.
///
/// Built from the configured operator rather than passed around ad hoc, so a
/// new query cannot forget to apply it — the parameters are always present and
/// a `None` means unrestricted rather than "filter omitted".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewScope {
    /// Restrict to calls handled by this person. `None` for no restriction.
    pub person_id: Option<PersonId>,
    /// Restrict to calls handled by anyone on this team.
    pub team_id: Option<TeamId>,
    pub role: AccessRole,
}

impl ViewScope {
    /// Everything, unrestricted. What an admin gets, and what the CLI uses
    /// unless asked otherwise.
    pub fn all() -> Self {
        Self {
            person_id: None,
            team_id: None,
            role: AccessRole::Admin,
        }
    }

    /// One agent's own calls.
    pub fn own(person_id: PersonId) -> Self {
        Self {
            person_id: Some(person_id),
            team_id: None,
            role: AccessRole::Agent,
        }
    }

    /// A team's calls.
    pub fn team(team_id: TeamId) -> Self {
        Self {
            person_id: None,
            team_id: Some(team_id),
            role: AccessRole::Supervisor,
        }
    }

    /// Whether this scope filters anything at all.
    pub fn is_unrestricted(&self) -> bool {
        self.person_id.is_none() && self.team_id.is_none()
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
    /// Only meaningful for [`PersonKind::Agent`]; a contact has no role.
    pub role: AccessRole,
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
    fn role_round_trips_and_defaults_to_the_least_privileged() {
        for r in [AccessRole::Agent, AccessRole::Supervisor, AccessRole::Admin] {
            assert_eq!(AccessRole::parse(r.as_str()), Some(r));
        }
        assert_eq!(AccessRole::parse("root"), None);
        // A parse failure or a missing column must not silently grant admin.
        assert_eq!(AccessRole::default(), AccessRole::Agent);
        assert!(!AccessRole::Agent.can_administer());
        assert!(!AccessRole::Supervisor.can_administer());
        assert!(AccessRole::Admin.can_administer());
    }

    #[test]
    fn an_unrestricted_scope_is_distinguishable_from_a_narrow_one() {
        assert!(ViewScope::all().is_unrestricted());
        assert!(!ViewScope::own(PersonId::from("person_1")).is_unrestricted());
        assert!(!ViewScope::team(TeamId::from("team_1")).is_unrestricted());
    }

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
