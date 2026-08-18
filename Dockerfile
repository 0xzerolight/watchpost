# Builds for whichever architecture it runs on. The published image covers
# linux/amd64 and linux/arm64 by building this file on a native runner per
# architecture and merging the two into one manifest — see
# .github/workflows/docker-publish.yml. Emulating a Rust build under QEMU is
# the alternative, and it is many times slower.
FROM rust:1.90-alpine3.22 AS builder

# musl-dev/gcc: rusqlite is built from bundled C sources.
RUN apk add --no-cache musl-dev gcc

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets

# The cache mounts are not part of the image, so the binary has to be copied off
# /src/target before the mount detaches at the end of this RUN.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
 && cp /src/target/release/watchpost /usr/local/bin/watchpost

FROM alpine:3.22

# ca-certificates: the GitHub client speaks TLS. su-exec: the entrypoint starts
# as root to fix bind-mount ownership, then drops to PUID:PGID. wget ships with
# busybox and serves the healthcheck.
RUN apk add --no-cache ca-certificates su-exec

# uid/gid 1000 matches the usual first host user, so a bind-mounted ./data
# created by that user needs no chown at all. A host user with a different uid
# sets PUID/PGID and the entrypoint adjusts.
RUN addgroup -g 1000 watchpost \
 && adduser -D -u 1000 -G watchpost watchpost \
 && mkdir -p /app/data \
 && chown -R watchpost:watchpost /app

COPY --from=builder /usr/local/bin/watchpost /usr/local/bin/watchpost
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

WORKDIR /app

# No `USER watchpost`: the entrypoint needs root to chown the bind mount, and
# drops to PUID:PGID with su-exec before exec'ing the binary. su-exec takes
# numeric ids, so the runtime user does not have to exist in /etc/passwd.
ENV WATCHPOST_HOST=0.0.0.0 \
    WATCHPOST_DB_PATH=/app/data/watchpost.db \
    PUID=1000 \
    PGID=1000

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD wget -q -O- http://127.0.0.1:8080/health || exit 1

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["watchpost"]
