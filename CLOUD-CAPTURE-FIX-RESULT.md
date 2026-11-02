# Cloud capture fix verification

This is a bounded end-to-end observation of the hosted session startup and normal `report_progress` commit path. No hooks, configuration, source, or credentials were changed, and no hook was invoked manually. The setup workflow's test-only Rust runtime build completed successfully.

## Before the documentation commit

- The configured `core.hooksPath` resolved to `.git/copilot-hooks`; the active `post-commit`, `pre-push`, and `prepare-commit-msg` hooks contained the `trailvault-managed-v1` marker.
- Safe cloud-observation counters recorded 1 session-start, 10 pre-tool-use, and 9 post-tool-use events. Their corresponding result statuses were all `0`.
- The selected capture metadata exposed phase `commentary`; step and checkpoint counts were unavailable, and `transcript_path` and `pending_transcript` were both absent.
- `errors.log` had zero lines and no upload error was logged.
- No source commit had a `Trailvault-Checkpoint` trailer, and no local `refs/trailvault/checkpoints/*` ref existed.

## Desired commit-time behavior

At commit time, the active managed hooks should capture the real native session log before hosted `report_progress` creates a commit with a `Trailvault-Checkpoint` trailer and a matching local checkpoint ref.

Hook exit statuses alone are not proof that a native checkpoint was captured or uploaded; verify the actual commit trailer, ref, and upload error observations after the normal hosted commit.
