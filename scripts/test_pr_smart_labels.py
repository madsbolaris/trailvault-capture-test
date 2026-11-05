import copy
import json
import os
import re
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import pr_smart_labels as labels
from session_reports import MODEL, REPOSITORY, ReportError

HEAD = "a" * 40
BASE = "b" * 40


def scores():
    return {"risk": {"score": 35, "why": "The changed parser lacks a malformed-input test."},
            "slop": {"score": 12, "why": "The change reuses existing helpers without duplication."}}


def assessment():
    return {"repository": REPOSITORY, "pullRequest": 1, "headSha": HEAD,
            "baseSha": BASE, "runId": "123", "runAttempt": 1,
            "evaluatorSha": "c" * 40, "model": MODEL, "createdAt": "2026-10-30T12:00:00Z",
            "runUrl": f"https://github.com/{REPOSITORY}/actions/runs/123", **scores()}


def pr():
    return {"number": 1, "state": "open", "head": {"sha": HEAD,
            "repo": {"full_name": REPOSITORY}}, "base": {"sha": BASE}, "changed_files": 1}


class SmartLabelTests(unittest.TestCase):
    def test_exact_scores_and_single_sentence_explanations(self):
        self.assertEqual(labels.validate_scores(scores()), scores())
        for field, value in [("score", True), ("score", -1), ("score", 101), ("score", 3.2),
                             ("why", "First sentence. Second sentence."),
                             ("why", "No terminal punctuation"),
                             ("why", "A link https://example.com is not allowed."),
                             ("why", "An @mention is not allowed."),
                             ("why", "A line\nbreak is not allowed."),
                             ("why", "A table | cell is not allowed.")]:
            with self.subTest(field=field, value=value):
                invalid = scores()
                invalid["risk"][field] = value
                with self.assertRaises(ReportError):
                    labels.validate_scores(invalid)
        with self.assertRaises(ReportError):
            labels.validate_scores({"risk": scores()["risk"]})
        invalid = scores()
        invalid["risk"]["extra"] = True
        with self.assertRaises(ReportError):
            labels.validate_scores(invalid)

    def test_invalid_response_retried_once_without_echoing_model_output(self):
        prompts = []

        def reviewer(prompt):
            prompts.append(prompt)
            return {"secret": "must-not-be-echoed"} if len(prompts) == 1 else scores()

        self.assertEqual(labels.assess("[]", reviewer), scores())
        self.assertEqual(len(prompts), 2)
        self.assertNotIn("must-not-be-echoed", prompts[1])
        self.assertIn("ONLY the supplied PR diff", prompts[0])
        self.assertIn("UNTRUSTED_DIFF", prompts[0])
        with self.assertRaises(ReportError):
            labels.assess("[]", lambda _: {})

    def collect(self, files=None, changed_files=1, stale_base=False):
        calls = 0

        def request(path):
            nonlocal calls
            if "/files?" in path:
                return files if files is not None else [{
                    "filename": "example.py", "status": "modified",
                    "additions": 1, "deletions": 1, "changes": 2,
                    "patch": "@@ -1 +1 @@\n-old\n+new"}]
            calls += 1
            result = pr()
            result["changed_files"] = changed_files
            if stale_base and calls > 1:
                result["base"]["sha"] = "d" * 40
            return result

        return labels.collect_diff(1, HEAD, request)

    def test_complete_diff_only_and_metadata_preserved(self):
        base, diff = self.collect()
        self.assertEqual(base, BASE)
        self.assertEqual(json.loads(diff)[0]["path"], "example.py")
        self.assertEqual(json.loads(diff)[0]["patch"], "@@ -1 +1 @@\n-old\n+new")
        with self.assertRaises(labels.IncompleteDiff):
            self.collect(changed_files=2)
        with self.assertRaises(ReportError):
            self.collect(stale_base=True)

    def test_binary_truncated_and_oversized_diffs_never_scored(self):
        for patch_text in [None, "", "@@ -1 +1 @@\n+new",
                           "@@ -1 +1 @@\n-old\n+" + "x" * labels.MAX_DIFF_BYTES]:
            with self.subTest(patch=type(patch_text).__name__):
                with self.assertRaises(labels.IncompleteDiff):
                    self.collect([{"filename": "file", "status": "modified",
                                   "additions": 1, "deletions": 1, "changes": 2,
                                   "patch": patch_text}])

    def test_metadata_only_rename_is_valid(self):
        _, diff = self.collect([{"filename": "new", "previous_filename": "old",
                                 "status": "renamed", "additions": 0,
                                 "deletions": 0, "changes": 0}])
        self.assertEqual(json.loads(diff)[0]["previousPath"], "old")

    def publish(self, comments=None, current=None):
        writes = []

        def request(path, method="GET", body=None):
            if method != "GET":
                writes.append((path, method, body))
                return {}
            if "/comments?" in path:
                return comments or []
            return current if current is not None else pr()

        labels.publish(1, HEAD, assessment(), request)
        return writes

    def test_one_comment_reused_and_no_native_github_labels_overwritten(self):
        writes = self.publish()
        path, method, payload = writes[0]
        self.assertTrue(path.endswith("/issues/1/comments"))
        self.assertEqual(method, "POST")
        self.assertIn("| Risk | 35/100 |", payload["body"])
        comment = {"id": 9, "user": {"login": "github-actions[bot]", "type": "Bot"},
                   "body": payload["body"]}
        writes = self.publish([comment])
        self.assertEqual(writes[0][1], "PATCH")
        self.assertTrue(writes[0][0].endswith("/issues/comments/9"))
        retained = labels.unique_json(writes[0][2]["body"][len(labels.MARKER):].split(" -->")[0])
        self.assertEqual(len(retained["assessments"]), 1)

    def test_human_spoof_ignored_and_stale_publication_rejected(self):
        spoof = {"user": {"login": "human", "type": "User"},
                 "body": labels.MARKER + "bad"}
        self.assertEqual(self.publish([spoof])[0][1], "POST")
        stale = pr()
        stale["head"]["sha"] = "d" * 40
        with self.assertRaises(ReportError):
            self.publish(current=stale)
        stale = pr()
        stale["base"]["sha"] = "d" * 40
        with self.assertRaises(ReportError):
            self.publish(current=stale)

    def test_duplicate_or_corrupt_bot_publications_rejected(self):
        comment = {"user": {"login": "github-actions[bot]", "type": "Bot"},
                   "body": labels.MARKER + "broken"}
        with self.assertRaises(ReportError):
            self.publish([comment, copy.deepcopy(comment)])
        with self.assertRaises(ReportError):
            self.publish([comment])

    def test_run_publishes_scores_and_failure_is_explicit(self):
        for failure in (None, labels.IncompleteDiff("Incomplete"), ReportError("Model failed")):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                event = Path(directory) / "event.json"
                event.write_text("{}")
                writes = []

                def request(path, method="GET", body=None):
                    if method == "POST":
                        return {"id": 42}
                    writes.append(body)
                    return {}

                with patch.dict(os.environ, {"GITHUB_REPOSITORY": REPOSITORY,
                    "GITHUB_EVENT_PATH": str(event), "REQUESTED_PR": "1", "REQUESTED_SHA": HEAD,
                    "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "1"}), \
                    patch.object(labels, "resolve_pr", return_value=pr()), \
                    patch.object(labels, "collect_diff", side_effect=failure,
                                 return_value=(BASE, "[]")), \
                    patch.object(labels, "git", return_value="c" * 40), \
                    patch.object(labels, "publish") as publish:
                    if failure:
                        with self.assertRaises(ReportError):
                            labels.run(request, lambda _: scores())
                        publish.assert_not_called()
                    else:
                        labels.run(request, lambda _: scores())
                        self.assertEqual(publish.call_args.args[2]["risk"]["score"], 35)
                self.assertEqual(writes[-1]["conclusion"],
                                 "success" if failure is None else
                                 "neutral" if isinstance(failure, labels.IncompleteDiff) else "failure")

    def test_workflow_uses_trusted_code_and_pinned_runtime(self):
        workflow = (Path(__file__).parent.parent / ".github/workflows/pr-smart-labels.yml").read_text()
        self.assertNotIn("pull_request_target", workflow)
        self.assertIn("head_repository.id == github.repository_id", workflow)
        self.assertIn("ref: ${{ github.workflow_sha }}", workflow)
        self.assertIn("secrets.COPILOT_REVIEW_TOKEN", workflow)
        self.assertIn("permissions: {}", workflow)
        self.assertIn("pull-requests: write", workflow.split("  assess:")[1])
        self.assertNotIn("head.sha }}", workflow)
        for action in re.findall(r"uses: ([^\n]+)", workflow):
            self.assertRegex(action, r"@[a-f0-9]{40} # v\d+\.\d+\.\d+$")


if __name__ == "__main__":
    unittest.main()
