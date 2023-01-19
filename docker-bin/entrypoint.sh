#!/usr/bin/env sh
LOG_FILE=/var/log/rpxy/rpxy.log

if [ -z ${LOG_TO_FILE} ]; then
  LOG_TO_FILE=false
fi

if "${LOG_TO_FILE}"; then
  echo "rpxy: Start with writing log file"
  /run.sh 2>&1 | tee $LOG_FILE
else
  echo "rpxy: Start without writing log file"
  /run.sh 2>&1
fi
