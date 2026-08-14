# Phase 4a — Participant identity

**Status:** proposed, 2026-08-14.

## Why this comes first

The roadmap's Phase 4 reads "multi-tenant & cloud deployment". That is five
independently shippable projects, not one:

| | Sub-project | Server? | Unblocks |
| --- | --- | --- | --- |
| 4a | Participant identity | no | everything below |
| 4b | Roles and scoped views | no | 4c |
| 4c | Sync server | yes | 4d |
| 4d | Tenant isolation | yes | — |
| 4e | Deployment | yes | — |

Every one of 4b–4d needs to answer "whose call is this?", and today the corpus
cannot. `calls` is keyed by `(source_name, source_key)` — effectively a file
path. `CallFacts` carries QA score, sentiment, compliance and root cause but no
participant. "Agent" in this codebase already means an *LLM* agent
(`AgentKind`); the human agent exists only as a per-segment `Role`.

The product can say *this call scored 62*. It cannot say *Priya's calls average
62, down 8 this month*, or *this is the fifth time this customer has called
about the same thing*. Both are ordinary contact-centre questions, and both are
one identity column away.

**4a is that column, built entirely locally.** No server, no network, no new
egress.

## What the corpus actually contains

Measured against the live database, 2026-08-14. These numbers set the design.

| | |
| --- | --- |
| Calls | 1,972 (all `source_name = local`) |
| Distinct counterparties | 771 |
| Counterparties with more than one call | 255 |
| Calls belonging to a repeat contact | 1,456 |
| Counterparty is a saved contact name | 536 calls across 44 contacts |
| Counterparty is a bare phone number | 1,436 calls |
| Busiest single contact | 196 calls |

Every filename follows one shape:

```text
Call recording +918485823280_251112_142326.m4a
Call recording Ishita AI Cloud Koradia_251007_101533.m4a
```

A single regex parses **1,972 of 1,972 (100%)**.

Three consequences, each of which closes off a design that reads fine on paper:

1. **This is one person's phone, not a contact-centre export.** The filename
   names the *counterparty*, never the handler. An identity model built only
   around agents yields a scorecard table with one row — nothing to look at
   until multi-agent data is ingested. The dense structure is all on the other
   side of the call.

2. **`created_at` is ingest time, not call time.** 1,967 calls share the date
   `2026-08-10`, when the folder was scanned; three distinct dates cover the
   whole corpus, and one row reads `2000-01-01`. Any trend the product draws
   today is a vertical stripe. The filename holds the real time: **12 months,
   December 2024 through November 2025**, with the eleven full months running
   119–226 calls each. Identity and the time axis come out of the same parse.

3. **Almost nothing is analysed yet.** 1,951 calls sit at `discovered`; 20 have
   transcripts, 7 have any analysis, and `findings` has **zero rows**. Rollup
   views must therefore be honest about absent data rather than averaging over
   nothing — see *Empty is not zero*.

## Naming

The human side is called **`Person`**, never "agent", because `AgentKind`
already means an LLM agent and the two would otherwise collide in the same
file. A person has a `PersonKind` of `Agent` or `Contact`.

## Data model

Migration `0006_people`:

```sql
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
CREATE UNIQUE INDEX idx_people_handle ON people(kind, handle);
CREATE INDEX idx_people_team ON people(team_id);

-- Deleting a person must not delete their calls: the recording is the record.
ALTER TABLE calls ADD COLUMN agent_id    TEXT REFERENCES people(id) ON DELETE SET NULL;
ALTER TABLE calls ADD COLUMN contact_id  TEXT REFERENCES people(id) ON DELETE SET NULL;
ALTER TABLE calls ADD COLUMN occurred_at TEXT;
ALTER TABLE calls ADD COLUMN attribution TEXT NOT NULL DEFAULT 'none';

CREATE INDEX idx_calls_agent    ON calls(agent_id, occurred_at);
CREATE INDEX idx_calls_contact  ON calls(contact_id, occurred_at);
CREATE INDEX idx_calls_occurred ON calls(occurred_at DESC);
```

Two columns on `calls` rather than a `call_participants` join table. A QA call
is two-sided by construction — `segments.role` already models exactly two roles
— so a join table would buy conference support nobody has asked for at the cost
of a join on every list query. If N-party calls ever arrive, that is a
migration, not a rewrite.

`ON DELETE SET NULL` is deliberate and differs from the cascade used elsewhere.
Removing a person from the roster must not erase their calls; the corpus is
evidence.

### Provenance is load-bearing

`attribution` records *how* a call was attributed: `rule`, `manual`, `live`, or
`none`. It is not decoration. Without it the second backfill run silently
overwrites every correction made by hand — the single most likely way this
feature loses user data. Re-runs skip `manual` rows unless explicitly forced.

## `ca-identity`

A new crate whose core is a pure function over a string, shaped exactly like
`ca-rules` in Phase 3 so the whole thing tests without I/O:

```rust
pub fn attribute(display_name: &str, source_key: &str, patterns: &[Pattern]) -> Attribution;
pub fn builtin_patterns() -> Vec<Pattern>;
pub fn normalise_handle(raw: &str) -> String;

pub struct Attribution {
    pub agent: Option<Handle>,
    pub contact: Option<Handle>,
    pub occurred_at: Option<DateTime<Utc>>,
    /// Which pattern matched. Surfaced in the UI so a wrong attribution is
    /// traceable to the rule that produced it.
    pub rule: Option<String>,
}

pub struct Handle {
    pub kind: PersonKind,
    pub display_name: String,
    pub handle: String,
}
```

A `Pattern` is a named-capture regex plus a declaration of which group means
what (`agent`, `contact`, `ymd`, `hms`) and how to read the timestamp. Patterns
are tried in order; the first match wins.

Two built-ins ship in code:

| Pattern | Matches |
| --- | --- |
| `android_call_recorder` | `Call recording <who>_<yymmdd>_<hhmmss>.<ext>` — 100% of the current corpus |
| `agent_customer_date` | `<agent>_<customer>_<yyyymmdd>_<hhmmss>` — the common export shape |

Additional patterns come from a `settings` key as JSON. Because a pattern may
match `source_key` as well as `display_name`, the widespread "one folder per
agent" layout is expressible as configuration with no special case in code.

### Handle normalisation

Phone numbers reduce to their digits, preserving a leading `+`; names fold to
lowercase with collapsed whitespace. Uniqueness is `(kind, handle)`, so the same
number may exist once as an agent and once as a contact without colliding.

Deliberately **not** E.164. Converting `08618075571` to E.164 requires guessing
a country code, and a wrong guess merges two unrelated people under one
identity — a worse failure than leaving the number in the form it arrived in.

**Known limitation, accepted for 4a:** a contact saved under a name in one call
and appearing as a bare number in another becomes two people. Merging is real
work — rewriting identity across calls, keeping a merge record so it can be
undone — and belongs in its own change rather than being half-done here.

### Time

Filename timestamps carry local wall-clock time and no zone. They are parsed as
machine-local and stored RFC3339 UTC, matching every other timestamp in the
schema. This is an assumption, not a fact: a recording moved between machines in
different zones will be off by the offset. It is documented rather than hidden,
and `created_at` is never overwritten, so ingest time stays recoverable.

## Data flow

- **New ingests** attribute automatically as part of registering the call.
- **Existing calls** are attributed by an explicit `ca attribute` run — never
  silently at startup. 1,972 rows are not rewritten without the user asking.
  `--dry-run` prints what would change; the default skips `manual`.
- **Live sessions** (Phase 3) attribute the agent side to a configured
  `local_agent_handle` with provenance `live`, and `occurred_at` to session
  start. The contact side stays unknown unless named.

## Aggregation

`ca-insights` gains:

```rust
pub struct PersonFacts {
    pub person_id: String,
    pub calls: usize,
    pub mean_qa: Option<f32>,
    pub mean_sentiment: Option<f32>,
    pub compliance_pass_rate: Option<f32>,
    pub first_call: Option<DateTime<Utc>>,
    pub last_call: Option<DateTime<Utc>>,
}
```

plus a monthly trend series per person and a **repeat-contact rate**: of the
calls in a given month, the share whose contact also called within the
preceding 30 days. Both bounds are explicit on purpose — "repeat" is meaningless
without a window, and a lifetime definition would drift upward forever and look
like a worsening trend when nothing had changed. On the current corpus this is
computable for 1,456 calls immediately, because it needs only identity and time,
not analysis.

`CallFacts` gains `agent_id` and `contact_id`.

### Empty is not zero

Every aggregate is `Option`. With 7 analysed calls, a mean QA rendered as `0`
would be a fabricated number presented with the same visual weight as a real
one, and this screen invites exactly that error. Absent measures render as `—`.
Call counts, first/last call and repeat-contact rate are real for the whole
corpus and can be shown throughout; scores cannot, and must say so.

This mirrors the existing treatment of `compliance_passed: Option<bool>`, where
"did not run" is already deliberately distinguished from "failed".

## UI

- **People** — agents and contacts, searchable, sortable by calls, mean QA,
  mean sentiment, last call.
- **Person detail** — header stats, monthly trend (reusing the Phase 3
  `Sparkline`, which needs at least three points), that person's calls, and
  their findings rolled up.
- **Calls list** — Agent and Contact columns; filter by person.
- **Call detail** — a manual assignment control, which sets provenance to
  `manual` and thereby protects the choice from later backfills. The matched
  rule name is shown next to a rule-attributed person.
- **Repeat-contact badge** — "5th call from this contact", linking to the prior
  calls. This is the piece that makes 4a visible on day one.

## Privacy

`people` is **local-only, the same class of data as `redaction_map`**, which the
schema already marks "Stays local; never leaves the machine". Contact names and
phone numbers are precisely what the transcript pipeline redacts before a
provider sees a call; storing them as identity is a separate, deliberate, local
decision and must not become a new egress path.

Three rules follow:

1. No participant name or handle may enter a provider prompt. A test asserts
   that a name present in `people` does not appear in the analyzer's rendered
   prompt.
2. `guard_redaction` is untouched. 4a adds no cloud path and weakens no check.
3. When 4c later syncs anything, it syncs person **IDs**, never names — the
   mapping stays on the machine, exactly as the redaction map does.

## Error handling

Attribution is best-effort and never fatal. A call that matches no pattern keeps
`attribution = 'none'`, both IDs null, and behaves exactly as it does today.
Unattributed calls remain fully usable — they simply do not appear in per-person
rollups, and the People screen reports how many are unattributed rather than
quietly dropping them from totals.

A malformed user-supplied pattern is rejected at load with the reason, and the
built-ins continue to work.

## Testing

| Unit | Covered by |
| --- | --- |
| `ca-identity` | Pure tests over the real filename shapes observed in the corpus: bare-number form, saved-contact form, a contact name containing an emoji, and input that matches nothing |
| Handle normalisation | E.164 folding, whitespace and case folding, agent/contact collision |
| Migration | Applies to a populated database; existing rows survive with `attribution = 'none'` |
| Backfill | Idempotent across runs; a `manual` assignment survives a re-run |
| `ca-insights` | Person rollups, repeat-contact rate, and that absent measures stay `None` rather than becoming `0` |
| Privacy | A name in `people` never reaches a rendered provider prompt |

## Out of scope

Deferred deliberately, each to its own change: merging two people; a rules
editor screen; roster import; supervisor/agent permissions (4b); anything
involving a server (4c–4e).
