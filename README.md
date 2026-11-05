# Private session capture fixture

Fabricated test data only.

## HTML page previews

**Preview HTML pages** runs after each same-repository PR push through **Request
session reports**, or manually with `pull_request`. Merge its trusted workflow
and scripts into the default branch to activate it. It copies changed HTML pages
and static assets below their directories from immutable head-commit blobs;
it never checks out, installs dependencies for, builds, or executes PR code.
Forks, symlinks, hidden paths, non-static files, incomplete file/tree evidence,
changed heads, and invalid blob contents are rejected or excluded.

The archive is named `html-page-preview-pr-<PR number>-<40-character head SHA>`
and retains the original repository paths at the archive root. Limits are
400 files, 1 MiB per file, 20 MiB total, and 14-day retention. No changed HTML
means no preview artifact. Supporting assets must be in the HTML directory or
its descendants; remote services and build-time frameworks are not supported.

The prototype discovers this exact-head artifact even when the trusted workflow
runs on the default branch. HTML opens in the existing browser preview with
device controls, reload, and fullscreen, using an isolated iframe with no parent
credentials or external network access. Other report artifacts keep their
existing layout.

```sh
gh workflow run html-page-preview.yml --repo madsbolaris/trailvault-capture-test \
  -f pull_request=36
python3 -m unittest discover -s scripts -p 'test_html_previews.py' -v
```

## PR smart labels

**Assess PR smart labels** uses the existing **Request session reports** push
trigger and trusted default-branch workflow boundary to compute **Risk** and
**Slop** with the integrity-pinned, tool-free Copilot runtime and existing
`COPILOT_REVIEW_TOKEN`. Merge the workflow and scripts into the default branch
to activate it. Fork PRs and PR-head execution remain excluded.

Both scores are integers from 0 to 100, lower is better, with one short
sentence explaining **Why**. They are review estimates, not measured
probabilities. Risk evaluates correctness/regression exposure; Slop evaluates
avoidable duplication, complexity, boilerplate, dead code, inadequate tests,
and incomplete feature delivery,
not whether AI wrote the code. Only the PR diff is scored: sessions and Slack
are excluded.

Rubric version 2 distinguishes a standalone demo from an integrated UI feature.
When the main deliverable is only a directly opened demo file and the diff adds
neither application entry-point integration nor feature tests, the delivery gap
is substantial Slop (60-79), even if regression Risk is low. Mostly placeholder
behavior or fake core functionality can be severe (80-100). Styling, working
animation, and a dependency-free implementation do not substitute for integration.
Supporting examples, test fixtures, and integrated static pages are not penalized
just for being demos or HTML; static features do not require a backend.
Only integration evidence visible in the diff is assessed, without inventing
requirements or claiming unseen code is absent. Published assessments retain
`rubricVersion` so rubric changes remain distinguishable in score history.

The CI applies the simplified-English and repetition-removal principles from
[aidr](https://github.com/JasonMore/ai-skills/blob/9960f13e2892b675787552ed867bb9f9728211bd/skills/aidr/SKILL.md)
and the audience-focused copy guidance from
[Impeccable clarify](https://github.com/pbakaus/impeccable/blob/c8891a9ae865d6ee311836ae5199bfce0fe115b1/.github/skills/impeccable/reference/clarify.md).
The relevant guidance is adapted into trusted prompts, not downloaded or
executed at runtime. After scoring, a focused Copilot copy-edit pass simplifies
only the explanations; validation prevents it from changing either score.
Each explanation gives one main reason in at most 25 words
and 180 visible characters, without changing the evidence or its uncertainty.
Plain text is preferred; optional inline Markdown is rendered by the prototype's
sanitized Markdown renderer. HTML, links, block formatting, and line breaks are
rejected. Assessment metadata retains the pinned writing-source URLs. Older
published explanations remain readable in history.

The analyzer verifies all changed files and additions/deletions, rejects binary,
truncated, or over-200,000-byte diffs, and rechecks the head and base before
publication. Incomplete evidence is unassessed, never a zero score. Malformed
model output gets one schema-guided retry, then fails explicitly. Scores appear
in the head-bound **PR smart labels** check and one updated Actions-bot comment,
using the `prototype-pr-scores` envelope. It retains the latest 40 assessments
with immutable PR/head/base, run/attempt, evaluator SHA, model, and timestamp.
The prototype renders current-head scores and their Why using the existing
smart-label/history treatment in both PR headers and list rows. Native GitHub
labels are not overwritten.

Manual rerun:

```sh
gh workflow run pr-smart-labels.yml --repo madsbolaris/trailvault-capture-test \
  -f pull_request=1
```

Local contract validation (no publication):

```sh
python3 -m unittest discover -s scripts -p 'test_pr_smart_labels.py' -v
```

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
Before analysis, CI reads the latest published artifact and all its existing
anchored discussions, including earlier comments after history rollover. These
are explicitly untrusted context, not instructions. The agent updates the
previous report against the current captures instead of writing independently
from scratch. A recurring recommendation references its existing discussion
even when its wording, evidence checkpoint, or PR head changes; the original
action and verification wording remain stable so its text anchor still works.
The publisher creates action records only for new recommendations. Repeated
findings update the report explanation/evidence but do nothing to the existing
comment: no duplicate root comment, repetitive reply, task reopening, or
completion-state reset. Existing historical versions and local replies remain
untouched. This prevents new duplicates; it does not delete historical duplicate
discussions or synchronize prototype-local replies back into CI.
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
