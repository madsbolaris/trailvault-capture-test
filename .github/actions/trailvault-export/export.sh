#!/bin/sh
set -eu
cd "$GITHUB_WORKSPACE"
export GIT_CONFIG_COUNT=1
export GIT_CONFIG_KEY_0=safe.directory
export GIT_CONFIG_VALUE_0="$GITHUB_WORKSPACE"
/usr/local/bin/trailvault cloud export /tmp/checkpoints.json
/usr/local/bin/trailvault cloud upload /tmp/checkpoints.json
/usr/local/bin/trailvault cloud dispatch /tmp/checkpoints.json
rm -f /tmp/checkpoints.json
