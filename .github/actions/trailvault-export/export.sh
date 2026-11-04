#!/bin/sh
set -eu
cd "$GITHUB_WORKSPACE"
. .trailvault/cloud-publication-env.sh
/usr/local/bin/trailvault cloud export /tmp/checkpoints.json
/usr/local/bin/trailvault cloud upload /tmp/checkpoints.json
/usr/local/bin/trailvault cloud dispatch /tmp/checkpoints.json
rm -f /tmp/checkpoints.json
