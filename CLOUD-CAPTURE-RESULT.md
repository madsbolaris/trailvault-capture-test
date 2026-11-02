# Hosted capture probe result

This documentation-only probe did not run or install the Trailvault runtime or
manually invoke any session hooks. The approved release binary was present and
executable at `.trailvault/cloud-runtime/target/release/trailvault`.

## Observed capture evidence

At the pre-edit inspection, `hooks.jsonl` contained 13 events: one
`user-prompt-submitted`, one `session-start`, six `pre-tool-use`, and five
`post-tool-use`. The paired `results.jsonl` contained 13 events, each with
status `0`. This is evidence that hooks fired automatically during the hosted
session; it is not evidence that checkpoint capture or publishing works.

The observation log reported these available payload-key names:
`cwd`, `initialPrompt`, `prompt`, `sessionId`, `source`, `timestamp`,
`toolArgs`, `toolName`, and `toolResult`. Only field names and aggregate
statuses were used here; prompt, transcript, and tool-content bodies were not
copied.

`errors.log` was empty. Transcript access was not demonstrated: the
observations contain no transcript-availability error, but they also do not
show that transcript contents were retrieved.

## Checkpoint evidence

The available `.git/trailvault-sessions` metadata reported
`checkpoint_count: 0`. No checkpoint-related commit trailers were found in the
inspected local commit history, and no local checkpoint refs were present.
Therefore, checkpoint trailers were not generated and there is no evidence
that checkpoint refs were uploaded.

These results describe only the permitted local observations and metadata;
they do not establish that capture or publishing succeeds.
