# Trailvault hosted capture compatibility probe

This documentation-only probe tests repository hooks in GitHub Copilot cloud.
It does not change application behavior or repair, remove, or configure hooks.

Before editing, `.github/hooks` was absent, so no hook JSON or configured
executable was available to check for existence or executable permission.
No hook execution error was observed.

Capture success is not established by this document. The PR body reports test
results and whether a `Trailvault-Checkpoint` commit trailer or
`refs/trailvault/checkpoints` refs were generated. Tokens and native transcript
content must not be included. Keep this probe PR draft and unmerged.
