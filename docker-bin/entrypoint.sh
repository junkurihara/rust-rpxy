#!/usr/bin/env bash
LOG_FILE=/var/log/rpxy/rpxy.log

/run.sh 2>&1 | tee $LOG_FILE
