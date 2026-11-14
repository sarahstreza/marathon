# syntax=docker/dockerfile:1
#
# Container image for the Marathon server, marathond. See README.md
# ("Running the server in a container") for how to run it.
#
#   docker build -t marathond .
#
# The binary is linked statically against musl (Alpine's Rust targets musl
# and links statically by default). SQLite is bundled into it (sqlx's
# `sqlite-bundled`). The runtime image is plain Alpine: the binary, CA
# certificates (for https Jellyfin servers), ffmpeg (for audio-only
# streams, PLAN.md M12), and su-exec plus a small
# entrypoint (docker/entrypoint.sh) that runs the server as PUID:PGID,
# 1000:1000 unless overridden:
#
#   docker run -e PUID=1001 -e PGID=1001 ...

ARG RUST_VERSION=1.95
ARG ALPINE_VERSION=3.24

FROM docker.io/library/rust:${RUST_VERSION}-alpine AS build
# A C toolchain for the bundled SQLite and aws-lc (rustls' crypto).
RUN apk add --no-cache build-base
WORKDIR /src
COPY . .
# Release-profile overrides for the image only, so the workspace's own
# profile (used by developers and CI) is untouched: fat LTO and a single
# codegen unit for a smaller, faster binary, and no symbols. (Not
# panic=abort: a panicking request task must not take the server down.)
ENV CARGO_PROFILE_RELEASE_LTO=fat \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
    CARGO_PROFILE_RELEASE_STRIP=symbols
# The cache mounts keep the registry and target directory between builds;
# the target directory is a mount, so copy the binary out of it.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p marathond \
    && install -D target/release/marathond /out/marathond

FROM docker.io/library/alpine:${ALPINE_VERSION}
RUN apk add --no-cache ca-certificates ffmpeg su-exec \
    && install -d -o 1000 -g 1000 /data
COPY --from=build /out/marathond /usr/local/bin/marathond
COPY docker/entrypoint.sh /usr/local/bin/docker-entrypoint.sh
# The uid and gid marathond runs as; the entrypoint re-owns /data to match.
# Numeric ids need no passwd entry.
ENV PUID=1000 \
    PGID=1000
VOLUME ["/data"]
EXPOSE 7878
# Starts as root only long enough to fix /data's ownership and drop to
# PUID:PGID; see docker/entrypoint.sh.
ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
# Mount a config at /config/config.toml (docker/config.toml is a starting
# point). Inside a container loopback is unreachable from outside, so
# listen on every interface. That requires server.token in the config; marathond
# refuses to start without one.
CMD ["--config", "/config/config.toml", "--bind", "0.0.0.0:7878"]
