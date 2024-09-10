#!/bin/bash

set -e

CACHE_DIR="/tmp/rpxy/.cache"
CONFIG_DIR="/etc/rpxy"
CONFIG_FILE="$CONFIG_DIR/config.toml"
WEBUI_CONFIG="/var/www/rpxy-webui/storage/app/config.toml"
COMMENT_MARKER="# IMPORTANT: DEACTIVATED This config is deactivated because rpxy-webui is installed"

setup_directories() {
    # Check if systemd is available
    if [ -d /run/systemd/system ]; then
        # Use systemd RuntimeDirectory if available
        if [ -d /run/rpxy ]; then
            RUNTIME_DIR="/run/rpxy"
        # If not available use PrivateTmp
        elif [ -d /tmp/systemd-private-*/tmp ]; then
            RUNTIME_DIR=$(find /tmp/systemd-private-*/tmp -type d -name "rpxy" 2>/dev/null | head -n 1)
        fi
        
        # Create subdirectory for cache
        CACHE_DIR="$RUNTIME_DIR/.cache"
        # Ensure the cache directory exists as it could get deleted on system restart
        mkdir -p "$CACHE_DIR"
        chown rpxy:rpxy "$CACHE_DIR" # not recursively because parent folder is managed by systemd
        chmod 700 "$CACHE_DIR"
    else
        # Fallback to linux tmp directory if no systemd is found
        RUNTIME_DIR="/tmp/rpxy"
        CACHE_DIR="$RUNTIME_DIR/.cache"
        # Ensure the cache directory exists as it could get deleted on system restart
        mkdir -p "$CACHE_DIR"
        chown -R rpxy:rpxy "$RUNTIME_DIR"
        chmod 700 "$CACHE_DIR"
    fi

    echo "Using runtime directory: $RUNTIME_DIR"
    echo "Using cache directory: $CACHE_DIR"
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
    setup_directories
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
