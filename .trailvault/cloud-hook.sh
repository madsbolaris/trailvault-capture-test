#!/bin/bash
set -euo pipefail
source .trailvault/cloud-publication-env.sh
event="$1"
payload="$(cat)"
mkdir -p .trailvault/cloud-observations
printf '%s' "$payload" | jq --arg event "$event" \
  '{event:$event,keys:keys,sessionId,cwd,transcriptPath}' \
  >> .trailvault/cloud-observations/hooks.jsonl
runtime="$PWD/.trailvault/cloud-runtime/target/release/trailvault"
if [ ! -x "$runtime" ]; then
  echo "Trailvault cloud test: runtime missing; setup did not build it." >&2
  exit 1
fi
set +e
printf '%s' "$payload" | "$runtime" hooks copilot-cli "$event" \
  2>> .trailvault/cloud-observations/errors.log
status=$?
set -e
jq -n --arg event "$event" --argjson status "$status" \
  '{event:$event,status:$status}' >> .trailvault/cloud-observations/results.jsonl
if [ "$status" -ne 0 ]; then
  echo "Trailvault cloud hook $event failed with status $status; see local observations." >&2
fi
exit "$status"
