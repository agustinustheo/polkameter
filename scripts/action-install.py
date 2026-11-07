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
from urllib.parse import urlsplit
from urllib.request import build_opener, HTTPRedirectHandler, Request
import zipfile

REPO = "agustinustheo/polkameter"
API = "https://api.github.com"
STABLE = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+")
# A release tag, optionally with a SemVer prerelease such as v0.3.0-rc.1.
TAG = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?")
# gh exit status for "authentication required" (gh help exit-codes).
GH_AUTH_REQUIRED = 4


def asset_name(version, target):
    suffix = "zip" if target.endswith("-windows-msvc") else "tar.gz"
    return f"polkameter-{version}-{target}.{suffix}"


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
        # Releases are on github.com: never send another server's runner token there.
        on_github = os.environ.get("GITHUB_SERVER_URL", "https://github.com").rstrip("/") == "https://github.com"
        self.token = os.environ.get("GITHUB_TOKEN", "") if on_github else ""
        self.opener = build_opener(SafeRedirect())

    def request(self, url, accept="application/vnd.github+json"):
        headers = {"Accept": accept, "User-Agent": "polkameter-action"}
        # Only send the runner token to the API, never to a release storage host.
        if self.token and urlsplit(url)[:2] == urlsplit(API)[:2]:
            headers["Authorization"] = f"Bearer {self.token}"
        try:
            return self.opener.open(Request(url, headers=headers), timeout=30)
        except URLError as error:
            raise Unavailable(f"GitHub request failed: {error}") from error

    def get(self, endpoint):
        with self.request(f"{API}/repos/{REPO}/{endpoint}") as response:
            return json.load(response)

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
                raise ValueError("version must be X.Y.Z or vX.Y.Z, optionally with a prerelease such as -rc.1")
        elif TAG.fullmatch(ref):
            tag = ref
        elif re.fullmatch(r"v[0-9]+", ref):
            # Remote action packages have no .git. Never query the moving tag:
            # it may have advanced since the runner downloaded this package.
            # Moving tags only ever select stable releases.
            tag = f"v{self.source_version()}"
            if not STABLE.fullmatch(tag) or tag.split(".")[0] != ref:
                raise Unavailable("The action package version does not match the moving major tag")
        else:
            raise Unavailable("This action ref is not a release tag; use version to select a release")
        release = self.get(f"releases/tags/{tag}")
        if release["tag_name"] != tag or release["draft"]:
            raise ValueError(f"GitHub did not return the requested published release {tag}")
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
        name = asset_name(tag.removeprefix("v"), target)
        assets = {asset["name"]: asset["url"] for asset in release["assets"]}
        if name not in assets or f"{name}.sha256" not in assets:
            raise Unavailable(f"Release {tag} has no archive and checksum for {target}")
        if not shutil.which("gh"):
            raise Unavailable("Build provenance verification requires gh, but gh is not installed")
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

        env = dict(os.environ, GH_HOST="github.com")
        for key in ("GH_TOKEN", "GITHUB_TOKEN"):
            env.pop(key, None)
        if self.token:
            env["GH_TOKEN"] = self.token
        # gh hashes this exact file and verifies the attested subject digest.
        # Pin both the trusted signer and the source tag, so a valid archive
        # from an older release cannot be substituted with its checksum.
        # gh cannot query attestations anonymously, even for a public
        # repository; it exits 4 when it has no github.com credentials.
        verified = subprocess.run(["gh", "attestation", "verify", str(archive),
                                   "--repo", REPO, "--hostname", "github.com",
                                   "--signer-workflow", f"{REPO}/.github/workflows/release.yml",
                                   "--source-ref", f"refs/tags/{tag}", "--digest-alg", "sha256"],
                                  env=env)
        if verified.returncode == GH_AUTH_REQUIRED and not self.token:
            raise Unavailable("Build provenance verification needs gh authenticated to github.com "
                              "(for example gh auth login on the runner)")
        if verified.returncode:
            raise subprocess.CalledProcessError(verified.returncode, verified.args)

        exe = ".exe" if suffix == "zip" else ""
        binary = f"polkameter{exe}"
        bindir = directory / "bin"
        bindir.mkdir()
        # Copy only the root executables; do not extract arbitrary paths or symlinks. The
        # substrate plugin is in archives from v0.3.0.
        for name, required in ((binary, True), (f"polkameter-substrate-plugin{exe}", False)):
            if suffix == "zip":
                with zipfile.ZipFile(archive) as bundle:
                    if name not in bundle.namelist() and not required:
                        continue
                    with bundle.open(name) as src, (bindir / name).open("wb") as out:
                        shutil.copyfileobj(src, out)
            else:
                with tarfile.open(archive) as bundle:
                    if name not in bundle.getnames() and not required:
                        continue
                    member = bundle.getmember(name)
                    if not member.isfile():
                        raise ValueError(f"The release {name} is not a regular file")
                    with bundle.extractfile(member) as src, (bindir / name).open("wb") as out:
                        shutil.copyfileobj(src, out)
            (bindir / name).chmod(0o755)
        with open(os.environ["GITHUB_PATH"], "a") as output:
            output.write(f"{bindir}\n")
        return tag.removeprefix("v")


def report(method, version, reason):
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a") as output:
            output.write(f"PolkaMeter installation: **{method}** (version {version}). {reason}\n\n")


def main():
    mode = os.environ.get("INSTALL", "auto")
    if mode not in ("auto", "release", "source"):
        raise ValueError("install must be auto, release or source")
    installer = Installer()
    method = "source"
    reason = "Source build explicitly requested"
    version = installer.source_version()
    if mode != "source":
        try:
            release = installer.resolve(os.environ.get("ACTION_REF", ""), os.environ.get("VERSION", ""))
            version = installer.install(release)
            method = "release"
            reason = f"Downloaded release v{version}; SHA-256 and release attestation verified"
        except Unavailable as error:
            if mode == "release":
                raise ValueError(str(error)) from error
            reason = str(error)
            print(f"::warning::Using source build: {reason}")
    needs_toolchain = method == "source" or bool(os.environ.get("PLUGIN_CRATES", "").strip())
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"install-method={method}\nversion={version}\nneeds-toolchain={str(needs_toolchain).lower()}\n")
    report(method, version, reason)
    print(f"::notice::PolkaMeter {version}: {method}. {reason}")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, subprocess.CalledProcessError, OSError, KeyError, tarfile.TarError, zipfile.BadZipFile) as error:
        report("failed", "unavailable", str(error))
        print(f"::error::{error}", file=sys.stderr)
        sys.exit(1)
