#!/usr/bin/env python3
"""Install a release CLI for the composite action, or select its source build."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from urllib.error import URLError
from urllib.parse import quote, urlsplit
from urllib.request import build_opener, HTTPRedirectHandler, Request
import zipfile

REPO = "agustinustheo/polkameter"
TAG = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+")


class Unavailable(Exception):
    """A release or platform asset cannot be resolved."""


class SafeRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        redirected = super().redirect_request(req, fp, code, msg, headers, newurl)
        if redirected is not None and urlsplit(req.full_url)[:2] != urlsplit(newurl)[:2]:
            redirected.remove_header("Authorization")
        return redirected


class Installer:
    def __init__(self):
        self.root = Path(os.environ["GITHUB_ACTION_PATH"])
        self.api = os.environ.get("GITHUB_API_URL", "https://api.github.com").rstrip("/")
        self.opener = build_opener(SafeRedirect())

    def request(self, url, accept="application/vnd.github+json"):
        headers = {"Accept": accept, "User-Agent": "polkameter-action"}
        token = os.environ.get("GITHUB_TOKEN", "")
        # Only send the runner token to the API, never to a release storage host.
        if token and urlsplit(url).netloc == urlsplit(self.api).netloc:
            headers["Authorization"] = f"Bearer {token}"
        try:
            return self.opener.open(Request(url, headers=headers), timeout=30)
        except URLError as error:
            raise Unavailable(f"GitHub request failed: {error}") from error

    def get(self, endpoint):
        with self.request(f"{self.api}/repos/{REPO}/{endpoint}") as response:
            return json.load(response)

    def tag_commit(self, tag):
        obj = self.get(f"git/ref/tags/{quote(tag, safe='')}")["object"]
        # Release tags may be annotated (including nested annotated tags).
        for _ in range(10):
            if obj["type"] == "commit":
                return obj["sha"]
            if obj["type"] != "tag":
                break
            obj = self.get(f"git/tags/{obj['sha']}")["object"]
        raise Unavailable(f"Tag {tag} does not resolve to a commit")

    def checkout_commit(self):
        if not (self.root / ".git").exists():
            return None
        result = subprocess.run(["git", "-C", str(self.root), "rev-parse", "HEAD"],
                                capture_output=True, text=True, check=False)
        return result.stdout.strip() if result.returncode == 0 else None

    def source_version(self):
        # Read only the package version, never a similarly named dependency.
        def version(path, section):
            content = path.read_text().split(f"[{section}]", 1)[1].split("\n[", 1)[0]
            match = re.search(r'^version\s*=\s*"([^"]+)"', content, re.MULTILINE)
            return match[1] if match else None
        return (version(self.root / "crates/cli/Cargo.toml", "package")
                or version(self.root / "Cargo.toml", "workspace.package"))

    def resolve(self, ref, explicit):
        if explicit:
            tag = explicit if explicit.startswith("v") else f"v{explicit}"
            if not TAG.fullmatch(tag):
                raise ValueError("version must be X.Y.Z or vX.Y.Z")
            return self.get(f"releases/tags/{tag}")
        if TAG.fullmatch(ref):
            return self.get(f"releases/tags/{ref}")
        if not re.fullmatch(r"v[0-9]+", ref):
            raise Unavailable("This action ref is not a release tag; use version to select a release")

        commit = self.checkout_commit()
        if not commit:
            try:
                commit = self.tag_commit(ref)
            except Unavailable:
                pass
        if commit:
            # Find the immutable release behind a moving major tag, not /latest.
            page = 1
            while True:
                try:
                    releases = self.get(f"releases?per_page=100&page={page}")
                except Unavailable:
                    break
                for release in releases:
                    tag = release["tag_name"]
                    if not TAG.fullmatch(tag) or tag.split(".")[0] != ref or release["draft"]:
                        continue
                    try:
                        if self.tag_commit(tag) == commit:
                            return release
                    except Unavailable:
                        continue
                if len(releases) < 100:
                    break
                page += 1

        tag = f"v{self.source_version()}"
        if tag.split(".")[0] != ref:
            raise Unavailable("The action package version does not match the moving major tag")
        release = self.get(f"releases/tags/{tag}")
        if commit and self.tag_commit(tag) != commit:
            raise Unavailable("The package release tag does not match the action commit")
        return release

    def install(self, release):
        targets = {
            ("Linux", "X64"): "x86_64-unknown-linux-gnu",
            ("Linux", "ARM64"): "aarch64-unknown-linux-gnu",
            ("macOS", "X64"): "x86_64-apple-darwin",
            ("macOS", "ARM64"): "aarch64-apple-darwin",
            ("Windows", "X64"): "x86_64-pc-windows-msvc",
        }
        target = targets.get((os.environ["RUNNER_OS"], os.environ["RUNNER_ARCH"]))
        if not target:
            raise Unavailable("No release binary for this runner platform")
        tag = release["tag_name"]
        suffix = "zip" if os.environ["RUNNER_OS"] == "Windows" else "tar.gz"
        name = f"polkameter-{tag.removeprefix('v')}-{target}.{suffix}"
        assets = {asset["name"]: asset["url"] for asset in release["assets"]}
        if name not in assets or f"{name}.sha256" not in assets:
            raise Unavailable(f"Release {tag} has no archive and checksum for {target}")
        directory = Path(tempfile.mkdtemp(prefix="polkameter-cli-", dir=os.environ["RUNNER_TEMP"]))
        archive = directory / name
        with self.request(assets[name], "application/octet-stream") as response, archive.open("wb") as out:
            shutil.copyfileobj(response, out)
        with self.request(assets[f"{name}.sha256"], "application/octet-stream") as response:
            checksum = response.read().decode().strip()
        match = re.fullmatch(r"([a-fA-F0-9]{64}) [ *](.+)", checksum)
        with archive.open("rb") as downloaded:
            digest = hashlib.sha256()
            for chunk in iter(lambda: downloaded.read(1024 * 1024), b""):
                digest.update(chunk)
        if not match or match[2] != name or match[1].lower() != digest.hexdigest():
            raise ValueError(f"Checksum verification failed for {name}")

        verify = os.environ.get("VERIFY_ATTESTATION", "")
        if verify == "true" or (verify == "" and shutil.which("gh")):
            if not shutil.which("gh"):
                raise ValueError("Attestation verification requested, but gh is not installed")
            subprocess.run(["gh", "attestation", "verify", str(archive), "--repo", REPO], check=True)

        binary = "polkameter.exe" if suffix == "zip" else "polkameter"
        bindir = directory / "bin"
        bindir.mkdir()
        # Copy only the root binary; do not extract arbitrary paths or symlinks.
        if suffix == "zip":
            with zipfile.ZipFile(archive) as bundle, bundle.open(binary) as src:
                with (bindir / binary).open("wb") as out:
                    shutil.copyfileobj(src, out)
        else:
            with tarfile.open(archive) as bundle:
                member = bundle.getmember(binary)
                if not member.isfile():
                    raise ValueError("The release binary is not a regular file")
                with bundle.extractfile(member) as src, (bindir / binary).open("wb") as out:
                    shutil.copyfileobj(src, out)
        (bindir / binary).chmod(0o755)
        with open(os.environ["GITHUB_PATH"], "a") as output:
            output.write(f"{bindir}\n")
        return tag.removeprefix("v")


def main():
    mode = os.environ.get("INSTALL", "auto")
    if mode not in ("auto", "release", "source"):
        raise ValueError("install must be auto, release or source")
    if os.environ.get("VERIFY_ATTESTATION", "") not in ("", "true", "false"):
        raise ValueError("verify-attestation must be true or false")
    installer = Installer()
    method = "source"
    version = installer.source_version()
    if mode != "source":
        try:
            release = installer.resolve(os.environ.get("ACTION_REF", ""), os.environ.get("VERSION", ""))
            version = installer.install(release)
            method = "release"
        except Unavailable as error:
            if mode == "release":
                raise ValueError(str(error)) from error
            print(f"Using source build: {error}")
    needs_toolchain = method == "source" or bool(os.environ.get("PLUGIN_CRATES", "").strip())
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"install-method={method}\nversion={version}\nneeds-toolchain={str(needs_toolchain).lower()}\n")
    print(f"PolkaMeter {version}: {method}")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, subprocess.CalledProcessError, OSError, KeyError, tarfile.TarError, zipfile.BadZipFile) as error:
        print(f"::error::{error}", file=sys.stderr)
        sys.exit(1)
