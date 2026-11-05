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
TAG = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+")


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
        # Distribution is hosted on github.com even for GHES callers. A GHES
        # runner token must never be sent to the public API or to gh.
        public_runner = (os.environ.get("GITHUB_SERVER_URL", "https://github.com").rstrip("/")
                         == "https://github.com"
                         and os.environ.get("GITHUB_API_URL", API).rstrip("/") == API)
        self.token = os.environ.get("GITHUB_TOKEN", "") if public_runner else ""
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
                raise ValueError("version must be X.Y.Z or vX.Y.Z")
        elif TAG.fullmatch(ref):
            tag = ref
        elif re.fullmatch(r"v[0-9]+", ref):
            # Remote action packages have no .git. Never query the moving tag:
            # it may have advanced since the runner downloaded this package.
            tag = f"v{self.source_version()}"
            if not TAG.fullmatch(tag) or tag.split(".")[0] != ref:
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
        verify_attestation = os.environ.get("VERIFY_ATTESTATION", "true") != "false"
        if verify_attestation and not shutil.which("gh"):
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

        if verify_attestation:
            env = dict(os.environ, GH_HOST="github.com")
            for key in ("GH_TOKEN", "GITHUB_TOKEN"):
                env.pop(key, None)
            if self.token:
                env["GH_TOKEN"] = self.token
            # gh hashes this exact file and verifies the attested subject digest.
            # Pin both the trusted signer and the source tag, so a valid archive
            # from an older release cannot be substituted with its checksum.
            subprocess.run(["gh", "attestation", "verify", str(archive),
                            "--repo", REPO, "--hostname", "github.com",
                            "--signer-workflow", f"{REPO}/.github/workflows/release.yml",
                            "--source-ref", f"refs/tags/{tag}", "--digest-alg", "sha256"],
                           env=env, check=True)
        else:
            print("::warning::verify-attestation=false: installing with SHA-256 only; "
                  "release build provenance is not verified")

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


def report(method, version, reason):
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a") as output:
            output.write(f"PolkaMeter installation: **{method}** (version {version}). {reason}\n\n")


def main():
    mode = os.environ.get("INSTALL", "auto")
    if mode not in ("auto", "release", "source"):
        raise ValueError("install must be auto, release or source")
    if os.environ.get("VERIFY_ATTESTATION", "") not in ("", "true", "false"):
        raise ValueError("verify-attestation must be true or false")
    installer = Installer()
    method = "source"
    reason = "Source build explicitly requested"
    version = installer.source_version()
    if mode != "source":
        try:
            release = installer.resolve(os.environ.get("ACTION_REF", ""), os.environ.get("VERSION", ""))
            version = installer.install(release)
            method = "release"
            reason = f"Downloaded release v{version}; SHA-256 verified"
            reason += ("; provenance verification explicitly disabled"
                       if os.environ.get("VERIFY_ATTESTATION") == "false"
                       else "; release workflow and source tag attestation verified")
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
