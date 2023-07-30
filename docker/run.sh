#!/usr/bin/env sh
CONFIG_FILE=/etc/rpxy.toml

# debug level logging
if [ -z $LOG_LEVEL ]; then
  LOG_LEVEL=info
fi
echo "rpxy: Logging with level ${LOG_LEVEL}"

# continuously watch and reload the config file
if [ -z $WATCH ]; then
  WATCH=false
else
  if [ "$WATCH" = "true" ]; then
    WATCH=true
  else
    WATCH=false
  fi
fi

if  $WATCH ; then
  RUST_LOG=${LOG_LEVEL} /rpxy/bin/rpxy --config ${CONFIG_FILE} -w
else
  RUST_LOG=${LOG_LEVEL} /rpxy/bin/rpxy --config ${CONFIG_FILE}
fi
