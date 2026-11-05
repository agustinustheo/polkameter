#!/usr/bin/env python3
"""Publish missing workspace versions in dependency order; safe to resume."""

import argparse
import json
import os
import re
import subprocess
import sys
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen

API = "https://crates.io/api/v1"
# Six attempts, with at most 15 minutes of backoff per operation.
BACKOFF = (30, 60, 120, 240, 450)
TOOLCHAIN = os.environ.get("RUST_VERSION", "1.93.0")


def request_json(url, **kwargs):
    headers = {"User-Agent": "polkameter-release", **kwargs.pop("headers", {})}
    with urlopen(Request(url, headers=headers, **kwargs), timeout=60) as response:
        body = response.read()
        return json.loads(body) if body else None


def retry(operation):
    for attempt in range(len(BACKOFF) + 1):
        try:
            return operation()
        except HTTPError as error:
            if error.code != 429 and error.code < 500:
                raise
            if attempt == len(BACKOFF):
                raise
            delay = BACKOFF[attempt]
            print(f"Registry HTTP {error.code}; retrying in {delay}s", flush=True)
            time.sleep(delay)


def published(name, version):
    try:
        retry(lambda: request_json(f"{API}/crates/{name}/{version}"))
        return True
    except HTTPError as error:
        if error.code == 404:
            return False
        raise


def publish_order(metadata):
    members = set(metadata["workspace_members"])
    packages = {
        p["name"]: p for p in metadata["packages"]
        if p["id"] in members and (p["publish"] is None or "crates-io" in p["publish"])
    }
    ordered, visiting, visited = [], set(), set()

    def visit(name):
        if name in visited:
            return
        if name in visiting:
            raise ValueError(f"Workspace dependency cycle at {name}")
        visiting.add(name)
        for dependency in sorted(packages[name]["dependencies"], key=lambda d: d["name"]):
            # Dev dependencies do not constrain registry publication order.
            if dependency.get("path") and dependency["kind"] != "dev" and dependency["name"] in packages:
                visit(dependency["name"])
        visiting.remove(name)
        visited.add(name)
        ordered.append(packages[name])

    for name in sorted(packages):
        visit(name)
    return ordered


def release_version(metadata, tag):
    if not re.fullmatch(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", tag):
        raise ValueError("Expected a release tag v<major>.<minor>.<patch> (optionally a prerelease)")
    version = tag[1:]
    # All workspace packages inherit workspace.package.version.
    members = set(metadata["workspace_members"])
    if any(p["version"] != version for p in metadata["packages"] if p["id"] in members):
        raise ValueError(f"Tag {tag} does not match the workspace version")
    return version


class Credentials:
    """Refresh short-lived OIDC credentials during long new-crate rate limits."""

    def __init__(self):
        self.token = os.environ.get("CARGO_REGISTRY_TOKEN", "")
        self.trusted = os.environ.get("CRATES_IO_TRUSTED_PUBLISHING") == "true"
        self.created = time.monotonic()
        self.minted = False

    def environment(self):
        if self.trusted and time.monotonic() - self.created >= 600:
            # Same exchange as rust-lang/crates-io-auth-action; that action supplies
            # the initial token and revokes it in its post step.
            url = os.environ["ACTIONS_ID_TOKEN_REQUEST_URL"]
            url += ("&" if "?" in url else "?") + "audience=crates.io"
            jwt = retry(lambda: request_json(url, headers={
                "Authorization": "Bearer " + os.environ["ACTIONS_ID_TOKEN_REQUEST_TOKEN"],
            }))["value"]
            result = retry(lambda: request_json(
                f"{API}/trusted_publishing/tokens", method="POST",
                headers={"Content-Type": "application/json"},
                data=json.dumps({"jwt": jwt}).encode(),
            ))
            token = result["token"]
            print(f"::add-mask::{token}", flush=True)
            self.close()
            self.token, self.created, self.minted = token, time.monotonic(), True
        if not self.token:
            raise ValueError("Configure crates.io Trusted Publishing or CARGO_REGISTRY_TOKEN")
        return {**os.environ, "CARGO_REGISTRY_TOKEN": self.token}

    def close(self):
        if self.minted:
            retry(lambda: request_json(
                f"{API}/trusted_publishing/tokens", method="DELETE",
                headers={"Authorization": f"Bearer {self.token}"},
            ))
            self.minted = False


def publish(package, credentials):
    name, version = package["name"], package["version"]
    for attempt in range(len(BACKOFF) + 1):
        # Check before every attempt: also handles a successful upload whose
        # response was lost, and resuming after another crate failed.
        if published(name, version):
            print(f"Skip {name}@{version}: already on crates.io", flush=True)
            return
        print(f"Publish {name}@{version} (attempt {attempt + 1})", flush=True)
        result = subprocess.run(
            ["cargo", f"+{TOOLCHAIN}", "publish", "--locked", "--registry", "crates-io", "-p", name],
            env=credentials.environment(), text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        )
        print(result.stdout, end="", flush=True)
        if result.returncode == 0:
            return
        if published(name, version):
            return
        limited = re.search(r"429|rate.?limit|too many requests|try again after", result.stdout, re.I)
        if not limited or attempt == len(BACKOFF):
            raise RuntimeError(f"Publishing {name}@{version} failed; dispatch the same tag to resume")
        delay = BACKOFF[attempt]
        print(f"Rate limited; retrying in {delay}s", flush=True)
        time.sleep(delay)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--check-only", action="store_true")
    args = parser.parse_args()
    metadata = json.loads(subprocess.check_output([
        "cargo", f"+{TOOLCHAIN}", "metadata", "--no-deps", "--format-version", "1", "--locked",
    ]))
    version = release_version(metadata, args.tag)
    packages = publish_order(metadata)
    print("Publication order: " + ", ".join(p["name"] for p in packages), flush=True)
    if args.check_only:
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as output:
                output.write(f"version={version}\nmajor={version.split('.')[0]}\n")
        return
    credentials = Credentials()
    try:
        for package in packages:
            publish(package, credentials)
    finally:
        credentials.close()


if __name__ == "__main__":
    try:
        main()
    except (ValueError, RuntimeError, HTTPError) as error:
        print(f"Error: {error}", file=sys.stderr)
        sys.exit(1)
