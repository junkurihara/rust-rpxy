#!/usr/bin/env sh
CONFIG_FILE=/etc/rpxy.toml

# debug level logging
if [ -z $LOG_LEVEL ]; then
  LOG_LEVEL=info
fi
echo "rpxy: Logging with level ${LOG_LEVEL}"

RUST_LOG=${LOG_LEVEL} /rpxy/bin/rpxy --config ${CONFIG_FILE}
