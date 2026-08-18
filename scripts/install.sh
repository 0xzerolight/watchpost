#!/usr/bin/env bash
# watchpost installer
# Usage: curl -fsSL https://raw.githubusercontent.com/0xzerolight/watchpost/main/scripts/install.sh | bash
#
# SUPPLY-CHAIN NOTE: curl|bash runs whatever this URL returns, and this script
# also fetches compose.prod.yml (which names the container image) from the same
# ref. Both come from the mutable "main" branch with no commit pin, tag,
# signature or checksum, so a repo compromise or a MITM proxy means arbitrary
# code runs as you. To reduce that trust:
#   1. Read this script before piping it to a shell, or download and run it.
#   2. Pin a release tag instead of "main":
#        WATCHPOST_REF=v1.0.0 curl -fsSL \
#          https://raw.githubusercontent.com/0xzerolight/watchpost/v1.0.0/scripts/install.sh | bash
#      WATCHPOST_REF also pins the compose file this script downloads.
#
# No token prompt: watchpost asks for one in the browser, so a personal access
# token never has to pass through a terminal, a shell history or a file — and
# the manual install below gets the same flow.
set -euo pipefail

REPO="0xzerolight/watchpost"
# Pin to a commit SHA or a release tag for a verifiable install. Defaults to
# "main" (mutable) — see the supply-chain note above.
BRANCH="${WATCHPOST_REF:-main}"
INSTALL_DIR="${WATCHPOST_DIR:-$HOME/watchpost}"
# The port on *this machine*. Deliberately not WATCHPOST_PORT: that is the port
# the binary binds inside the container, which the compose mapping and the
# healthcheck both fix at 8080. See compose.prod.yml.
PORT="${WATCHPOST_HOST_PORT:-8080}"
# Autostart persistence is opt-in. Set WATCHPOST_AUTOSTART=yes|no to answer
# non-interactively; a piped run with no answer defaults to "no".
AUTOSTART="${WATCHPOST_AUTOSTART:-}"

# --- Colours (degrade gracefully) ---
if [ -t 1 ]; then
    BOLD='\033[1m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    RED='\033[0;31m'
    RESET='\033[0m'
else
    BOLD='' GREEN='' YELLOW='' RED='' RESET=''
fi

info()  { echo -e "${GREEN}[+]${RESET} $*"; }
warn()  { echo -e "${YELLOW}[!]${RESET} $*"; }
error() { echo -e "${RED}[x]${RESET} $*" >&2; }

# --- Prerequisites ---
if ! command -v docker &>/dev/null || ! docker compose version &>/dev/null; then
    error "Docker with the Compose plugin is required but was not found."
    echo ""
    echo "  Install Docker: https://docs.docker.com/engine/install/"
    exit 1
fi

info "Docker found: $(docker compose version 2>/dev/null | head -1)"

# --- Install directory ---
info "Installing to ${BOLD}${INSTALL_DIR}${RESET}"
mkdir -p "$INSTALL_DIR/data"

# --- Compose file ---
COMPOSE_URL="https://raw.githubusercontent.com/${REPO}/${BRANCH}/compose.prod.yml"
info "Downloading docker-compose.yml..."
curl -fsSL "$COMPOSE_URL" -o "$INSTALL_DIR/docker-compose.yml"

# --- PUID/PGID, so the bind-mounted ./data is writable by this host user ---
# Docker bind mounts keep host ownership. If this user's uid/gid is not the
# image default (1000), the container must chown ./data to match. The compose
# file reads PUID/PGID from this .env; the entrypoint applies them at startup.
#
# Upserted rather than rewritten, so a re-run never truncates vars the user
# added by hand (a WATCHPOST_PORT override, a WATCHPOST_TZ, a token).
HOST_UID="$(id -u)"
HOST_GID="$(id -g)"
ENV_FILE="$INSTALL_DIR/.env"

upsert_env() {
    local key="$1" value="$2" file="$3"
    # Owner-only on every write path, so a secret the user later adds to .env
    # is never even briefly group- or world-readable.
    if [ ! -f "$file" ]; then
        (umask 077; echo "${key}=${value}" > "$file")
    elif grep -q "^${key}=" "$file"; then
        local tmp
        tmp="$(mktemp "${file}.XXXXXX")"
        grep -v "^${key}=" "$file" > "$tmp"
        echo "${key}=${value}" >> "$tmp"
        mv "$tmp" "$file"
    else
        echo "${key}=${value}" >> "$file"
    fi
    chmod 600 "$file"
}

upsert_env "PUID" "$HOST_UID" "$ENV_FILE"
upsert_env "PGID" "$HOST_GID" "$ENV_FILE"
# Written even when it is the default, so a later `docker compose up -d` in
# this directory publishes the same port rather than quietly reverting to 8080.
upsert_env "WATCHPOST_HOST_PORT" "$PORT" "$ENV_FILE"
chmod 600 "$ENV_FILE"

if [ "$HOST_UID" != "1000" ] || [ "$HOST_GID" != "1000" ]; then
    info "Host uid/gid is ${HOST_UID}:${HOST_GID} (not 1000); wrote PUID/PGID to .env"
else
    info "Wrote PUID/PGID (${HOST_UID}:${HOST_GID}) to .env"
fi

# --- Pull and start ---
cd "$INSTALL_DIR"
info "Pulling the image..."
# Fail with something actionable: the usual cause here is the image not being
# publicly pullable, and `set -e` alone would surface only Docker's "denied".
if ! docker compose pull; then
    error "Could not pull the image (ghcr.io/${REPO})."
    echo ""
    echo "  Most likely it is not publicly accessible, or ghcr.io is unreachable."
    echo "  - Check your network and that https://ghcr.io responds."
    echo "  - Maintainers: confirm the GHCR package visibility is set to Public."
    echo "  - Pin a known release instead of latest: WATCHPOST_REF=<tag> and re-run."
    exit 1
fi

info "Starting watchpost..."
docker compose up -d

# --- Wait for it to answer ---
info "Waiting for watchpost to start..."
for _ in $(seq 1 30); do
    if curl -sf "http://localhost:${PORT}/health" >/dev/null 2>&1; then
        break
    fi
    sleep 1
done

if ! curl -sf "http://localhost:${PORT}/health" >/dev/null 2>&1; then
    warn "No answer yet on /health. Check: cd ${INSTALL_DIR} && docker compose logs"
fi

# --- Desktop integration (Linux only) ---
if [[ "${OSTYPE:-}" == linux* ]]; then
    DESKTOP_DIR="$HOME/.local/share/applications"
    mkdir -p "$DESKTOP_DIR"
    cat > "$DESKTOP_DIR/watchpost.desktop" << DESKTOP_EOF
[Desktop Entry]
Type=Application
Name=watchpost
Comment=Self-hosted tracking for your own GitHub repositories
Exec=xdg-open http://localhost:${PORT}
Icon=applications-internet
Terminal=false
Categories=Network;Monitor;
StartupNotify=false
DESKTOP_EOF
    info "Desktop entry installed (find 'watchpost' in your app launcher)"

    # --- Autostart at boot (opt-in) ---
    # A systemd user service plus enable-linger starts the container at boot
    # even when nobody is logged in. That is real persistence on the machine,
    # so it is asked for rather than installed quietly.
    want_autostart="no"
    case "$AUTOSTART" in
        yes|y|YES|Y) want_autostart="yes" ;;
        no|n|NO|N)   want_autostart="no" ;;
        "")
            if [ -t 0 ]; then
                printf "%b" "${YELLOW}[?]${RESET} Start watchpost automatically at boot (systemd user service + linger)? [y/N] "
                read -r reply </dev/tty || reply=""
                case "$reply" in y|Y|yes|YES) want_autostart="yes" ;; esac
            else
                warn "Skipping boot autostart (non-interactive). Set WATCHPOST_AUTOSTART=yes to enable it."
            fi
            ;;
    esac

    if [ "$want_autostart" = "yes" ]; then
        SYSTEMD_DIR="$HOME/.config/systemd/user"
        mkdir -p "$SYSTEMD_DIR"
        cat > "$SYSTEMD_DIR/watchpost.service" << SERVICE_EOF
[Unit]
Description=watchpost - self-hosted GitHub repo metrics
After=network-online.target docker.service
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=${INSTALL_DIR}
ExecStart=/usr/bin/docker compose up
ExecStop=/usr/bin/docker compose down
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
SERVICE_EOF

        systemctl --user daemon-reload
        systemctl --user enable watchpost 2>/dev/null || true
        info "Systemd user service installed and enabled"

        if command -v loginctl &>/dev/null; then
            loginctl enable-linger "$USER" 2>/dev/null || \
                warn "Could not enable lingering. Run: sudo loginctl enable-linger $USER"
        fi
        info "To remove autostart later: systemctl --user disable --now watchpost &&"
        info "  rm -f \"$HOME/.config/systemd/user/watchpost.service\" && loginctl disable-linger \"$USER\""
    else
        info "Boot autostart not installed. Enable later by re-running with WATCHPOST_AUTOSTART=yes."
    fi
fi

# --- Done ---
echo ""
info "${BOLD}watchpost is running.${RESET}"
echo ""
echo "  Open http://localhost:${PORT} and paste a GitHub token to finish setup."
echo "  Data is stored in: ${INSTALL_DIR}/data/"
echo ""
echo "  Manage it with:"
echo "    cd ${INSTALL_DIR} && docker compose logs -f   # Follow the log"
echo "    cd ${INSTALL_DIR} && docker compose restart   # Restart"
echo "    cd ${INSTALL_DIR} && docker compose down      # Stop"
echo ""
echo "  Update:"
echo "    cd ${INSTALL_DIR} && docker compose pull && docker compose up -d"
echo ""
echo "  Uninstall:"
echo "    cd ${INSTALL_DIR} && docker compose down      # Stop the container"
echo "    systemctl --user disable --now watchpost      # Remove boot autostart (if enabled)"
echo "    rm -f ~/.config/systemd/user/watchpost.service ~/.local/share/applications/watchpost.desktop"
echo "    loginctl disable-linger \"\$USER\"               # Stop running at boot when logged out"
echo "    rm -rf ${INSTALL_DIR}                          # Remove the install dir and its data (irreversible)"
echo ""

if command -v xdg-open &>/dev/null; then
    xdg-open "http://localhost:${PORT}" 2>/dev/null &
elif command -v open &>/dev/null; then
    open "http://localhost:${PORT}" 2>/dev/null &
fi
