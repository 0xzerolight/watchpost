# Single-arch (host architecture) image. Multi-arch via cargo-zigbuild is a
# post-v1 follow-up; see README.
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

# ca-certificates: the GitHub client speaks TLS. wget ships with busybox and
# serves the healthcheck.
RUN apk add --no-cache ca-certificates

# uid/gid 1000 matches the usual first host user, so a bind-mounted ./data
# created by that user is writable without a chown.
RUN addgroup -g 1000 watchpost \
 && adduser -D -u 1000 -G watchpost watchpost \
 && mkdir -p /app/data \
 && chown -R watchpost:watchpost /app

COPY --from=builder /usr/local/bin/watchpost /usr/local/bin/watchpost

USER watchpost
WORKDIR /app

ENV WATCHPOST_HOST=0.0.0.0 \
    WATCHPOST_DB_PATH=/app/data/watchpost.db

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD wget -q -O- http://127.0.0.1:8080/health || exit 1

CMD ["watchpost"]
