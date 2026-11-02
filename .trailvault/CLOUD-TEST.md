# Private cloud capture experiment

This branch contains a test-only Rust source snapshot of Trailvault.
Repository hooks invoke the release binary built by Copilot setup steps.
Hook observations contain payload field names, session IDs, paths and exit
statuses only, never prompts, tool content or credentials. They are ignored by
Git. Capture success must be verified, not assumed from the hook JSON.

Do not modify hooks, runtime source, settings, setup steps or Git configuration
while performing the documentation-only hosted probe. Do not fabricate agent
events or transcripts. Do not read credentials or dump environment variables.
