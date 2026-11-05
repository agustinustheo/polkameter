#!/usr/bin/env python3
"""Promote stable image aliases; callers must serialize this across the repo."""

import argparse
import json
import os
import re
import subprocess


def stable_version(version):
    if not re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:\+[0-9A-Za-z.-]+)?", version):
        raise ValueError(f"Expected stable SemVer, got {version!r}")
    return tuple(map(int, version.split("+", 1)[0].split(".")))


def aliases(version):
    if "-" in version.split("+", 1)[0]:
        return []
    major, minor, _patch = stable_version(version)
    return ["latest", str(major), f"{major}.{minor}"]


def inspect(reference):
    result = subprocess.run(["skopeo", "inspect", "docker://" + reference], text=True, capture_output=True)
    if result.returncode:
        # Fail closed on auth, network or malformed responses, not just missing tags.
        if re.search(r"manifest unknown|name unknown", result.stderr, re.I):
            return None
        raise RuntimeError(f"Cannot inspect {reference}: {result.stderr}")
    return json.loads(result.stdout)


def existing_version(image, version, revision):
    current = inspect(f"{image}:{version}")
    if current is None:
        return False
    labels = current.get("Labels", {})
    if (labels.get("org.opencontainers.image.version") != version
            or labels.get("org.opencontainers.image.revision") != revision):
        raise ValueError("Existing immutable version image does not match this release")
    return True


def promote(image, version):
    tags = aliases(version)
    if not tags:
        print(f"Skip aliases for prerelease {version}")
        return
    source = inspect(f"{image}:{version}")
    if source is None or source.get("Labels", {}).get("org.opencontainers.image.version") != version:
        raise ValueError("Version image missing or has an unexpected version label")
    digest = source["Digest"]
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise ValueError("Invalid image digest")
    for tag in tags:
        current = inspect(f"{image}:{tag}")
        if current is not None:
            current_version = current.get("Labels", {}).get("org.opencontainers.image.version", "")
            if stable_version(current_version) >= stable_version(version):
                print(f"Keep :{tag} at {current_version}")
                continue
        print(f"Promote :{tag} to {version} ({digest})")
        subprocess.run(["docker", "buildx", "imagetools", "create", "--tag", f"{image}:{tag}", f"{image}@{digest}"], check=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version")
    parser.add_argument("--image", default="ghcr.io/agustinustheo/polkameter")
    parser.add_argument("--check-existing", metavar="REVISION")
    args = parser.parse_args()
    if args.check_existing:
        exists = existing_version(args.image, args.version, args.check_existing)
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"exists={str(exists).lower()}\n")
    else:
        promote(args.image, args.version)
