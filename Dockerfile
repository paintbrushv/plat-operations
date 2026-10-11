# boxscore-exact container image: the exact-cent calculation catalog, its CLI
# and its MCP stdio server, nothing else. See docs/INSTALL.md (how to run it)
# and docs/DATA_FLOW.md (what goes in, what is stored, what leaves).
#
# Build from the repository root:
#   docker build --build-arg VERSION=0.1.1 --build-arg REVISION="$(git rev-parse HEAD)" \
#     -t boxscore-exact:local .
#
# Base images are pinned by tag and by the multi-arch index digest resolved
# from the registries on 2026-10-11. Re-resolve both digests when preparing a
# release (`docker buildx imagetools inspect <image>:<tag>`), so the release
# picks up current Debian security updates on purpose rather than by accident.

# --- builder ----------------------------------------------------------------
# Debian 12 (bookworm), matching the glibc of the distroless runtime below.
FROM rust:1.99-slim-bookworm@sha256:2c3a22f0a5533ea2dd5a16627bc841228151faa2d4de2644ac9987e4a2f1f2fa AS builder

WORKDIR /build/boxscore
ENV CARGO_TERM_COLOR=never \
    CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_RELEASE_STRIP=symbols

# Dependencies first, so a source-only change reuses the download layer.
# Cargo needs a target to read the manifest; the placeholder is removed again.
COPY boxscore/Cargo.toml boxscore/Cargo.lock ./
RUN mkdir src && touch src/lib.rs && cargo fetch --locked && rm -r src

# The library embeds boxscore/migrations at compile time; nothing else from
# the repository is needed (see .dockerignore).
COPY boxscore/migrations ./migrations
COPY boxscore/src ./src

# /data must exist in the image and belong to the runtime user: a new named
# volume mounted at /data copies its owner and mode, so the non-root process
# can create its database there. The image ships it empty.
RUN cargo build --locked --offline --release --bin boxscore-exact \
 && install -D -m 0755 target/release/boxscore-exact /out/bin/boxscore-exact \
 && install -d -m 0700 /out/data

# --- runtime ----------------------------------------------------------------
# Distroless C runtime (glibc, libgcc): no shell, no package manager.
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

ARG VERSION=dev
ARG REVISION=unknown
LABEL org.opencontainers.image.title="boxscore-exact" \
      org.opencontainers.image.description="Checked integer-cent USD multifamily operating calculations: CLI and MCP stdio server. Local-first; no telemetry." \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.source="https://github.com/paintbrushv/plat-operations" \
      org.opencontainers.image.documentation="https://github.com/paintbrushv/plat-operations/blob/main/docs/INSTALL.md" \
      org.opencontainers.image.licenses="Apache-2.0"

# Only the binary (root-owned, not writable by the runtime user) and the
# empty data directory (owned by the runtime user, mode 0700).
COPY --from=builder /out/bin/boxscore-exact /usr/local/bin/boxscore-exact
COPY --from=builder --chown=65532:65532 /out/data /data

# distroless "nonroot" (uid/gid 65532), stated explicitly so `docker inspect`
# shows it and the CI job can assert it.
USER 65532:65532
WORKDIR /data
VOLUME ["/data"]

# Nothing listens by default, so there is no EXPOSE. When `serve` ships, the
# default command becomes the one-line change below (plus an EXPOSE line):
#   CMD ["serve", "--bind", "0.0.0.0:<port>", "--api-key-file", "/run/secrets/<key-file>"]
# (inside a container the server must bind 0.0.0.0; keep it on loopback at the
# host with `-p 127.0.0.1:<port>:<port>`).
ENTRYPOINT ["/usr/local/bin/boxscore-exact"]
# Stateless: prints the catalog envelope (status "calculated") and exits 0.
CMD ["call", "--function", "describe"]
