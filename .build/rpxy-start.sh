#!/bin/bash

set -e

CACHE_DIR="/tmp/rpxy/.cache"
CONFIG_DIR="/etc/rpxy"
CONFIG_FILE="$CONFIG_DIR/config.toml"
WEBUI_CONFIG="/var/www/rpxy-webui/storage/app/config.toml"
COMMENT_MARKER="# IMPORTANT: DEACTIVATED This config is deactivated because rpxy-webui is installed"

# Ensure the cache directory exists as it could get deleted on system restart
create_cache_dir() {
    # Create the temporary directory for rpxy
    mkdir -p "$CACHE_DIR"
    chown -R rpxy:rpxy /tmp/rpxy
    chmod 700 "$CACHE_DIR"
}

# Check if rpxy-webui is installed
is_package_installed() {
    if command -v rpm >/dev/null 2>&1; then
        rpm -q "$1" >/dev/null 2>&1
    elif command -v dpkg-query >/dev/null 2>&1; then
        dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q "install ok installed"
    else
        echo "Neither rpm nor dpkg-query found. Cannot verify installation status of rpxy-webui package." >&2
        return 1
    fi
}

# Create the config file if it doesn't exist
ensure_config_exists() {
    mkdir -p "$CONFIG_DIR"
    [ -f "$CONFIG_FILE" ] || echo "# Standard rpxy Konfigurationsdatei" > "$CONFIG_FILE"
}

add_comment_to_config() {
    if ! grep -q "^$COMMENT_MARKER" "$CONFIG_FILE"; then
        sed -i "1i$COMMENT_MARKER\n" "$CONFIG_FILE"
    fi
}

remove_comment_from_config() {
    sed -i "/^$COMMENT_MARKER/d" "$CONFIG_FILE"
}

main() {
    [ -d "$CACHE_DIR" ] || create_cache_dir
    ensure_config_exists

    if is_package_installed rpxy-webui; then
        echo "rpxy-webui is installed. Starting rpxy with rpxy-webui"
        add_comment_to_config
        exec /usr/bin/rpxy -w -c "$WEBUI_CONFIG"
    else
        echo "rpxy-webui is not installed. Starting with default config"
        remove_comment_from_config
        exec /usr/bin/rpxy -c "$CONFIG_FILE"
    fi
}

main "$@"
