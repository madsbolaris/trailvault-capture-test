import copy
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import session_reports as r

SHA = "a" * 40
OTHER = "b" * 40
CHECKPOINT = "01M670QQM0VJN4K0HYX6K1SZ0F"
REFS = [f"refs/trailvault/checkpoints/{CHECKPOINT[-2:]}/{CHECKPOINT} {OTHER}"]
PROVENANCE = {"runId": "123", "runAttempt": 1, "evaluatorSha": OTHER,
              "createdAt": "2026-11-05T13:00:00Z",
              "runUrl": f"https://github.com/{r.REPOSITORY}/actions/runs/123"}


def pr(sha=SHA, repository=r.REPOSITORY, state="open"):
    return {"number": 1, "state": state, "commits": 0,
            "user": {"login": "madsbolaris"},
            "head": {"sha": sha, "ref": "feature", "repo": {"full_name": repository}}}


def row(key="one", role="user", session="session"):
    return {"id": key, "session_id": session,
            "url": f"#/{r.REPOSITORY}/pull/1/artifacts?at=h_{session}%40L2&checkpoint={OTHER}",
            "record": {"type": "message", "role": role, "content": "Synthetic fixture only."}}


def finding(key="one", kind="agent-behavior"):
    result = {"title": "Avoid a repeated failed query", "detail": "The captured operation failed twice.",
              "evidence_ids": [key], "action": "Teach the agent skill to adapt after a missing-file error",
              "verification": "The next trace uses the discovered path", "confidence": 0.9}
    if kind == "agent-behavior":
        result["remediation_target"] = "skills"
    return result


def bundle():
    return {"sessions": [{"checkpoint": OTHER, "session_id": "session", "coverage": False}],
            "follow_up_assignee": "madsbolaris",
            "records": [row(), row("assistant", "assistant")],
            "limitations": ["Fixture coverage only."]}


def reports():
    b = bundle()
    return b, [r.analyze(b, kind, lambda prompt: {
        "reviewed_ids": [item["id"] for item in json.loads(prompt.split("UNTRUSTED_DATA:\n")[1])],
        "findings": [finding(kind=kind)],
    }) for kind in r.REPORTS]


def bot(body, identity=10):
    return {"id": identity, "body": body, "user": {"login": "github-actions[bot]", "type": "Bot"}}


class AnalysisTests(unittest.TestCase):
    def test_agent_scope_requires_reusable_remediation_and_excludes_pr_review(self):
        prompts = []
        def review(prompt):
            prompts.append(prompt)
            return {"reviewed_ids": ["one", "assistant"], "findings": [finding()]}
        result = r.analyze(bundle(), "agent-behavior", review)
        self.assertEqual(result["findings"][0]["remediation_target"], "skills")
        self.assertIn("This is NOT a PR review", prompts[0])
        self.assertIn("not fix the PR", prompts[0])
        for target in ("implementation", "missing-tests", None):
            candidate = {**finding(), "remediation_target": target}
            with self.subTest(target=target), self.assertRaisesRegex(r.ReportError, "remediation"):
                r.validate_analysis({"reviewed_ids": ["one"], "findings": [candidate]}, [row()], "agent-behavior")

    def test_invalid_model_shape_gets_one_explicit_schema_repair(self):
        responses = [
            {"reviewed_ids": ["one", "assistant"],
             "findings": [{**finding(), "unexpected": "must not be logged"}]},
            {"reviewed_ids": ["one", "assistant"], "findings": [finding()]},
        ]
        prompts = []
        def review(prompt):
            prompts.append(prompt)
            return responses.pop(0)
        with patch("builtins.print") as log:
            result = r.analyze(bundle(), "agent-behavior", review)
        self.assertEqual(len(result["findings"]), 1)
        self.assertEqual(len(prompts), 2)
        self.assertIn("1 unexpected fields", prompts[1])
        self.assertNotIn("must not be logged", str(log.call_args_list))
        self.assertEqual(json.loads(prompts[1].split("UNTRUSTED_DATA:\n")[1]), bundle()["records"])
        log.assert_called_once()

    def test_invalid_model_response_still_blocks_after_one_repair(self):
        calls = []
        def review(_):
            calls.append(True)
            return {"reviewed_ids": ["one", "assistant"], "findings": [{}]}
        with patch("builtins.print"):
            with self.assertRaisesRegex(r.ReportError, "Invalid finding shape"):
                r.analyze(bundle(), "agent-behavior", review)
        self.assertEqual(len(calls), 2)

    def test_only_user_messages_reach_human_decisions(self):
        prompts = []
        def review(prompt):
            prompts.append(prompt)
            return {"reviewed_ids": ["one"], "findings": []}
        result = r.analyze(bundle(), "human-decisions", review)
        records = json.loads(prompts[0].split("UNTRUSTED_DATA:\n")[1])
        self.assertEqual([item["id"] for item in records], ["one"])
        self.assertIn("NOT prove human authorship", prompts[0])
        self.assertEqual(result["status"], "assessed")

    def test_empty_input_is_not_clean_review(self):
        b = bundle()
        b["records"] = [row("assistant", "assistant")]
        result = r.analyze(b, "human-decisions", lambda _: self.fail("No model needed"))
        self.assertEqual(result["status"], "not_assessed")
        self.assertIn("not a clean review", r.render(result, b, SHA))

    def test_complete_session_batches_preserve_context(self):
        records = [row("one", session="first"), row("two", session="second"),
                   row("three", session="first")]
        limit = len(json.dumps([records[0], records[2]]).encode()) + 1
        with patch.object(r, "MAX_BATCH_BYTES", limit):
            batches = r.review_batches(records)
        self.assertEqual([[v["id"] for v in batch] for batch in batches], [["one", "three"], ["two"]])

    def test_oversized_session_is_rejected_not_truncated(self):
        with patch.object(r, "MAX_BATCH_BYTES", 10), self.assertRaises(r.ReportError):
            r.review_batches([row()])

    def test_incomplete_or_duplicate_coverage_rejected(self):
        for ids in ([], ["one", "one"], ["foreign"]):
            with self.subTest(ids=ids), self.assertRaises(r.ReportError):
                r.validate_analysis({"reviewed_ids": ids, "findings": []}, [row()], "agent-behavior")

    def test_invalid_findings_block_publication(self):
        for key, value in (("evidence_ids", ["foreign"]), ("evidence_ids", []),
                           ("confidence", -0.5), ("confidence", 1.1), ("confidence", float("nan")),
                           ("confidence", True), ("action", "x" * 181),
                           ("verification", "<script>"), ("detail", "ghp_" + "x" * 30),
                           ("title", "@someone"), ("detail", "https://untrusted.example")):
            candidate = finding()
            candidate[key] = value
            with self.subTest(key=key, value=value), self.assertRaises(r.ReportError):
                r.validate_analysis({"reviewed_ids": ["one"], "findings": [candidate]},
                                    [row()], "agent-behavior")

    def test_duplicate_json_and_duplicate_findings_rejected(self):
        with self.assertRaises(r.ReportError):
            r.unique_json('{"findings": [], "findings": []}')
        with self.assertRaises(r.ReportError):
            r.validate_analysis({"reviewed_ids": ["one"], "findings": [finding(), finding()]},
                                [row()], "agent-behavior")

    def test_task_length_is_checked_as_combined_text(self):
        candidate = finding()
        candidate.update(action="a" * 150, verification="b" * 150)
        with self.assertRaises(r.ReportError):
            r.validate_analysis({"reviewed_ids": ["one"], "findings": [candidate]},
                                [row()], "agent-behavior")

    def test_lower_confidence_candidates_are_visible_without_actionable_todos(self):
        candidate = finding(kind="human-decisions")
        candidate["confidence"] = 0.7
        b = bundle()
        result = r.analyze(b, "human-decisions", lambda _: {
            "reviewed_ids": ["one"], "findings": [candidate]})
        self.assertEqual(result["findings"], [])
        self.assertEqual(len(result["observations"]), 1)
        self.assertIn("Deferred candidates", r.render(result, b, SHA))
        plan = r.publication_plan([result], b, SHA, PROVENANCE, [])
        self.assertEqual(len(plan), 1)
        artifact = r.unique_json(plan[0][1].split("\n")[0][len(r.MARKER):-4])
        self.assertEqual(artifact["versions"][0]["actions"], [])


class CollectionTests(unittest.TestCase):
    def related(self, snapshots=None):
        return {"version": 1, "target": {"host": "github.com", "repository": r.REPOSITORY,
                                       "number": 1}, "snapshots": snapshots or []}

    def root(self):
        return {"checkpoint_id": CHECKPOINT, "sessions": [{
            "metadata": "/0/metadata.json", "compact_transcript": "/0/transcript.jsonl"}]}

    def test_source_commit_trailers_select_contributor_sessions(self):
        commits = [{"commit": {"message": f"Implement feature\n\nTrailvault-Checkpoint: {CHECKPOINT}\n"}}]
        selected = r.snapshots_for_pr(1, commits, self.related(),
                                     lambda *_: json.dumps(self.root()), REFS)
        self.assertEqual(selected, {(OTHER, 0): CHECKPOINT})

    def test_missing_checkpoint_is_explicit_failure(self):
        commits = [{"commit": {"message": f"Trailvault-Checkpoint: {CHECKPOINT}"}}]
        with self.assertRaisesRegex(r.ReportError, "not published yet"):
            r.snapshots_for_pr(1, commits, self.related(), refs=[])

    def test_invalid_index_paths_and_identity_rejected(self):
        commits = [{"commit": {"message": f"Trailvault-Checkpoint: {CHECKPOINT}"}}]
        for field in ("metadata", "compact_transcript"):
            root = self.root()
            root["sessions"][0][field] = "/../../secrets"
            with self.subTest(field=field), self.assertRaises(r.ReportError):
                r.snapshots_for_pr(1, commits, self.related(), lambda *_: json.dumps(root), REFS)

    def test_related_read_only_session_requires_fetched_ref(self):
        snapshot = {"checkpoint": CHECKPOINT, "checkpoint_commit": OTHER, "session_directory": 0}
        self.assertEqual(r.snapshots_for_pr(1, [], self.related([snapshot]), refs=REFS),
                         {(OTHER, 0): CHECKPOINT})
        snapshot["checkpoint_commit"] = SHA
        with self.assertRaises(r.ReportError):
            r.snapshots_for_pr(1, [], self.related([snapshot]), refs=REFS)

    def test_cross_pr_related_report_rejected(self):
        related = self.related()
        related["target"]["number"] = 2
        with self.assertRaises(r.ReportError):
            r.snapshots_for_pr(1, [], related, refs=REFS)

    def test_collection_reads_git_blobs_without_checking_out_pr(self):
        record = {**row()["record"], "content": "Synthetic\u2028fixture only."}
        payload = "\n" + json.dumps({"type": "header"}) + "\n\n" + json.dumps(record, ensure_ascii=False) + "\n"
        def read(revision, path):
            if path.endswith("metadata.json"):
                return json.dumps({"session_id": "test", "checkpoint_id": CHECKPOINT})
            return payload
        with patch.object(r, "current_pr", return_value=pr()), patch.object(r, "git") as git, (
                patch.object(r, "execute", return_value=json.dumps(self.related()))), (
                patch.object(r, "pages", return_value=[])), (
                patch.object(r, "snapshots_for_pr", return_value={(OTHER, 0): CHECKPOINT})), (
                patch.object(r, "blob", side_effect=read)):
            result = r.collect(1, SHA)
        self.assertEqual(result["records"][0]["url"],
                         f"#/{r.REPOSITORY}/pull/1/artifacts?at=h_test%40L4&checkpoint={OTHER}")
        self.assertEqual(result["records"][0]["id"], f"{OTHER}:0:4")
        self.assertFalse(any("checkout" in call.args for call in git.call_args_list))

    def test_api_commit_cap_cannot_silently_hide_source_checkpoints(self):
        candidate = {**pr(), "commits": 251}
        with patch.object(r, "current_pr", return_value=candidate), (
                patch.object(r, "git")), (
                patch.object(r, "execute", return_value=json.dumps(self.related()))), (
                patch.object(r, "pages", return_value=[{}] * 250)):
            with self.assertRaisesRegex(r.ReportError, "incomplete source-commit"):
                r.collect(1, SHA)


class PublicationTests(unittest.TestCase):
    def test_assignment_comes_from_the_pr_author_and_missing_owner_blocks_publication(self):
        b, values = reports()
        b["follow_up_assignee"] = "another-author"
        plan = r.publication_plan(values, b, SHA, PROVENANCE, [])
        artifact = r.unique_json(plan[0][1].split("\n")[0][len(r.MARKER):-4])
        self.assertEqual(artifact["versions"][0]["actions"][0]["assignee"], "another-author")
        for owner in (None, "", "not a login"):
            b["follow_up_assignee"] = owner
            with self.assertRaisesRegex(r.ReportError, "PR author"):
                r.publication_plan(values, b, SHA, PROVENANCE, [])

    def test_two_versioned_reports_and_single_todo_identity_per_finding(self):
        b, values = reports()
        plan = r.publication_plan(values, b, SHA, PROVENANCE, [])
        self.assertEqual(len(plan), 2)
        artifacts = [r.unique_json(body.split("\n")[0][len(r.MARKER):-4])
                     for _, body in plan if body.startswith(r.MARKER)]
        self.assertEqual({artifact["title"] for artifact in artifacts}, set(r.REPORTS.values()))
        for artifact in artifacts:
            version = artifact["versions"][0]
            self.assertEqual(version["headSha"], SHA)
            self.assertEqual(version["evaluatorSha"], OTHER)
            self.assertNotIn("tasks", version)
            action = version["actions"][0]
            self.assertEqual(action["status"], "todo")
            self.assertIn("; verify: ", action["title"])
            self.assertEqual(action["assignee"], "madsbolaris")
            prefix = "Proposed rule" if artifact["id"].endswith("human-decisions") else "Recommended change"
            self.assertEqual(action["quote"], f"{prefix}: {finding()['action']}")
            self.assertNotIn("sourceHeading", action)
            self.assertIn(f"**{prefix}:** {finding()['action']}", version["body"])
            self.assertNotIn("Coverage boundaries", version["body"])
            self.assertNotIn(f"Snapshot `{OTHER}`", version["body"])
            self.assertIn('<Inline id={"session-review-summary"}', version["body"])
            self.assertIn('<Stat id={"follow-ups"}', version["body"])
        self.assertFalse(any(r.FINDING_MARKER in body for _, body in plan))

    def test_retries_do_not_duplicate_versions_or_discussions(self):
        b, values = reports()
        initial = r.publication_plan(values, b, SHA, PROVENANCE, [])
        comments = [bot(body, i + 1) for i, (_, body) in enumerate(initial)]
        self.assertEqual(r.publication_plan(values, b, SHA, PROVENANCE, comments), [])
        changed = copy.deepcopy(values)
        changed[0]["findings"][0]["detail"] = "Changed immutable content"
        with self.assertRaisesRegex(r.ReportError, "immutable"):
            r.publication_plan(changed, b, SHA, PROVENANCE, comments)

    def test_later_run_retains_history_without_duplicate_action_comment(self):
        b, values = reports()
        initial = r.publication_plan(values, b, SHA, PROVENANCE, [])
        comments = [bot(body, i + 1) for i, (_, body) in enumerate(initial)]
        later = {**PROVENANCE, "runId": "124",
                 "runUrl": f"https://github.com/{r.REPOSITORY}/actions/runs/124"}
        plan = r.publication_plan(values, b, SHA, later, comments)
        self.assertEqual(len(plan), 2)
        artifact = r.unique_json(plan[0][1].split("\n")[0][len(r.MARKER):-4])
        self.assertEqual([v["runId"] for v in artifact["versions"]], ["123", "124"])

    def test_human_comment_cannot_impersonate_publisher(self):
        b, values = reports()
        initial = r.publication_plan(values, b, SHA, PROVENANCE, [])
        comments = [bot(body) for _, body in initial]
        for comment in comments:
            comment["user"] = {"login": "someone", "type": "User"}
        self.assertEqual(len(r.publication_plan(values, b, SHA, PROVENANCE, comments)), 2)

    def test_detached_legacy_discussions_are_removed_only_after_reports_publish(self):
        b, values = reports()
        marker = {"headSha": SHA, "artifactId": "session-review.agent-behavior", "taskId": "old"}
        legacy = bot(r.FINDING_MARKER + json.dumps(marker) + " -->", 99)
        writes = []
        def request(path, method="GET", body=None):
            if "comments?" in path:
                return [legacy]
            if method == "GET":
                return pr()
            writes.append((method, path))
        r.publish(1, SHA, values, b, PROVENANCE, request)
        self.assertEqual([method for method, _ in writes], ["POST", "POST", "DELETE"])
        self.assertTrue(writes[-1][1].endswith("/99"))

    def test_empty_human_report_is_concise_and_does_not_invent_decisions(self):
        b = bundle()
        result = r.analyze(b, "human-decisions", lambda _: {
            "reviewed_ids": ["one"], "findings": []})
        text = r.render(result, b, SHA)
        self.assertIn("No reusable repository decisions identified", text)
        self.assertNotIn("Coverage boundaries", text)
        self.assertNotIn("Snapshot", text)
        self.assertNotIn("## ", text)
        self.assertIn("no rules or memories are saved automatically", text)

    def test_head_is_rechecked_before_every_write(self):
        b, values = reports()
        writes = []
        calls = 0
        def request(path, method="GET", body=None):
            nonlocal calls
            if "comments?" in path:
                return []
            if method == "GET":
                calls += 1
                return pr(SHA if calls == 1 else OTHER)
            writes.append(body)
        with self.assertRaisesRegex(r.ReportError, "head changed"):
            r.publish(1, SHA, values, b, PROVENANCE, request)
        self.assertEqual(len(writes), 1)

    def test_closed_and_fork_prs_never_publish(self):
        for candidate in (pr(state="closed"), pr(repository="someone/fork")):
            with self.subTest(candidate=candidate), self.assertRaises(r.ReportError):
                r.current_pr(1, SHA, lambda _: candidate)

    def test_comments_over_size_budget_fail_instead_of_truncating(self):
        with self.assertRaises(r.ReportError):
            r.serialize({"versions": [{"body": "x" * 60000}]})


class TriggerTests(unittest.TestCase):
    def test_cloud_publication_dispatches_only_matching_same_repo_pr(self):
        writes = []
        candidate = pr()
        candidate["head"]["ref"] = "copilot/review"
        def request(path, method="GET", body=None):
            if method == "POST":
                writes.append(body)
            else:
                return [candidate, pr(repository="someone/fork")]
        r.refresh_published("copilot/review", "main", request)
        self.assertEqual(writes, [{"ref": "main", "inputs": {"pull_request": "1"}}])

    def test_cloud_publication_without_open_pr_does_not_invent_target(self):
        calls = []
        def request(path, method="GET", body=None):
            calls.append(method)
            return []
        r.refresh_published("copilot/review", "main", request)
        self.assertEqual(calls, ["GET"])
        with self.assertRaises(r.ReportError):
            r.refresh_published("not-a-cloud-branch", "main", request)

    def test_empty_workflow_run_pr_array_is_resolved_via_api(self):
        event = {"workflow_run": {"event": "pull_request", "conclusion": "success",
                                 "head_repository": {"full_name": r.REPOSITORY},
                                 "head_branch": "feature", "head_sha": SHA, "pull_requests": []}}
        def request(path):
            return [pr()] if "pulls?" in path else pr()
        self.assertEqual(r.resolve_pr(event, request=request)["number"], 1)
        event["workflow_run"]["head_sha"] = OTHER
        with self.assertRaises(r.ReportError):
            r.resolve_pr(event, request=request)

    def test_manual_pr_input_is_validated(self):
        self.assertEqual(r.resolve_pr({}, "1", lambda _: pr())["number"], 1)
        for value in ("0", "1;echo", "-1", "../x"):
            with self.subTest(value=value), self.assertRaises(r.ReportError):
                r.resolve_pr({}, value)

    def test_workflows_are_trusted_pinned_and_nonexecuting(self):
        root = Path(__file__).resolve().parents[1]
        workflow = (root / ".github/workflows/session-reports.yml").read_text()
        signal_workflow = (root / ".github/workflows/session-review-request.yml").read_text()
        self.assertIn("pull_request:", signal_workflow)
        self.assertIn("synchronize", signal_workflow)
        self.assertIn("head.repo.id == github.repository_id", signal_workflow)
        self.assertNotIn("pull_request_target", workflow)
        self.assertNotIn("actions/checkout", signal_workflow)
        for line in workflow.splitlines():
            if "uses:" in line:
                self.assertRegex(line, r"@[a-f0-9]{40} # v\d+\.\d+\.\d+$")
        self.assertIn("ref: ${{ github.workflow_sha }}", workflow)
        self.assertIn("group: session-reports-pr-${{ needs.resolve.outputs.number }}", workflow)
        self.assertNotIn("upload-artifact", workflow)
        publisher = (root / ".github/workflows/trailvault-cloud-publish.yml").read_text()
        self.assertIn("actions: write", publisher)
        self.assertIn("scripts/session_reports.py refresh-published", publisher)

    def test_subprocess_failure_does_not_disclose_raw_output(self):
        with self.assertRaisesRegex(r.ReportError, "raw diagnostics withheld") as error:
            r.execute(["python3", "-c", "import sys; print('sensitive'); sys.exit(1)"])
        self.assertNotIn("sensitive", str(error.exception))


if __name__ == "__main__":
    unittest.main()
