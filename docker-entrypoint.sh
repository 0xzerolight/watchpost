#!/bin/sh
# watchpost container entrypoint.
#
# A bind-mounted ./data keeps its HOST ownership, so a host user whose uid is
# not the image's 1000 cannot write it and the database fails to open. Started
# as root, this aligns the data directory with the host-supplied PUID/PGID and
# then drops to that user, so the binary never runs as root. Started as a
# non-root user already (a compose `user:` override), it cannot chown anything,
# so it checks writability and fails with the fix rather than letting sqlite
# report "unable to open database file" from deep inside startup.
set -eu

PUID="${PUID:-1000}"
PGID="${PGID:-1000}"
DATA_DIR="/app/data"

case "$PUID" in *[!0-9]*) echo "ERROR: PUID must be numeric (got '$PUID')" >&2; exit 1;; esac
case "$PGID" in *[!0-9]*) echo "ERROR: PGID must be numeric (got '$PGID')" >&2; exit 1;; esac

if [ "$PUID" = "0" ]; then
    echo "WARNING: PUID=0 — watchpost will run as root, which defeats the privilege drop" >&2
fi

# WATCHPOST_DB_PATH is a documented setting, and an absolute or relocated value
# can put the database outside the bind mount — at a directory the handling
# below would otherwise never touch, leaving it root-owned.
db_path="${WATCHPOST_DB_PATH:-$DATA_DIR/watchpost.db}"
case "$db_path" in
    /*) db_dir="$(dirname "$db_path")" ;;        # absolute — use as-is
    *)  db_dir="/app/$(dirname "$db_path")" ;;   # relative — resolved from /app
esac
# When it already lives inside the data volume the handling below covers it;
# collapsing to DATA_DIR keeps the common case touching a single path.
case "$db_dir/" in
    "$DATA_DIR"/* | "$DATA_DIR/") db_dir="$DATA_DIR" ;;
esac

# Not root: no chown is possible, so report what a host would have to fix.
if [ "$(id -u)" != "0" ]; then
    for dir in "$DATA_DIR" "$db_dir"; do
        mkdir -p "$dir" 2>/dev/null || true
        if ! { touch "$dir/.wtest" 2>/dev/null && rm -f "$dir/.wtest"; }; then
            echo "ERROR: $dir is not writable by uid=$(id -u) gid=$(id -g)" >&2
            echo "       (owned by uid=$(stat -c %u "$dir" 2>/dev/null || echo '?')" \
                 "gid=$(stat -c %g "$dir" 2>/dev/null || echo '?'))." >&2
            echo "       The container is not running as root, so it cannot fix this itself." >&2
            echo "       On the host:  chown -R $(id -u):$(id -g) <the directory mounted at $dir>" >&2
            echo "       or drop the compose 'user:' override and set PUID/PGID instead." >&2
            exit 1
        fi
    done
    exec "$@"
fi

for dir in "$DATA_DIR" "$db_dir"; do
    mkdir -p "$dir"
    # Skip the recursive chown when ownership already matches: it is expensive
    # on a large bind mount and buys nothing when nothing changed. Both halves
    # are checked, so a gid-only change is not missed.
    if [ "$(stat -c %u "$dir")" != "$PUID" ] || [ "$(stat -c %g "$dir")" != "$PGID" ]; then
        chown -R "$PUID:$PGID" "$dir"
    fi
done

exec su-exec "$PUID:$PGID" "$@"
