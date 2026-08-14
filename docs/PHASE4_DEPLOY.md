# Phase 4e — Deploying the collector

## Status: implemented (2026-08-14)

Only the **collector** is deployed. The desktop app is a Windows binary that
needs audio devices, a GUI and the user's own credential store; there is
nothing sensible to containerise about it. What runs on a server is the piece
that holds metrics and no call content.

## What is in `deploy/`

| File | Purpose |
| --- | --- |
| `Dockerfile` | Two-stage build; ships a single static-ish binary and no toolchain |
| `compose.yaml` | Collector plus a TLS-terminating proxy |
| `Caddyfile` | Certificates obtained and renewed automatically |

```bash
cd deploy
export CA_DOMAIN=sync.example.com
export CA_SYNC_TOKENS="a-long-random-secret=acme,another-secret=globex"
docker compose up -d
```

Then, on each machine that produces calls:

```bash
CA_SYNC_TOKEN=a-long-random-secret ca sync https://sync.example.com --peer hq
```

## The proxy is not optional

`ca sync-server` speaks plain HTTP on purpose — see
[PHASE4_SERVER.md](PHASE4_SERVER.md). The compose file does not publish the
collector's port to the host at all; only the proxy can reach it, and the
proxy is the only thing listening on 443.

The Caddyfile forwards `/v1/*` and answers 404 to everything else. Forwarding
the whole namespace would widen the surface to routes the collector does not
serve.

## Health

`GET /v1/health` is answered **before authentication** and before the request
body is read. A container probe and a load balancer both need it, and neither
should hold a bearer token to ask whether a process is alive — a token in a
healthcheck is a token in every process list on the box. It returns a literal
`{"ok": true}`; there is no data in it to leak.

```bash
ca sync-health --url http://127.0.0.1:8480   # exit 0 healthy, 1 not
```

## Storage and backups

Corpora live on the `corpora` volume, one SQLite file per tenant. Without a
volume a restart loses every metric the collector has been sent.

```bash
docker compose exec collector ca backup-all
```

`backup-all` copies every tenant beside itself with a timestamp, and reports
per tenant. One tenant failing does not abandon the rest: a partial backup is
worth more than none.

SQLite runs in WAL mode, so copying the file under load can miss the tail of
the write-ahead log. For a consistent copy, either stop the collector or use
`sqlite3 .backup`. For a collector holding derived metrics that can be
re-pushed from source machines, the file copy is usually enough — and that is
the point of the metrics being derived.

## Configuration

| Variable | Meaning |
| --- | --- |
| `CA_SYNC_TOKENS` | `token=tenant`, comma separated. Required. |
| `CA_DOMAIN` | Hostname clients connect to. Required by the proxy. |
| `RUST_LOG` | `info` by default. |
| `APPDATA` | Where corpora live. Set to `/data` in the image. |

Both required variables use compose's `:?` form, so a missing one fails at
`up` with a message naming it rather than starting a server that rejects every
request.

**Keep tokens out of the compose file.** Use a secrets manager, or an env file
that is not in git. A token is the entire authentication story and maps
directly to one tenant's data.

## What this does not do

- **No horizontal scaling.** One process, SQLite files on a local volume.
  Several collectors would need shared storage, which SQLite is a poor fit for.
  A tenant's throughput is one sync per machine per interval, so this is a long
  way from being the constraint.
- **No rotation flow for tokens.** Changing one means editing the environment
  and restarting.
- **No server-side UI.** The collector holds metrics, not a corpus; it cannot
  show a transcript because it does not have one.
- **The image is not built here.** It is written and reviewed but has not been
  built or run — this machine has no Docker daemon.
