#!/usr/bin/env python3
"""Check the release tag against the workspace version, then publish the crates that are missing."""

import argparse
import json
import os
import re
import subprocess
import sys
from urllib.error import HTTPError
from urllib.request import Request, urlopen

TOOLCHAIN = os.environ.get("RUST_VERSION", "1.93.0")
# The SDK first: polkameter depends on it.
CRATES = ("polkameter-plugin-sdk", "polkameter")


def release_version(metadata, tag):
    if not re.fullmatch(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", tag):
        raise ValueError("Expected a release tag v<major>.<minor>.<patch> (optionally a prerelease)")
    version = tag[1:]
    # All workspace packages inherit workspace.package.version.
    members = set(metadata["workspace_members"])
    if any(p["version"] != version for p in metadata["packages"] if p["id"] in members):
        raise ValueError(f"Tag {tag} does not match the workspace version")
    return version


def published(name, version):
    request = Request(f"https://crates.io/api/v1/crates/{name}/{version}",
                      headers={"User-Agent": "polkameter-release"})
    try:
        with urlopen(request, timeout=60):
            return True
    except HTTPError as error:
        if error.code == 404:
            return False
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--check-only", action="store_true")
    args = parser.parse_args()
    metadata = json.loads(subprocess.check_output([
        "cargo", f"+{TOOLCHAIN}", "metadata", "--no-deps", "--format-version", "1", "--locked",
    ]))
    version = release_version(metadata, args.tag)
    if args.check_only:
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as output:
                output.write(f"version={version}\nmajor={version.split('.')[0]}\n")
        return
    for name in CRATES:
        # A re-run skips the crates that the failed run already published.
        if published(name, version):
            print(f"Skip {name}@{version}: already on crates.io", flush=True)
            continue
        subprocess.run(["cargo", f"+{TOOLCHAIN}", "publish", "--locked", "-p", name], check=True)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, HTTPError, subprocess.CalledProcessError) as error:
        print(f"Error: {error}", file=sys.stderr)
        sys.exit(1)
