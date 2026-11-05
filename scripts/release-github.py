#!/usr/bin/env python3
"""Create or resume a GitHub release: draft, upload every asset, then publish.

Assets of a published release are never replaced. A recovery run whose rebuilt
asset differs from the published one fails instead of changing a digest that
users may already have pinned.
"""

import hashlib
import json
import os
from pathlib import Path
import subprocess


def gh(*args, capture=False):
    result = subprocess.run(["gh", *args], check=True, capture_output=capture, text=capture)
    return result.stdout if capture else None


def view(tag):
    """The release's state, or None if there is none (drafts included)."""
    result = subprocess.run(["gh", "release", "view", tag, "--json", "isDraft,assets"],
                            capture_output=True, text=True)
    if result.returncode:
        if "release not found" in result.stderr.lower():
            return None
        raise SystemExit(f"Could not read release {tag}: {result.stderr.strip()}")
    return json.loads(result.stdout)


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(chunk)
    return f"sha256:{digest.hexdigest()}"


def plan_uploads(release, assets):
    """Returns (uploads, conflicts) for local assets against the release's assets."""
    remote = {asset["name"]: asset for asset in release["assets"]} if release else {}
    uploads, conflicts = [], []
    for path in assets:
        existing = remote.get(Path(path).name)
        if existing is None or existing.get("state", "uploaded") != "uploaded":
            uploads.append(path)
        elif existing.get("digest") == sha256(path):
            continue
        elif release["isDraft"]:
            # A draft is not public: nobody can have downloaded it yet.
            uploads.append(path)
        else:
            conflicts.append((Path(path).name, existing.get("digest") or "unknown", sha256(path)))
    return uploads, conflicts


def publish(tag, assets):
    prerelease = "-" in tag.split("+", 1)[0]
    flags = ["--latest=false"] if prerelease else []
    release = view(tag)
    if release is None:
        gh("release", "create", tag, "--draft", "--verify-tag", "--title", tag, "--generate-notes",
           *(["--prerelease"] if prerelease else []), *flags)
        release = {"isDraft": True, "assets": []}
    uploads, conflicts = plan_uploads(release, assets)
    if conflicts:
        lines = "\n".join(f"  {name}: published {old}, rebuilt {new}" for name, old, new in conflicts)
        raise SystemExit(
            f"Release {tag} is published and these assets differ from this run's builds:\n{lines}\n"
            "Published assets are never replaced. Re-run only the failed jobs of the original run, "
            "which reuses its built assets, or release a new patch version. See RELEASING.md.")
    if uploads:
        # Partial uploads and draft assets may be replaced; published ones never reach here.
        gh("release", "upload", tag, "--clobber", *map(str, uploads))
    gh("release", "edit", tag, f"--prerelease={str(prerelease).lower()}", *flags,
       *(["--draft=false"] if release["isDraft"] else []))


if __name__ == "__main__":
    assets = sorted(Path("release-assets").glob("*"))
    if not assets:
        raise SystemExit("No release assets found")
    publish(os.environ["RELEASE_TAG"], assets)
