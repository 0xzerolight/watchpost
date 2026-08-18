#!/usr/bin/env bash
# Update an existing watchpost install in place.
# Usage: curl -fsSL https://raw.githubusercontent.com/0xzerolight/watchpost/main/scripts/update.sh | bash
#
# No backup step: the database is copied before any schema migration the new
# image performs, and the newest three copies are kept (see the README).
set -euo pipefail

INSTALL_DIR="${WATCHPOST_DIR:-$HOME/watchpost}"

if [ ! -f "$INSTALL_DIR/docker-compose.yml" ]; then
    echo "[x] No watchpost install found at $INSTALL_DIR" >&2
    echo "    Set WATCHPOST_DIR to point at yours." >&2
    exit 1
fi

cd "$INSTALL_DIR"
echo "[+] Pulling the latest image..."
docker compose pull
echo "[+] Restarting..."
docker compose up -d
echo "[+] watchpost updated. Log: cd $INSTALL_DIR && docker compose logs -f"
