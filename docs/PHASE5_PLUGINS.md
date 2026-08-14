# Phase 5 — Custom agents as plugins

## Status: implemented (2026-08-14)

Phase 2 shipped eight agents. Every team wants a ninth that nobody else wants —
*did we mention the loyalty scheme*, *was the callback promised inside SLA* —
and each is the same shape: ask a model a question about a transcript, get a
structured answer, store it beside the others. None of them justify a code
change.

A plugin is a TOML file describing that question.

## A plugin is data, never code

There is no script, no expression language, and no hook that executes on your
machine. That is the entire security design, and it is not incidental.

This product handles recordings of other people's phone calls. A plugin format
that ran anything would mean a downloaded file could reach the corpus, the
redaction map, or the credential store. A declarative plugin cannot: the worst
a bad one does is ask a model a poor question about an **already-redacted**
transcript and get an answer that fails validation.

What a plugin sees is exactly what a built-in agent sees. The
redact-before-cloud guard sits upstream and is not weakened by any of this.

## Writing one

```toml
name = "callback_sla"
title = "Callback within SLA"

instructions = """
Decide whether the AGENT promised to call the customer back, and whether the
promise named a time within two working days.
"""

findings = true

[[fields]]
name = "promised"
type = "boolean"
required = true

[[fields]]
name = "within_sla"
type = "boolean"
```

Field types are `text`, `number`, `boolean` and `list`. Two working examples
are in [`examples/plugins/`](../examples/plugins).

Drop the file in `plugins/` beside the database, then:

```bash
ca plugins                                    # what loaded, and what did not
ca plugin-check examples/plugins/callback_sla.toml
```

`plugin-check` prints the exact JSON shape the model will be asked for, so a
plugin can be reviewed before it is pointed at a corpus or a metered provider.

## The rules, and why each exists

| Rule | Why |
| --- | --- |
| Name must be lowercase, digits and `_` | It becomes the `agent` column |
| Name cannot be a built-in | `analyses` is keyed by `(call_id, agent)`; a plugin called `compliance` would **overwrite the real compliance agent's results** |
| Two plugins cannot share a name | Same reason; the later one is disabled and says so |
| Instructions must be more than a few words | A one-word prompt produces noise a supervisor then has to disbelieve |
| At least one field | Otherwise there is nothing to store |
| Cannot declare a `findings` field | The harness produces that; a declared one would shadow it |
| `require_citations` defaults to **true** | An unevidenced finding in a risk register is worse than no finding, because someone acts on it |

A missing **required** field fails the response rather than defaulting. A
plugin measuring policy adherence that silently reports `0` when the model said
nothing is producing a fabricated measurement — indistinguishable, on screen,
from a real score of zero.

## Findings

A plugin that sets `findings = true` files into the same register the built-in
compliance agent uses, so it faces the same evidence rule: a finding citing a
segment that does not exist is **dropped**, not stored.

Codes are namespaced with the plugin name — `callback_sla` raising `LATE`
becomes `CALLBACK_SLA_LATE` — so two plugins cannot collide in the register.

An unrecognised severity becomes `medium` rather than dropping the finding: the
finding is evidenced, only its label is unusable.

## Failure behaviour

One broken plugin never disables the others. A file that fails to parse, or
fails a rule, is reported by **name** with the reason, and every other plugin
in the directory still loads:

```text
callback_sla         Callback within SLA              files findings
hijack.toml          FAILED: `compliance` is a built-in agent — a plugin using
                     that name would overwrite its results
```

An absent `plugins/` directory is not an error. Most installs have none.

## Known limitations

- **No UI.** Plugins are managed from the CLI and the filesystem.
- **No scheduling.** A plugin runs when asked; it is not yet part of the
  automatic battery, so adding one does not silently start spending on every
  call in a batch.
- **No versioning.** Editing a plugin changes what future runs produce; stored
  results from the old version are not migrated or marked.
- **One `AgentKind`.** All plugin findings share the `rules` kind so the
  battery's exhaustive matches stay exhaustive; the namespaced code carries
  which plugin it was.

## Testing

```bash
cargo test -p ca-plugins
```

22 tests covering the rules above, including that a plugin cannot take a
built-in name, that a duplicate name disables only the later file, and that a
finding citing an invented segment is dropped.
