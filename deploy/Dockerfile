# Sync server only.
#
# The desktop app is a Windows binary and is not containerised: it needs audio
# devices, a GUI and the user's own credential store. What runs in a container
# is the collector — the piece that holds metrics and no call content.

# ---- build ----------------------------------------------------------------
FROM rust:1.82-slim-bookworm AS build

# rusqlite is built with `bundled`, which compiles SQLite from source, so a C
# toolchain is needed here and nothing is needed at runtime.
RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential pkg-config \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# Manifests first so a source-only change does not re-download the registry.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY apps/desktop/src-tauri/Cargo.toml ./apps/desktop/src-tauri/Cargo.toml

# Only the CLI is built. The desktop crate is a workspace member but needs the
# Tauri toolchain, so it is excluded rather than pulled into a server image.
RUN cargo build --release -p ca-cli --locked

# ---- run ------------------------------------------------------------------
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home --uid 10001 analyzer

COPY --from=build /src/target/release/ca /usr/local/bin/ca

# Corpora live on a volume. Without one, a container restart loses every
# metric the collector has been sent.
VOLUME ["/data"]
ENV APPDATA=/data

USER analyzer
EXPOSE 8480

# Binding 0.0.0.0 inside a container is correct — the container boundary is
# the loopback here — but TLS still terminates at the proxy in front.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD ["/usr/local/bin/ca", "sync-health", "--url", "http://127.0.0.1:8480"]

ENTRYPOINT ["/usr/local/bin/ca"]
CMD ["sync-server", "--host", "0.0.0.0", "--port", "8480", "--allow-public"]
