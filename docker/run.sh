#!/usr/bin/env sh
CONFIG_FILE=/etc/rpxy.toml
LOG_DIR=/rpxy/log
LOGGING=${LOG_TO_FILE:-false}

# debug level logging
if [ -z $LOG_LEVEL ]; then
  LOG_LEVEL=info
fi
echo "rpxy: Logging with level ${LOG_LEVEL}"


if "${LOGGING}"; then
  echo "rpxy: Start with writing log files"
  RUST_LOG=${LOG_LEVEL} /rpxy/bin/rpxy --config ${CONFIG_FILE} --log-dir ${LOG_DIR}
else
  echo "rpxy: Start without writing log files"
  RUST_LOG=${LOG_LEVEL} /rpxy/bin/rpxy --config ${CONFIG_FILE}
fi
