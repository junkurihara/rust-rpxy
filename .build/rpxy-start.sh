#!/bin/bash

# Check if rpxy-webui is installed
if dpkg-query -W -f='${Status}' rpxy-webui 2>/dev/null | grep -q "install ok installed"; then
    echo "rpxy-webui is installed. Starting rpxy with rpxy-webui"
    exec /usr/local/bin/rpxy --enable-webui
else
    echo "rpxy-webui is not installed. Starting with default config"
    exec /usr/local/bin/rpxy
fi
