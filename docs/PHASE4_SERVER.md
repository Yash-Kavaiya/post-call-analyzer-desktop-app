# Phase 4c/4d — Sync server and tenant isolation

## Status: implemented (2026-08-14)

Ten agents on ten laptops, one supervisor who needs one dashboard. This is how
those corpora reach it — and, more importantly, what they leave behind.

## What crosses the wire

**Metrics travel. Content does not.**

| Sent | Not sent |
| --- | --- |
| Call id, real call time, duration, stage | The recording, the transcript, any segment |
| QA score, sentiment, compliance verdict | The model's reasoning, and every quote in it |
| Finding **code** and severity, with a count | The finding's title and detail |
| Opaque participant ids (`person_a1b2…`) | Names, phone numbers, the redaction map |

A dashboard needs to know a call scored 62, ran nine minutes, raised
`PCI_PAN_SPOKEN`, and was handled by `person_a1b2`. It does not need the
sentence the model quoted as evidence. That sentence is the part that makes a
corpus of recorded phone calls sensitive.

Two details worth stating because they are easy to get backwards:

- **A finding sends its code, never its detail.** The detail is prose about a
  specific moment and routinely quotes it — *"the agent said 'give me the card
  number'"*. A code is a label from a fixed vocabulary and reveals nothing the
  code itself does not.
- **Participants are opaque ids.** `person_a1b2c3` means nothing without the
  local `people` table, which never syncs. A server operator can see that two
  calls involved the same person without learning who.

`SyncRecord::assert_no_content` enforces this at **both** ends: the client
refuses to send a record that looks like content, and the server refuses to
store one. The server does not trust the client's guard.

### Measured against the real corpus

Pushing all 1,972 calls to a hub produced a **996 KB** database from a 9.2 MB
source. Checking every contact name and number in the source against the hub
found **zero** present, and the only tables with rows on the hub are
`sync_records` and `schema_migrations` — there is no `calls`, `people` or
`segments` data to leak.

## Tenant isolation: one database per tenant

Not a `tenant_id` column.

A shared table is only as isolated as the least careful `WHERE` clause in the
codebase. Every query has to remember it, now and forever, and the one that
forgets returns another customer's calls rather than failing. A separate file
makes that leak unrepresentable: there is no query you can write against
tenant A's connection that returns tenant B's rows.

The trade is a connection per active tenant and no cross-tenant aggregate
queries. Neither is needed here, and for transcripts of other people's phone
calls the trade is worth making.

**A bearer token maps to exactly one tenant.** A request cannot name a tenant,
so reaching another customer's data needs a different token *and* a different
file.

The tenant slug reaches the filesystem directly, so `validate_slug` allows only
lowercase letters, digits, `-` and `_`. That makes traversal unrepresentable
rather than filtered — there is no encoding of `..` or a path separator that
survives it.

## Running it

```bash
# On the machine that collects
ca sync-server --port 8480 --token "SECRET=acme" --token "OTHER=globex"

# On each machine that produces calls
CA_SYNC_TOKEN=SECRET ca sync http://collector:8480 --peer hq
```

`--mode` is `push` by default. A machine feeding a dashboard should not pull:
it would fetch its own records straight back. A dashboard uses `--mode pull`.

One `ca sync` **drains** rather than sending a single batch — the first sync of
a real corpus is several thousand calls and `BATCH` is 500.

### Transport, stated plainly

The server speaks **plain HTTP with no TLS**. Terminate TLS at a reverse proxy:

```nginx
location /v1/ {
    proxy_pass http://127.0.0.1:8480;
    proxy_set_header Authorization $http_authorization;
}
```

`serve` refuses to bind a non-loopback address unless `--allow-public` is
given, so serving plaintext to a network has to be chosen rather than
stumbled into. The payload is scores rather than transcripts, but scores about
identifiable people over plaintext on an untrusted network is still wrong.

The HTTP parser is small and correspondingly strict: it requires a
`Content-Length`, caps the body at 8 MB and headers at 64, authenticates
*before* reading the body so an unauthenticated client cannot make the server
allocate, and closes every connection. It is not a general-purpose HTTP server
and does not pretend to be one.

## Idempotency, and why there is no conflict resolution

A record is owned by the machine that produced the call and keyed by an id
unique within a tenant, so two machines cannot write the same record. A
re-push **replaces** rather than appends — a client that retries after a
timeout cannot double-count a call in a rollup.

The client's watermark is `calls.updated_at` rather than a local sequence, so a
corpus restored from a backup re-pushes what the peer may have missed instead
of silently resuming past it.

## Known limitations

- **No TLS in-process.** Stated above; use a proxy.
- **Tokens are static**, passed as flags or `CA_SYNC_TOKENS`. There is no
  rotation flow and no per-agent identity on the server.
- **The server stores metrics, not a corpus.** It cannot show a transcript
  because it does not have one — by design, and the reason there is no server
  UI yet.
- **No cross-tenant reporting.** A direct consequence of one file per tenant.

## Testing

```bash
cargo test -p ca-sync
```

29 tests: the protocol and guard in isolation, the client loop against an
in-memory peer, and five that run a real client against a real server over a
real socket — including that two tokens write to two separate corpora and that
a forged token gets a 401.
