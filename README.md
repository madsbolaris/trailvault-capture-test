# Private session capture fixture

Fabricated test data only.

## PR session reports

After each same-repository PR push (including draft PRs), **Request session
reports** triggers **Analyze PR sessions** on the default branch. The analysis
workflow and its scripts must be merged into the default branch before automatic
runs are available. Fork PRs are excluded. PR-head code is never checked out or
executed by the privileged analyzer.

The workflow publishes two CI-versioned PR comment artifacts, using the same
`prototype-artifact` envelope as the issue-classification reports:

- **Agent behavior**: evidence-backed mistakes, repeated unsuccessful queries,
  unnecessary tool use, and ignored constraints, considering subsequent recovery.
  Findings must propose a reusable improvement to agent skills, instructions, or
  tooling and declare that remediation target. PR defects, missing product tests,
  merge readiness, and implementation requests are not agent-behavior findings.
- **Human decisions**: explicit, durable repository-rule proposals from user-role
  messages only. This fixture includes AI-orchestrated prompts, so a user role is
  not proof of human authorship. Maintainers must confirm authorship, scope, and
  the proposal before adopting a rule. No memories or instructions are changed
  automatically.

Reports lead with their conclusion, summary cards, and actionable findings,
not workflow metadata or snapshot inventories. Immutable run/head provenance
remains in the version envelope rather than in the visible report body.
Each actionable finding publishes one anchored `actions` record in its report's
version metadata, with immutable transcript-line evidence. The prototype
assembles this into the shared report highlight, native Comments thread, and
todo with one identity. The recommended-change paragraph (or proposed rule in
Human decisions), not the section heading or explanation, is the text anchor;
activating the highlighted passage reveals its existing discussion and
completion/ownership controls without changing the comment's presentation.
Follow-ups are assigned to
the PR author from trusted GitHub PR metadata, not an identity invented by the
model. There is no separate standalone finding comment
or independently editable duplicate task list. CI actions survive reload;
local replies and task edits remain prototype-local.
Evidence links are relative prototype routes with a captured session ID,
checkpoint commit, and one-based compact-transcript record. They open the Chat
pane at that immutable record without replacing the report with a GitHub blob.
Findings below the 0.8 uncalibrated-confidence threshold appear as deferred
candidates in the report without creating todos.
Native GitHub displays the report and action text; the prototype additionally
consumes the anchored action metadata. Local todo completion, discussion replies,
and verification outcomes are distinct. Rerunning the same report publication
does not duplicate its version or anchored action.
New pushes retain old report versions with their original head/run
provenance. Long histories roll into another comment without deleting history.

### Credentials and operation

`COPILOT_REVIEW_TOKEN` is an encrypted repository Actions secret containing a
Copilot-authorized GitHub credential. A maintainer can provision their existing
`gh` login without printing it:

```sh
set -o pipefail
gh auth token --hostname github.com |
  gh secret set COPILOT_REVIEW_TOKEN --repo madsbolaris/trailvault-capture-test
```

This credential has the account's token permissions and Copilot usage is billed
to that account. Prefer a dedicated least-privilege credential for longer-term
operation; rotate/revoke it when appropriate. The workflow's separate
`GITHUB_TOKEN` publishes comments/checks. The model process receives only the
Copilot credential, an isolated temporary home, and no tools, MCP servers,
custom instructions, repository checkout, or inherited GitHub publisher token.
Copilot CLI is integrity-locked in `session-review-runtime/package-lock.json`.
Raw transcripts, model output and credential values are not logged or uploaded
as Actions artifacts.

Slack is deliberately excluded. Analysis reads only published checkpoint refs,
using the trusted Trailvault `related` command for PR references and exact
`Trailvault-Checkpoint` trailers for contributor sessions. Mere mentions are not
proof of contribution. Prefix-overlapping snapshots are deduplicated; divergent
histories remain separate. Child transcripts and local-only sessions are not
assessed. A short scope note states this limitation without a separate technical
coverage section. The publisher removes its old detached finding comments only
after successfully retaining the new reports; old report versions are preserved.

There is a 2 MiB / 10,000-record evidence budget. Each model batch preserves
complete sessions, up to 300,000 serialized bytes, with at most eight batches per
report. Oversized evidence fails explicitly, never silently truncates. Invalid
model responses log safe validation diagnostics and receive one fresh-response
attempt with the schema requirements restated. A second invalid response blocks
publication without replacing the existing reports. No eligible
records produces **not assessed**, not a clean result. A successful check means
analysis completed, not that agent behavior or a proposed rule was approved.
Stale heads are rejected before every publication write.

Checkpoint publication can finish after the push-triggered analysis. Missing
source checkpoints fail explicitly. The cloud publisher automatically requests
fresh reports after successfully publishing its checkpoints for an open PR.
For other late publications or a manual rerun:

```sh
gh workflow run session-reports.yml --repo madsbolaris/trailvault-capture-test \
  -f pull_request=1
```

Local validation (no report publication):

```sh
python3 -m unittest discover -s scripts -p 'test_session_reports.py' -v
npm ci --prefix session-review-runtime --ignore-scripts --no-audit --no-fund
COPILOT_GITHUB_TOKEN="$(gh auth token --hostname github.com)" \
  python3 scripts/session_reports.py preflight
```
