#!/bin/bash

# Ensure the cache directory exists as it could get deleted on system restart
if [ ! -d /tmp/rpxy/.cache ]; then
    # Create the temporary directory for rpxy
    mkdir -p /tmp/rpxy/.cache
    chown -R rpxy:rpxy /tmp/rpxy
    chmod 700 /tmp/rpxy/.cache
fi

# Check if rpxy-webui is installed
if dpkg-query -W -f='${Status}' rpxy-webui 2>/dev/null | grep -q "install ok installed"; then
    echo "rpxy-webui is installed. Starting rpxy with rpxy-webui"
    exec /usr/local/bin/rpxy -w -c /var/www/rpxy-webui/storage/app/config.toml
else
    echo "rpxy-webui is not installed. Starting with default config"
    
    # Ensure the /etc/rpxy directory exists
    if [ ! -d /etc/rpxy ]; then
        mkdir -p /etc/rpxy
    fi
    
    # Create the config file if it doesn't exist
    if [ ! -f /etc/rpxy/config.toml ]; then
        echo "# Default rpxy config file" > /etc/rpxy/config.toml
    fi
    
    exec /usr/local/bin/rpxy -c /etc/rpxy/config.toml
fi
