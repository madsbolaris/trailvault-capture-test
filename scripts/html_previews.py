"""Package exact-head static HTML previews without executing PR code."""

import argparse
import base64
import binascii
import hashlib
import os
from pathlib import Path, PurePosixPath

from session_reports import (
    REPOSITORY, SHA, ReportError, api, current_pr, pages, require,
    resolve_pr, unique_json,
)

MAX_FILES = 400
MAX_FILE_BYTES = 1024 * 1024
MAX_TOTAL_BYTES = 20 * 1024 * 1024
STATIC_EXTENSIONS = {
    ".html", ".htm", ".css", ".js", ".mjs", ".svg", ".png", ".jpg",
    ".jpeg", ".gif", ".webp", ".ico", ".woff", ".woff2", ".ttf",
}


def static_path(path):
    require(isinstance(path, str) and path and "\\" not in path and "\0" not in path,
            "Invalid preview path")
    parsed = PurePosixPath(path)
    require(not parsed.is_absolute() and str(parsed) == path
            and all(part not in {".", ".."} for part in parsed.parts),
            "Unsafe preview path")
    return (not any(part.startswith(".") for part in parsed.parts)
            and parsed.suffix.lower() in STATIC_EXTENSIONS)


def package_preview(number, sha, output, request=api):
    pr = current_pr(number, sha, request)
    changed = list(pages(f"repos/{REPOSITORY}/pulls/{number}/files", request))
    require(len(changed) == pr["changed_files"] and len(changed) < 3000,
            "Changed-file evidence is incomplete")
    html_paths = {
        row["filename"] for row in changed
        if row["status"] != "removed" and static_path(row["filename"])
        and PurePosixPath(row["filename"]).suffix.lower() in {".html", ".htm"}
    }
    if not html_paths:
        return None
    tree = request(f"repos/{REPOSITORY}/git/trees/{sha}?recursive=1")
    require(tree.get("truncated") is False, "Preview tree is truncated")
    roots = {PurePosixPath(path).parent for path in html_paths}
    files = []
    for row in tree["tree"]:
        path = row["path"]
        if (row["type"] != "blob" or not static_path(path)
                or not any(PurePosixPath(path).is_relative_to(root) for root in roots)):
            continue
        require(row["mode"] in {"100644", "100755"}, "Preview symlinks are not supported")
        require(SHA.fullmatch(row["sha"]), "Invalid preview blob SHA")
        require(type(row["size"]) is int and 0 <= row["size"] <= MAX_FILE_BYTES,
                "Preview file exceeds its byte limit")
        files.append(row)
    require(html_paths <= {row["path"] for row in files}, "Changed HTML is missing from preview")
    require(len(files) <= MAX_FILES, "Preview contains too many files")
    require(sum(row["size"] for row in files) <= MAX_TOTAL_BYTES, "Preview exceeds total byte limit")
    payloads = []
    for row in files:
        blob = request(f"repos/{REPOSITORY}/git/blobs/{row['sha']}")
        require(blob.get("encoding") == "base64", "Unsupported preview blob encoding")
        try:
            data = base64.b64decode("".join(blob["content"].split()), validate=True)
        except (binascii.Error, ValueError):
            raise ReportError("Invalid preview blob encoding") from None
        require(len(data) == row["size"], "Preview blob size mismatch")
        digest = hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest()
        require(digest == row["sha"], "Preview blob integrity mismatch")
        payloads.append((row["path"], data))
    current_pr(number, sha, request)
    output.mkdir(parents=True, exist_ok=False)
    for path, data in payloads:
        target = output / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    return f"html-page-preview-pr-{number}-{sha}"


def run(output, request=api):
    require(os.environ.get("GITHUB_REPOSITORY") == REPOSITORY, "Unexpected workflow repository")
    event = unique_json(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    pr = resolve_pr(event, os.environ.get("REQUESTED_PR"), request)
    name = package_preview(pr["number"], pr["head"]["sha"], output, request)
    if os.environ.get("GITHUB_OUTPUT"):
        with Path(os.environ["GITHUB_OUTPUT"]).open("a") as stream:
            stream.write(f"has_preview={'true' if name else 'false'}\narchive_name={name or ''}\n")
    print("Packaged exact-head static HTML preview." if name else "No changed HTML pages to preview.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        run(args.output)
    except (ReportError, KeyError, TypeError, ValueError, OSError) as error:
        message = str(error) if isinstance(error, ReportError) else "Invalid preview input or output"
        parser.exit(1, f"HTML preview: {message}\n")


if __name__ == "__main__":
    main()
