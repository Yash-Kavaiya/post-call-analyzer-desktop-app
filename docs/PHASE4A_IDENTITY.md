# Phase 4a — Participant identity

## Status: implemented (2026-08-14)

Phase 1 built the corpus. Phase 2 made every agent's output visible. Phase 3
analysed a call while it was still happening. All three could say what a call
scored. None of them could say **whose call it was**.

Design: [`docs/superpowers/specs/2026-08-14-phase4a-participant-identity-design.md`](superpowers/specs/2026-08-14-phase4a-participant-identity-design.md).
Plan: [`docs/superpowers/plans/2026-08-14-phase4a-participant-identity.md`](superpowers/plans/2026-08-14-phase4a-participant-identity.md).

## Why this is Phase 4's first piece

The roadmap's "multi-tenant & cloud deployment" is five projects, not one:

| | Sub-project | Server? | Status |
| --- | --- | --- | --- |
| 4a | Participant identity | no | **done** |
| 4b | Roles and scoped views | no | — |
| 4c | Sync server | yes | — |
| 4d | Tenant isolation | yes | — |
| 4e | Deployment | yes | — |

Every one of 4b–4d has to answer "whose call is this?". Until 4a, `calls` was
keyed by `(source_name, source_key)` — a file path — and `CallFacts` carried
scores but no participant. Building the server first would have meant inventing
an identity model implicitly and reworking it later.

## How it works

```text
 filename ──▶ ca-identity::attribute ──▶ Attribution { agent, contact, occurred_at, rule }
                       │                              │
                       │                              ▼
              (pure, no I/O)              upsert_person (kind, handle)
                                                      │
                                                      ▼
                                    calls.agent_id · calls.contact_id
                                    calls.occurred_at · calls.attribution
                                                      │
                        ┌─────────────────────────────┼──────────────────────┐
                        ▼                             ▼                      ▼
                  People screen               repeat-contact rate      per-person trends
```

Attribution runs in three places, and the provenance column records which:

| Where | Provenance | When |
| --- | --- | --- |
| Discovery | `rule` | Every newly scanned file, so a fresh ingest needs no backfill |
| `ca attribute` / the UI button | `rule` | Retroactively, over calls already in the corpus |
| A live session | `live` | The agent is whoever is at the machine — no filename could know that |
| Call detail, by hand | `manual` | A correction |

**`manual` is why the column exists.** A backfill skips it. Without that rule
the second run silently discards every correction the user made, which is the
most likely way a feature like this loses data.

## What the corpus showed

Measured before the design, against the real database. Two of these numbers
changed the design.

| | |
| --- | --- |
| Calls | 1,972 |
| Distinct counterparties | 771 |
| Counterparties with more than one call | 255 |
| Calls belonging to a repeat contact | 1,456 |
| Busiest single contact | 196 calls |
| Filenames parsed by one pattern | **1,972 of 1,972** |

1. **This corpus is one person's phone, not a contact-centre export.** The
   filename names the *counterparty*, never the handler. An identity model
   built only around agents would have produced a one-row table. Both sides are
   modelled instead, which is also what a real deployment needs.

2. **`created_at` is ingest time.** 1,967 calls share the date `2026-08-10`,
   when the folder was scanned. Every trend the product could draw was a
   vertical stripe. The filename holds the real time — 12 months, December 2024
   to November 2025 — and identity and the time axis fall out of the same
   parse. `occurred_at` is a new column; `created_at` is never overwritten.

## Attribution patterns

| Pattern | Shape | Captures |
| --- | --- | --- |
| `android_call_recorder` | `Call recording <who>_<yymmdd>_<hhmmss>.<ext>` | contact, time |
| `agent_customer_date` | `<agent>_<customer>_<yyyymmdd>_<hhmmss>` | agent, contact, time |

Patterns are named-capture regexes over the display name or the source key,
tried in order, first match wins. Because a pattern may match the **path**, the
common "one folder per agent" layout is expressible as configuration rather
than as a special case in code:

```json
[{
  "name": "folder_per_agent",
  "regex": "[/\\\\](?P<agent>[^/\\\\]+)[/\\\\][^/\\\\]+$",
  "on_source_key": true,
  "date_format": "none"
}]
```

## Two assumptions, stated rather than hidden

**Handles are not E.164.** A number reduces to its digits with a leading `+`
preserved. Converting `08618075571` to an international number means guessing a
country code, and a wrong guess merges two unrelated people under one identity
— worse than leaving the number as it arrived.

**Timestamps are read as machine-local.** Filenames carry wall-clock time with
no zone. A recording moved between machines in different zones will be off by
the offset.

## Usage

```bash
ca attribute --dry-run   # report what would change, write nothing
ca attribute             # derive participants for the whole corpus
ca attribute --force     # also overwrite assignments made by hand

ca people --kind contact --limit 20
ca people --search kavaiya
```

The desktop app does the same from **People → Re-run attribution**, and new
ingests attribute themselves.

## Empty is not zero

With 7 of 1,972 calls analysed, most people have no scores at all. Every
aggregate is an `Option` and every absent measure renders as `—`.

A mean drawn as `0` would be a fabricated number sitting in the same column,
in the same style, as a real one — and a contact nobody has scored would look
identical to the worst agent on the floor. Call counts, first and last call,
and the repeat-contact rate are real for the whole corpus and are shown
throughout, because they need only identity and time.

## Repeat-contact rate

The share of calls whose contact also called within the **preceding 30 days**.

Both bounds are deliberate. "Repeat" is meaningless without a window, and a
lifetime definition climbs forever — it would render as a steadily worsening
trend when nothing had actually changed. The rate is `None`, not `0`, when no
call carries a time: the question is unanswerable rather than answered
negatively.

## Privacy

`people` is **local-only, the same class of data as `redaction_map`**, which
the schema has always marked "Stays local; never leaves the machine". Contact
names and phone numbers are precisely what the transcript pipeline scrubs
before a provider sees a call.

The transcript redaction guard covers the words *spoken* on a call. It says
nothing about a name attached to the call as *metadata* — the same personal
data arriving by a different route. Nothing wires participants into
`AgentContext`, and a test asserts that a participant's name never appears in
what reaches the model, so adding one later is a visible decision rather than
a quiet leak.

When 4c syncs anything, it syncs person **IDs**, never names.

## Known limitations

- **A contact saved under a name in one call and appearing as a bare number in
  another is two people.** Merging means rewriting identity across calls and
  keeping a record so it can be undone; it belongs in its own change rather
  than half-done here.
- **No rules editor.** Extra patterns go in a settings key as JSON.
- **No teams UI.** The `teams` table and `people.team_id` exist and are indexed;
  nothing populates them yet. Team rollups are 4b.
- **Unattributed calls are ordinary calls.** They stay fully usable and the
  People screen reports how many matched no pattern rather than dropping them
  from totals.

## Resetting

```bash
ca reset --scope analyses      # scores and findings; transcripts kept
ca reset --scope transcripts   # also the text; calls return to `discovered`
ca reset --scope all           # empty the corpus; settings kept
```

A bare `ca reset` deletes nothing — it prints what would go, which is usually
enough to change your mind about the scope. Add `--yes` to proceed. The database
is copied first; `--no-backup` skips that, and a *failed* backup aborts rather
than continuing, because an unbacked-up reset is a riskier operation than the
one that was asked for.

The same thing lives in **Settings → Reset data**, which additionally requires
typing the scope name. A second "are you sure?" button is a reflex to click.

Audio files are never touched by any scope. Settings survive all three: they are
configuration, not corpus.

## Development against the real corpus

`npm run dev` runs the UI in a browser, where there is no Tauri runtime. It used
to draw every screen from invented fixtures, which is fine for checking a layout
and misleading for everything else — a list that looks right on nine
hand-written rows can be wrong on 1,972 real ones.

```bash
ca serve          # loopback only, port 8479
npm run dev       # in apps/desktop/ui
```

With the bridge running the UI reads the real database. A badge in the corner
says `dev · real corpus` or `dev · fixture data`, and any command the bridge
does not implement names itself in the console rather than silently mixing real
and invented data.

This is a development tool. The desktop app talks to the same `Store`
in-process and never over a socket.

## Testing

```bash
cargo test -p ca-identity -p ca-store -p ca-insights -p ca-pipeline
```

`ca-identity` is a pure function over two strings, so its tests use the real
filename shapes observed in the corpus — bare numbers, saved contact names, a
name containing an emoji, and input that matches nothing. The store tests cover
migration, backfill idempotency, that a `manual` assignment survives a re-run,
and that reassigning by hand does not erase the call time.
