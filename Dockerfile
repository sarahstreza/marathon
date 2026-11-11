# syntax=docker/dockerfile:1
#
# Container image for the Marathon server, marathond. See README.md
# ("Running the server in a container") for how to run it.
#
#   docker build -t marathond .
#
# The binary is linked statically against musl (Alpine's Rust targets musl
# and links statically by default), so the final image is distroless/static:
# no shell, no libc, just CA certificates (for https Jellyfin servers), a
# nonroot user, and the binary. SQLite is bundled into the binary
# (sqlx's `sqlite-bundled`), so nothing else is needed at run time.

ARG RUST_VERSION=1.95

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
# distroless has no shell to create directories with; make the data
# directory here so a fresh named volume inherits nonroot's ownership.
RUN install -d /out/data

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /out/marathond /usr/local/bin/marathond
COPY --from=build --chown=65532:65532 /out/data /data
VOLUME ["/data"]
EXPOSE 7878
ENTRYPOINT ["/usr/local/bin/marathond"]
# Mount a config at /config/config.toml (docker/config.toml is a starting
# point). Inside a container loopback is unreachable from outside, so
# listen on every interface. That requires server.token in the config; marathond
# refuses to start without one.
CMD ["--config", "/config/config.toml", "--bind", "0.0.0.0:7878"]
