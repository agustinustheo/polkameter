#!/usr/bin/env python3
"""Create or resume a GitHub release: draft, upload every asset, then publish.

A published asset is never replaced. A recovery run keeps the published file
when its rebuild differs (Rust builds and the SBOM are not byte-identical) and
uploads only the assets the release lacks, so the later jobs can still run.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

STABLE = re.compile(r"v([0-9]+)\.([0-9]+)\.([0-9]+)")


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


def previous_stable(tag, tags):
    """The newest stable tag older than tag, so notes after an rc start at the last stable release."""
    base = tuple(map(int, re.match(r"v([0-9]+)\.([0-9]+)\.([0-9]+)", tag).groups()))
    older = [(tuple(map(int, match.groups())), name) for name in tags
             if (match := STABLE.fullmatch(name)) and tuple(map(int, match.groups())) < base]
    return max(older)[1] if older else None


def release_tags(repository):
    refs = gh("api", "--paginate", f"repos/{repository}/git/matching-refs/tags/v", "--jq", ".[].ref",
              capture=True)
    return [ref.removeprefix("refs/tags/") for ref in refs.split()]


def partner(name):
    """An archive's checksum file, or a checksum file's archive."""
    return name.removesuffix(".sha256") if name.endswith(".sha256") else f"{name}.sha256"


def plan_uploads(release, assets):
    """Returns (uploads, kept, conflicts) for local assets against the release's assets.

    kept: published assets whose rebuild differs; the published file stays.
    conflicts: missing assets whose published partner differs from this run's,
    which would publish an archive with a checksum that does not match it.
    """
    remote = {asset["name"]: asset for asset in release["assets"]}
    local = {Path(path).name: sha256(path) for path in assets}
    uploaded = {name: asset.get("digest") for name, asset in remote.items()
                if asset.get("state", "uploaded") == "uploaded"}
    uploads, kept, conflicts = [], [], []
    for path in assets:
        name = Path(path).name
        if name not in uploaded:
            other = partner(name)
            if not release["isDraft"] and other in uploaded and other in local and uploaded[other] != local[other]:
                conflicts.append(name)
            else:
                uploads.append(path)
        elif uploaded[name] == local[name]:
            continue
        elif release["isDraft"]:
            # A draft is not public: nobody can have downloaded it yet.
            uploads.append(path)
        else:
            kept.append(name)
    return uploads, kept, conflicts


def publish(tag, assets, repository):
    prerelease = "-" in tag.split("+", 1)[0]
    flags = ["--latest=false"] if prerelease else []
    release = view(tag)
    if release is None:
        start = previous_stable(tag, release_tags(repository))
        gh("release", "create", tag, "--draft", "--verify-tag", "--title", tag, "--generate-notes",
           *(["--notes-start-tag", start] if start else []),
           *(["--prerelease"] if prerelease else []), *flags)
        release = {"isDraft": True, "assets": []}
    uploads, kept, conflicts = plan_uploads(release, assets)
    if conflicts:
        raise SystemExit(
            f"Release {tag} is published, and these missing assets belong with published files that differ "
            f"from this run's builds: {', '.join(conflicts)}. Uploading them would publish a checksum that "
            "does not match its archive. Re-run only the failed jobs of the original run, which reuses its "
            "built assets, or release a new patch version. See RELEASING.md.")
    if kept:
        print(f"::warning::Release {tag} is already published. Keeping the published {', '.join(kept)}: "
              "this run's rebuild differs, and published assets are never replaced.")
    if uploads:
        # Partial uploads and draft assets may be replaced; published ones never reach here.
        gh("release", "upload", tag, "--clobber", *map(str, uploads))
    gh("release", "edit", tag, f"--prerelease={str(prerelease).lower()}", *flags,
       *(["--draft=false"] if release["isDraft"] else []))


if __name__ == "__main__":
    assets = sorted(Path("release-assets").glob("*"))
    if not assets:
        raise SystemExit("No release assets found")
    publish(os.environ["RELEASE_TAG"], assets, os.environ["GH_REPO"])
