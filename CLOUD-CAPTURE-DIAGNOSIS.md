# Cloud capture diagnosis

## Observed facts

- I read `.trailvault/cloud-observations/hooks.jsonl` as a stream with `jq -s`. Immediately before the diagnostic invocation it contained 16 records: 8 `pre-tool-use`, 6 `post-tool-use`, 1 `session-start`, and 1 `user-prompt-submitted`.
- I selected the sole `session-start` record and used only its `sessionId` and `cwd` fields as input. The matching `.git/trailvault-sessions/<sessionId>.json` metadata did not have `transcript_path` or `pending_transcript`.
- I manually piped that two-field object to `.trailvault/cloud-runtime/target/release/trailvault hooks copilot-cli agent-stop`. It exited with status `0` and wrote no stderr. This was an explicit manual invocation, not evidence that a cloud stop hook fired automatically. No native transcript was supplied or intentionally created or modified; the metadata had no transcript path.
- `$HOME/.copilot/session-state` and the selected session's `events.jsonl` both existed. I checked existence only and did not read or enumerate home-directory contents.
- `core.hooksPath` was set in `.git/config` to `.git/copilot-hooks`. The resolved `prepare-commit-msg` and `post-commit` hook files both existed and were executable.

## Interpretation and limits

The missing transcript metadata is an observed gap, but this probe does not establish why it is missing or whether the existing session events are associated with Trailvault capture. The successful manual invocation only shows that this runtime accepted this input; it does not demonstrate transcript capture or cloud support. Existing Git hooks likewise do not prove that hosted commits trigger them or that they create a cloud checkpoint.

`report_progress` commits can happen during the agent run, before the final stop hook. That timing is a possible factor in a missing checkpoint, not a cause established by this experiment.
