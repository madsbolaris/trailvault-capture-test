import base64
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile
import unittest
from unittest.mock import patch

import html_previews as preview
from session_reports import REPOSITORY, ReportError

HEAD = "a" * 40


class HtmlPreviewTests(unittest.TestCase):
    def fixture(self):
        contents = {
            "visualizations/ball.html": b"<canvas></canvas><script src='ball.js'></script>",
            "visualizations/ball.js": b"throw new Error('This must never execute in CI');",
            "visualizations/.private.html": b"private",
            "visualizations/secret.json": b'{"token":"not-for-preview"}',
            "scripts/unrelated.js": b"unrelated",
        }
        tree, blobs = [], {}
        for path, data in contents.items():
            sha = hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest()
            tree.append({"path": path, "type": "blob", "mode": "100644",
                         "size": len(data), "sha": sha})
            blobs[sha] = {"encoding": "base64", "content": base64.b64encode(data).decode()}
        state = {
            "pr": {"number": 36, "state": "open", "changed_files": 1, "head": {
                "sha": HEAD, "repo": {"full_name": REPOSITORY}}},
            "changed": [{"filename": "visualizations/ball.html", "status": "added"}],
            "tree": {"truncated": False, "tree": tree}, "blobs": blobs, "pr_reads": 0,
        }

        def request(path):
            if "/pulls/36/files?" in path:
                return state["changed"]
            if path.endswith("/pulls/36"):
                state["pr_reads"] += 1
                return copy.deepcopy(state["pr"])
            if "/git/trees/" in path:
                return state["tree"]
            if "/git/blobs/" in path:
                return state["blobs"][path.rsplit("/", 1)[-1]]
            raise AssertionError(f"Unexpected API path: {path}")
        return state, request

    def test_exact_head_html_and_assets_are_copied_without_execution(self):
        state, request = self.fixture()
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "preview"
            name = preview.package_preview(36, HEAD, output, request)
            self.assertEqual(name, f"html-page-preview-pr-36-{HEAD}")
            self.assertEqual(
                sorted(str(path.relative_to(output)) for path in output.rglob("*") if path.is_file()),
                ["visualizations/ball.html", "visualizations/ball.js"])
            self.assertIn(b"never execute", (output / "visualizations/ball.js").read_bytes())
            self.assertEqual(state["pr_reads"], 2)

    def test_no_html_changes_skip_upload(self):
        state, request = self.fixture()
        state["changed"] = [{"filename": "README.md", "status": "modified"},
                            {"filename": "old.html", "status": "removed"}]
        state["pr"]["changed_files"] = 2
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "preview"
            self.assertIsNone(preview.package_preview(36, HEAD, output, request))
            self.assertFalse(output.exists())

    def test_unsafe_paths_are_rejected(self):
        for path in ["../page.html", "/page.html", "a/../page.html", "a\\page.html",
                     "a//page.html", "page.html\0"]:
            with self.subTest(path=path), self.assertRaises(ReportError):
                preview.static_path(path)

    def test_incomplete_oversized_symlink_and_corrupt_evidence_fail_before_writing(self):
        for failure in ["truncated", "size", "count", "total", "symlink", "sha", "encoding",
                        "integrity", "missing", "stale"]:
            state, request = self.fixture()
            first = state["tree"]["tree"][0]
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "preview"
                if failure == "truncated":
                    state["tree"]["truncated"] = True
                elif failure == "size":
                    first["size"] = preview.MAX_FILE_BYTES + 1
                elif failure == "count":
                    state["tree"]["tree"] = [first] * (preview.MAX_FILES + 1)
                elif failure == "total":
                    state["tree"]["tree"] = [{**first, "size": preview.MAX_FILE_BYTES}] * 21
                elif failure == "symlink":
                    first["mode"] = "120000"
                elif failure == "sha":
                    first["sha"] = "bad"
                elif failure == "encoding":
                    state["blobs"][first["sha"]]["content"] = "!"
                elif failure == "integrity":
                    state["blobs"][first["sha"]]["content"] = base64.b64encode(
                        b"x" * first["size"]).decode()
                elif failure == "missing":
                    state["tree"]["tree"] = []
                else:
                    original = request

                    def request(path):
                        result = original(path)
                        if path.endswith("/pulls/36") and state["pr_reads"] > 1:
                            result["head"]["sha"] = "b" * 40
                        return result
                with self.assertRaises(ReportError):
                    preview.package_preview(36, HEAD, output, request)
                self.assertFalse(output.exists())

    def test_manual_run_outputs_only_head_bound_artifact_metadata(self):
        _, request = self.fixture()
        with tempfile.TemporaryDirectory() as directory:
            event = Path(directory) / "event.json"
            event.write_text(json.dumps({}))
            outputs = Path(directory) / "outputs"
            with patch.dict(os.environ, {"GITHUB_REPOSITORY": REPOSITORY,
                            "GITHUB_EVENT_PATH": str(event), "GITHUB_OUTPUT": str(outputs),
                            "REQUESTED_PR": "36"}):
                preview.run(Path(directory) / "preview", request)
            self.assertIn("has_preview=true", outputs.read_text())
            self.assertIn(f"archive_name=html-page-preview-pr-36-{HEAD}", outputs.read_text())

    def test_workflow_uses_trusted_code_read_only_token_and_pinned_actions(self):
        workflow = (Path(__file__).parent.parent / ".github/workflows/html-page-preview.yml").read_text()
        self.assertIn("ref: ${{ github.workflow_sha }}", workflow)
        self.assertIn("head_repository.id == github.repository_id", workflow)
        self.assertNotIn("pull_request_target", workflow)
        self.assertNotIn("secrets.", workflow)
        self.assertNotIn("write", workflow)
        for action in re.findall(r"uses: ([^\n]+)", workflow):
            self.assertRegex(action, r"@[a-f0-9]{40} # v\d+\.\d+\.\d+$")


if __name__ == "__main__":
    unittest.main()
