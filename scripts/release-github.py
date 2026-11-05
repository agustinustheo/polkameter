#!/usr/bin/env python3
"""Create/resume a release and reconcile prerelease status on every run."""

import os
from pathlib import Path
import subprocess


def publish(tag, assets):
    prerelease = "-" in tag.split("+", 1)[0]
    exists = subprocess.run(["gh", "release", "view", tag], capture_output=True).returncode == 0
    if exists:
        command = ["gh", "release", "edit", tag, f"--prerelease={str(prerelease).lower()}"]
    else:
        command = ["gh", "release", "create", tag, "--verify-tag", "--title", tag, "--generate-notes"]
        if prerelease:
            command += ["--prerelease"]
    if prerelease:
        command += ["--latest=false"]
    subprocess.run(command, check=True)
    subprocess.run(["gh", "release", "upload", tag, "--clobber", *map(str, assets)], check=True)


if __name__ == "__main__":
    assets = sorted(Path("release-assets").glob("*"))
    if not assets:
        raise SystemExit("No release assets found")
    publish(os.environ["RELEASE_TAG"], assets)
