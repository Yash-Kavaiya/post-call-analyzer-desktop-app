//! Narrowing what a query returns to what the viewer may see.
//!
//! The scope is resolved once from settings and threaded through the read
//! methods rather than being reapplied ad hoc at each call site. Every scoped
//! query binds the same two parameters, always — a `NULL` means unrestricted,
//! not "the filter was left off".
//!
//! While everything runs against one local SQLite file this is a **view
//! filter, not a security boundary**: the operating-system user can open the
//! database directly. It becomes a real boundary only where the scope is
//! applied before data leaves a host, which is what the sync server does.

use ca_core::error::{CaError, Result};
use ca_core::identity::{AccessRole, ViewScope};
use ca_core::ids::{PersonId, TeamId};
use rusqlite::{params, OptionalExtension};

use crate::Store;

/// Settings key holding the handle of whoever is using this machine.
pub const OPERATOR_HANDLE_KEY: &str = "local_agent_handle";

/// The SQL predicate every scoped call query shares.
///
/// Written once and reused so the two halves cannot drift. Named parameters
/// rather than positional: this fragment is spliced into queries that already
/// bind their own arguments, and a positional placeholder would renumber them.
pub(crate) const CALL_SCOPE_SQL: &str = "(:scope_person IS NULL OR c.agent_id = :scope_person)
     AND (:scope_team IS NULL OR c.agent_id IN
          (SELECT id FROM people WHERE team_id = :scope_team))";

/// A team, with how many agents are on it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamSummary {
    pub id: String,
    pub name: String,
    pub members: usize,
}

impl Store {
    /// Who is using this machine, and what they may see.
    ///
    /// Falls back to unrestricted when no operator is configured. That is the
    /// right default for a single-user desktop install: the alternative is an
    /// app that shows an empty corpus until a setting nobody knew about is
    /// filled in.
    pub fn current_scope(&self) -> Result<ViewScope> {
        let handle: Option<String> = self.get_setting(OPERATOR_HANDLE_KEY)?;
        let Some(handle) = handle
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
        else {
            return Ok(ViewScope::all());
        };
        self.scope_for_handle(&handle)
    }

    /// What one agent handle may see, regardless of who is configured.
    ///
    /// Lets `--as` answer "what would this colleague see?" without changing
    /// the stored operator, which is the only safe way to check a scope.
    pub fn scope_for_handle(&self, handle: &str) -> Result<ViewScope> {
        let normalised = ca_identity::normalise_handle(handle);

        let conn = self.lock()?;
        let row: Option<(String, Option<String>, String)> = conn
            .query_row(
                "SELECT id, team_id, role FROM people WHERE kind = 'agent' AND handle = ?1",
                [normalised.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| CaError::Storage(format!("current scope: {e}")))?;

        // A configured operator who is not in `people` yet has no calls of
        // their own, so they see nothing rather than everything. An identity
        // that does not resolve must never widen access.
        let Some((id, team_id, role)) = row else {
            return Ok(ViewScope::own(PersonId::from(normalised)));
        };

        // An unrecognised role reads as `agent`: the narrowest, never the widest.
        Ok(match AccessRole::parse(&role).unwrap_or_default() {
            AccessRole::Admin => ViewScope::all(),
            AccessRole::Supervisor => match team_id {
                Some(team) => ViewScope::team(TeamId::from(team)),
                // A supervisor on no team supervises nobody but themselves.
                None => ViewScope::own(PersonId::from(id)),
            },
            AccessRole::Agent => ViewScope::own(PersonId::from(id)),
        })
    }

    /// Record who is using this machine. `None` clears it.
    pub fn set_operator(&self, handle: Option<&str>) -> Result<()> {
        match handle.map(str::trim).filter(|h| !h.is_empty()) {
            Some(h) => self.put_setting(OPERATOR_HANDLE_KEY, &h.to_string()),
            None => self.delete_setting(OPERATOR_HANDLE_KEY),
        }
    }

    /// The configured operator's handle, if any.
    pub fn operator_handle(&self) -> Result<Option<String>> {
        self.get_setting(OPERATOR_HANDLE_KEY)
    }

    /// Give an agent a role. Contacts have none, and the `kind` clause is what
    /// stops one being given accidentally.
    pub fn set_person_role(&self, id: &PersonId, role: AccessRole) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE people SET role = ?2, updated_at = ?3 WHERE id = ?1 AND kind = 'agent'",
            params![id.as_str(), role.as_str(), chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| CaError::Storage(format!("set role: {e}")))?;
        Ok(())
    }

    /// Put an agent on a team, creating the team if the name is new.
    pub fn set_person_team(&self, id: &PersonId, team: Option<&str>) -> Result<Option<TeamId>> {
        let team_id = match team.map(str::trim).filter(|t| !t.is_empty()) {
            Some(name) => Some(self.upsert_team(name)?),
            None => None,
        };
        let conn = self.lock()?;
        conn.execute(
            "UPDATE people SET team_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![
                id.as_str(),
                team_id.as_ref().map(|t| t.as_str()),
                chrono::Utc::now().to_rfc3339()
            ],
        )
        .map_err(|e| CaError::Storage(format!("set team: {e}")))?;
        Ok(team_id)
    }

    /// Find or create a team by name.
    pub fn upsert_team(&self, name: &str) -> Result<TeamId> {
        let conn = self.lock()?;
        let existing: Option<String> = conn
            .query_row("SELECT id FROM teams WHERE name = ?1", [name], |r| r.get(0))
            .optional()
            .map_err(|e| CaError::Storage(format!("lookup team: {e}")))?;
        if let Some(id) = existing {
            return Ok(TeamId::from(id));
        }
        let id = TeamId::new();
        conn.execute(
            "INSERT INTO teams (id, name, created_at) VALUES (?1,?2,?3)",
            params![id.as_str(), name, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| CaError::Storage(format!("insert team: {e}")))?;
        Ok(id)
    }

    /// Every team, with its membership count.
    pub fn list_teams(&self) -> Result<Vec<TeamSummary>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT t.id, t.name,
                        (SELECT count(*) FROM people p WHERE p.team_id = t.id)
                   FROM teams t ORDER BY t.name",
            )
            .map_err(|e| CaError::Storage(format!("prepare teams: {e}")))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(TeamSummary {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    members: r.get::<_, i64>(2)?.max(0) as usize,
                })
            })
            .map_err(|e| CaError::Storage(format!("query teams: {e}")))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| CaError::Storage(format!("read teams: {e}")))
    }
}

/// The values [`CALL_SCOPE_SQL`] binds by name.
pub(crate) fn scope_params(scope: &ViewScope) -> (Option<String>, Option<String>) {
    (
        scope.person_id.as_ref().map(|p| p.as_str().to_string()),
        scope.team_id.as_ref().map(|t| t.as_str().to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::call::Call;
    use ca_core::identity::PersonKind;

    /// Two agents on one team, one on another, each with a call.
    fn team_corpus() -> (Store, PersonId, PersonId, PersonId) {
        let s = Store::in_memory().unwrap();

        let mk = |handle: &str, team: Option<&str>, role: AccessRole| {
            let p = s.upsert_person(PersonKind::Agent, handle, handle).unwrap();
            s.set_person_role(&p, role).unwrap();
            s.set_person_team(&p, team).unwrap();
            let call = Call::new(format!("k-{handle}"), "local", format!("{handle}.m4a"));
            let id = s.upsert_call(&call).unwrap();
            s.set_call_participants(
                &id,
                Some(&p),
                None,
                Some(chrono::Utc::now()),
                ca_core::identity::AttributionSource::Manual,
            )
            .unwrap();
            p
        };

        let priya = mk("priya", Some("north"), AccessRole::Agent);
        let sam = mk("sam", Some("north"), AccessRole::Supervisor);
        let raj = mk("raj", Some("south"), AccessRole::Agent);
        (s, priya, sam, raj)
    }

    #[test]
    fn an_agent_sees_only_their_own_calls() {
        let (s, priya, _, _) = team_corpus();
        s.set_operator(Some("priya")).unwrap();

        let scope = s.current_scope().unwrap();
        assert_eq!(scope.role, AccessRole::Agent);
        assert_eq!(scope.person_id.as_ref(), Some(&priya));

        let calls = s.list_calls(0, 50, None, &scope).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].agent_name.as_deref(), Some("priya"));
    }

    #[test]
    fn a_supervisor_sees_their_whole_team_but_not_another() {
        let (s, _, _, _) = team_corpus();
        s.set_operator(Some("sam")).unwrap();

        let scope = s.current_scope().unwrap();
        assert_eq!(scope.role, AccessRole::Supervisor);

        let calls = s.list_calls(0, 50, None, &scope).unwrap();
        let names: Vec<_> = calls.iter().filter_map(|c| c.agent_name.clone()).collect();
        assert_eq!(names.len(), 2, "priya and sam, not raj");
        assert!(names.contains(&"priya".to_string()));
        assert!(names.contains(&"sam".to_string()));
        assert!(!names.contains(&"raj".to_string()));
    }

    #[test]
    fn an_admin_sees_everything() {
        let (s, _, _, _) = team_corpus();
        let admin = s.upsert_person(PersonKind::Agent, "boss", "boss").unwrap();
        s.set_person_role(&admin, AccessRole::Admin).unwrap();
        s.set_operator(Some("boss")).unwrap();

        let scope = s.current_scope().unwrap();
        assert!(scope.is_unrestricted());
        assert_eq!(s.list_calls(0, 50, None, &scope).unwrap().len(), 3);
    }

    /// Counts leak the shape of everyone else's work, so they are scoped too.
    #[test]
    fn the_dashboard_counts_are_scoped_as_well_as_the_list() {
        let (s, _, _, _) = team_corpus();
        s.set_operator(Some("priya")).unwrap();
        let scope = s.current_scope().unwrap();

        assert_eq!(s.stats(&scope).unwrap().total_calls, 1);
        assert_eq!(s.stats(&ViewScope::all()).unwrap().total_calls, 3);
    }

    /// Opening another agent's call by id must fail the same way a
    /// non-existent id does — a different error would confirm it exists.
    #[test]
    fn a_call_outside_the_scope_reads_as_missing() {
        let (s, _, _, _) = team_corpus();
        let all = s.list_calls(0, 50, None, &ViewScope::all()).unwrap();
        let rajs_call = all
            .iter()
            .find(|c| c.agent_name.as_deref() == Some("raj"))
            .unwrap();

        s.set_operator(Some("priya")).unwrap();
        let scope = s.current_scope().unwrap();

        let err = s
            .call_detail(&ca_core::ids::CallId::from(rajs_call.id.clone()), &scope)
            .unwrap_err();
        assert!(format!("{err}").contains("not found"), "got {err}");
    }

    /// A single-user desktop install with nobody configured must not open to
    /// an empty corpus.
    #[test]
    fn no_configured_operator_means_no_restriction() {
        let (s, _, _, _) = team_corpus();
        assert!(s.current_scope().unwrap().is_unrestricted());
    }

    /// A handle that matches nobody must narrow, never widen.
    #[test]
    fn an_unknown_operator_sees_nothing_rather_than_everything() {
        let (s, _, _, _) = team_corpus();
        s.set_operator(Some("stranger")).unwrap();

        let scope = s.current_scope().unwrap();
        assert!(!scope.is_unrestricted());
        assert!(s.list_calls(0, 50, None, &scope).unwrap().is_empty());
    }

    /// A supervisor with no team supervises nobody but themselves; falling
    /// back to unrestricted would hand out the whole corpus.
    #[test]
    fn a_supervisor_without_a_team_is_narrowed_to_themselves() {
        let s = Store::in_memory().unwrap();
        let p = s.upsert_person(PersonKind::Agent, "solo", "solo").unwrap();
        s.set_person_role(&p, AccessRole::Supervisor).unwrap();
        s.set_operator(Some("solo")).unwrap();

        let scope = s.current_scope().unwrap();
        assert_eq!(scope.person_id.as_ref(), Some(&p));
        assert!(scope.team_id.is_none());
        assert!(!scope.is_unrestricted());
    }

    #[test]
    fn a_contact_cannot_be_given_a_role() {
        let s = Store::in_memory().unwrap();
        let c = s
            .upsert_person(PersonKind::Contact, "Someone", "someone")
            .unwrap();
        s.set_person_role(&c, AccessRole::Admin).unwrap();

        assert_eq!(
            s.person(&c).unwrap().unwrap().role,
            AccessRole::Agent,
            "the update is scoped to kind = agent, so a contact keeps the default"
        );
    }

    #[test]
    fn teams_are_created_once_and_report_their_membership() {
        let (s, _, _, _) = team_corpus();
        let teams = s.list_teams().unwrap();
        assert_eq!(teams.len(), 2);
        let north = teams.iter().find(|t| t.name == "north").unwrap();
        assert_eq!(north.members, 2);
    }
}
