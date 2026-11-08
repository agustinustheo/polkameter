#!/usr/bin/env python3
"""Offline tests for release packaging, installation and resumable publishing."""

import functools
import hashlib
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
import threading
import unittest
from unittest.mock import Mock, patch
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


publisher = load("publisher", "publish-crates.py")
packager = load("packager", "package-cli.py")
promoter = load("promoter", "promote-container.py")
releaser = load("releaser", "release-github.py")
TARGETS = (
    "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin", "aarch64-apple-darwin", "x86_64-pc-windows-msvc",
)


class ReleaseHandler(SimpleHTTPRequestHandler):
    def do_HEAD(self):
        if getattr(self.server, "error_status", None):
            self.send_error(self.server.error_status)
        elif self.path == "/latest":
            self.send_response(302)
            self.send_header("Location", "/tag/" + getattr(self.server, "latest_tag", "v0.2.0"))
            self.end_headers()
        else:
            super().do_HEAD()

    def do_GET(self):
        status = getattr(self.server, "error_status", None)
        if status:
            self.send_error(status)
        else:
            super().do_GET()

    def log_message(self, *_args):
        pass


class InstallerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory()
        cls.root = Path(cls.temporary.name)
        cls.assets = cls.root / "download/v0.2.0"
        cls.assets.mkdir(parents=True)
        (cls.root / "tag").mkdir()
        (cls.root / "tag/v0.2.0").write_text("fixture release")
        binary = cls.root / "binary"
        binary.write_text("#!/bin/sh\nprintf 'fixture CLI 0.2.0\\n'\n")
        for target in TARGETS:
            packager.package("0.2.0", target, binary, cls.assets)
        cls.tools = cls.root / "tools"
        cls.tools.mkdir()
        (cls.tools / "uname").write_text('#!/bin/sh\ncase "$1" in -s) echo "$TEST_OS";; -m) echo "$TEST_ARCH";; esac\n')
        (cls.tools / "uname").chmod(0o755)
        (cls.tools / "gh").write_text('#!/bin/sh\nprintf "%s\\n" "$*" > "$TEST_GH_LOG"\nexit "${TEST_GH_EXIT:-0}"\n')
        (cls.tools / "gh").chmod(0o755)
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(ReleaseHandler, directory=str(cls.root)))
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base = f"http://127.0.0.1:{cls.server.server_port}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join()
        cls.temporary.cleanup()

    def run_installer(self, version="v0.2.0", **extra):
        self.destination = self.root / self.id().split(".")[-1] / "bin with spaces"
        env = {
            **os.environ, "PATH": f"{self.tools}:{os.environ['PATH']}",
            "POLKAMETER_RELEASE_BASE_URL": self.base,
            "POLKAMETER_INSTALL_DIR": str(self.destination),
            "POLKAMETER_VERIFY_ATTESTATION": "0",
            "TEST_OS": "Linux", "TEST_ARCH": "x86_64",
            "TEST_GH_LOG": str(self.destination.parent / "gh.log"), **extra,
        }
        self.destination.parent.mkdir(parents=True, exist_ok=True)
        return subprocess.run(["sh", str(ROOT / "install.sh"), version], env=env, text=True, capture_output=True)

    def assert_installed(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        binary = self.destination / "polkameter"
        self.assertTrue(os.access(binary, os.X_OK))
        self.assertEqual(subprocess.check_output([binary], text=True), "fixture CLI 0.2.0\n")
        self.assertEqual(sorted(p.name for p in self.destination.iterdir()), ["polkameter"])

    def test_all_posix_targets(self):
        for system, arch in (("Linux", "x86_64"), ("Linux", "aarch64"), ("Darwin", "x86_64"), ("Darwin", "arm64")):
            with self.subTest(system=system, arch=arch):
                self.assert_installed(self.run_installer(TEST_OS=system, TEST_ARCH=arch))

    def test_latest_redirect(self):
        self.assert_installed(self.run_installer("latest"))

    def test_unprefixed_version(self):
        self.assert_installed(self.run_installer("0.2.0"))

    def test_default_install_directory(self):
        home = self.root / "fake-home"
        result = self.run_installer(HOME=str(home), POLKAMETER_INSTALL_DIR="")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((home / ".local/bin/polkameter").is_file())

    def test_attestation_opt_in(self):
        self.assert_installed(self.run_installer(POLKAMETER_VERIFY_ATTESTATION="1"))
        log = (self.destination.parent / "gh.log").read_text()
        self.assertIn("attestation verify", log)
        self.assertIn("--repo agustinustheo/polkameter", log)
        self.assertIn("--signer-workflow agustinustheo/polkameter/.github/workflows/release.yml", log)
        self.assertIn("--source-ref refs/tags/v0.2.0", log)

    def test_attestation_failure(self):
        self.assertNotEqual(self.run_installer(POLKAMETER_VERIFY_ATTESTATION="1", TEST_GH_EXIT="1").returncode, 0)
        self.assertFalse((self.destination / "polkameter").exists())

    def test_unauthenticated_gh_falls_back_to_checksum_in_auto(self):
        # gh exits 4 when it has no github.com credentials, as on a GitHub-hosted
        # runner step without GH_TOKEN; anonymous verification is not possible.
        for setting in ("", "auto"):
            with self.subTest(setting=setting):
                result = self.run_installer(POLKAMETER_VERIFY_ATTESTATION=setting, TEST_GH_EXIT="4")
                self.assert_installed(result)
                self.assertIn("gh is not authenticated to github.com; only the checksum was verified", result.stderr)
                self.assertIn("--hostname github.com", (self.destination.parent / "gh.log").read_text())

    def test_unauthenticated_gh_fails_when_attestation_required(self):
        result = self.run_installer(POLKAMETER_VERIFY_ATTESTATION="1", TEST_GH_EXIT="4")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("gh must be authenticated to github.com", result.stderr)
        self.assertFalse((self.destination / "polkameter").exists())

    def test_failed_verification_is_fatal_in_auto(self):
        result = self.run_installer(POLKAMETER_VERIFY_ATTESTATION="auto", TEST_GH_EXIT="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("attestation verification failed (gh exit 1)", result.stderr)
        # gh signed in with an expired or invalid token also exits 1.
        self.assertIn("credentials are expired or invalid, run gh auth login or set a valid GH_TOKEN", result.stderr)
        self.assertIn("POLKAMETER_VERIFY_ATTESTATION=0", result.stderr)
        self.assertFalse((self.destination / "polkameter").exists())

    def test_attestation_automatic(self):
        self.assert_installed(self.run_installer(POLKAMETER_VERIFY_ATTESTATION=""))
        self.assertTrue((self.destination.parent / "gh.log").exists())

    def test_attestation_opt_out(self):
        self.assert_installed(self.run_installer())
        self.assertFalse((self.destination.parent / "gh.log").exists())

    def test_no_gh_notice_and_required_mode(self):
        tools = self.root / "without-gh"
        tools.mkdir(exist_ok=True)
        # Expose every command on the real PATH except gh, so the test does not depend on
        # which helpers (gzip, sha256sum, ...) tar and the installer need on each platform.
        for directory in os.environ["PATH"].split(os.pathsep):
            if not os.path.isdir(directory):
                continue
            for entry in os.listdir(directory):
                link = tools / entry
                source = os.path.join(directory, entry)
                if entry in ("gh", "uname") or link.exists() or not os.access(source, os.X_OK):
                    continue
                link.symlink_to(source)
        (tools / "uname").symlink_to(self.tools / "uname")
        result = self.run_installer(PATH=str(tools), POLKAMETER_VERIFY_ATTESTATION="auto")
        self.assert_installed(result)
        self.assertIn("gh is unavailable; only the checksum was verified", result.stderr)
        result = self.run_installer(PATH=str(tools), POLKAMETER_VERIFY_ATTESTATION="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("gh is required", result.stderr)

    def test_checksum_missing_newline(self):
        checksum = self.assets / "polkameter-0.2.0-x86_64-unknown-linux-gnu.tar.gz.sha256"
        original = checksum.read_text()
        try:
            checksum.write_text(original.rstrip("\n"))
            result = self.run_installer()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("invalid checksum file", result.stderr)
            self.assertFalse((self.destination / "polkameter").exists())
        finally:
            checksum.write_text(original)

    def test_missing_old_archive_message(self):
        result = self.run_installer("0.1.0")
        self.assertIn("v0.1.0 has no prebuilt CLI archive; install with cargo install --locked polkameter, or pick a newer version", result.stderr)

    def test_github_errors(self):
        for status in (403, 429):
            with self.subTest(status=status):
                self.server.error_status = status
                try:
                    result = self.run_installer()
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("rate limit or access denied", result.stderr)
                finally:
                    self.server.error_status = None

    def test_latest_github_errors(self):
        for status, message in ((403, "rate limit or access denied"), (429, "rate limit or access denied"), (404, "no latest stable release was found")):
            with self.subTest(status=status):
                self.server.error_status = status
                try:
                    result = self.run_installer("latest")
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(message, result.stderr)
                finally:
                    self.server.error_status = None

    def test_missing_checksum_message(self):
        checksum = self.assets / "polkameter-0.2.0-x86_64-unknown-linux-gnu.tar.gz.sha256"
        original = checksum.read_bytes()
        try:
            checksum.unlink()
            self.assertIn("GitHub returned 404", self.run_installer().stderr)
        finally:
            checksum.write_bytes(original)

    def test_latest_rejects_prerelease(self):
        (self.root / "tag/v0.3.0-rc.1").write_text("prerelease")
        self.server.latest_tag = "v0.3.0-rc.1"
        try:
            result = self.run_installer("latest")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("latest resolved to a prerelease", result.stderr)
        finally:
            self.server.latest_tag = "v0.2.0"

    def test_explicit_prerelease(self):
        assets = self.root / "download/v0.3.0-rc.1"
        assets.mkdir(exist_ok=True)
        packager.package("0.3.0-rc.1", TARGETS[0], self.root / "binary", assets)
        self.assert_installed(self.run_installer("0.3.0-rc.1", POLKAMETER_VERIFY_ATTESTATION="auto"))
        self.assertIn("--source-ref refs/tags/v0.3.0-rc.1", (self.destination.parent / "gh.log").read_text())

    def test_bundled_plugin_is_installed(self):
        assets = self.root / "download/v0.3.0"
        assets.mkdir(exist_ok=True)
        plugin = self.root / "polkameter-substrate-plugin"
        plugin.write_text("#!/bin/sh\nprintf 'fixture plugin\\n'\n")
        packager.package("0.3.0", TARGETS[0], self.root / "binary", assets, [plugin])
        result = self.run_installer("0.3.0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sorted(p.name for p in self.destination.iterdir()),
                         ["polkameter", "polkameter-substrate-plugin"])
        self.assertEqual(subprocess.check_output([self.destination / "polkameter-substrate-plugin"], text=True),
                         "fixture plugin\n")
        self.assertIn("Installed the substrate plugin", result.stdout)

    def test_checksum_mismatch_preserves_existing_install(self):
        checksum = self.assets / "polkameter-0.2.0-x86_64-unknown-linux-gnu.tar.gz.sha256"
        original = checksum.read_text()
        self.run_installer()
        binary = self.destination / "polkameter"
        original_binary = binary.read_bytes()
        try:
            checksum.write_text("0" * 64 + "  " + checksum.name.removesuffix(".sha256") + "\n")
            result = self.run_installer()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("SHA-256 verification failed", result.stderr)
            self.assertEqual(binary.read_bytes(), original_binary)
        finally:
            checksum.write_text(original)

    def test_missing_release(self):
        self.assertNotEqual(self.run_installer("9.9.9").returncode, 0)
        self.assertFalse((self.destination / "polkameter").exists())

    def test_invalid_version(self):
        self.assertNotEqual(self.run_installer("../../malicious").returncode, 0)

    def test_unsupported_os(self):
        self.assertNotEqual(self.run_installer(TEST_OS="FreeBSD").returncode, 0)

    def test_unsupported_arch(self):
        self.assertNotEqual(self.run_installer(TEST_ARCH="riscv64").returncode, 0)

    def test_archive_contract(self):
        for target in TARGETS:
            windows = "windows" in target
            name = f"polkameter-0.2.0-{target}.{'zip' if windows else 'tar.gz'}"
            archive = self.assets / name
            if windows:
                with zipfile.ZipFile(archive) as bundle:
                    members = bundle.namelist()
            else:
                with tarfile.open(archive) as bundle:
                    members = bundle.getnames()
                    self.assertEqual(bundle.getmember("polkameter").mode & 0o777, 0o755)
            self.assertEqual(sorted(members), ["LICENSE", "README.md", "polkameter.exe" if windows else "polkameter"])
            self.assertEqual(archive.with_name(name + ".sha256").read_text(), f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {name}\n")


    def test_archive_contract_with_plugin(self):
        plugin = self.root / "polkameter-substrate-plugin"
        plugin.write_text("plugin")
        for target in TARGETS:
            windows = "windows" in target
            output = self.root / "with-plugin" / target
            archive = packager.package("0.3.0", target, self.root / "binary", output, [plugin])
            if windows:
                with zipfile.ZipFile(archive) as bundle:
                    members = bundle.namelist()
            else:
                with tarfile.open(archive) as bundle:
                    members = bundle.getnames()
                    self.assertEqual(bundle.getmember("polkameter-substrate-plugin").mode & 0o777, 0o755)
            suffix = ".exe" if windows else ""
            self.assertEqual(sorted(members), sorted(["LICENSE", "README.md", f"polkameter{suffix}",
                                                      f"polkameter-substrate-plugin{suffix}"]))
        with self.assertRaises(ValueError):
            packager.package("0.3.0", TARGETS[0], self.root / "binary", self.root / "clash", [self.root / "LICENSE"])


class PublisherTests(unittest.TestCase):
    @staticmethod
    def metadata():
        def package(name, dependencies=(), publish=None):
            return {"id": name, "name": name, "version": "0.2.0", "publish": publish, "dependencies": [
                {"name": dependency, "rename": "alias", "kind": None, "path": "/local"} for dependency in dependencies
            ]}
        return {"workspace_members": ["cli", "lib", "example"], "packages": [
            package("cli", ["lib"]), package("lib"), package("example", ["cli"], []),
        ]}

    def test_dependency_order_excludes_private(self):
        self.assertEqual([p["name"] for p in publisher.publish_order(self.metadata())], ["lib", "cli"])

    def test_dependency_cycle(self):
        metadata = self.metadata()
        metadata["packages"][1]["dependencies"] = [{"name": "cli", "kind": "build", "path": "/local"}]
        with self.assertRaises(ValueError):
            publisher.publish_order(metadata)

    def test_tag_mismatch(self):
        with self.assertRaises(ValueError):
            publisher.release_version(self.metadata(), "v0.3.0")


FAKE_GH = r'''
import hashlib, json, os, sys
state_path, log_path = os.environ["FAKE_GH_STATE"], os.environ["FAKE_GH_LOG"]
state = json.load(open(state_path))
args = sys.argv[1:]
with open(log_path, "a") as log:
    log.write(json.dumps(args) + "\n")
if args[0] == "api":
    assert args[1:] == ["--paginate", "repos/owner/repo/git/matching-refs/tags/v", "--jq", ".[].ref"], args
    print("".join(f"refs/tags/{tag}\n" for tag in state.get("tags", [])), end="")
    sys.exit(0)
if os.environ.get("FAKE_GH_VIEW_ERROR") and args[:2] == ["release", "view"]:
    sys.stderr.write("HTTP 502: Bad Gateway\n")
    sys.exit(1)
release = state.get("release")
command = args[1]
if command == "view":
    if release is None:
        sys.stderr.write("release not found\n")
        sys.exit(1)
    print(json.dumps({"isDraft": release["isDraft"], "assets": [
        {"name": name, "digest": digest, "state": "uploaded"} for name, digest in release["assets"].items()]}))
elif command == "create":
    assert release is None and "--draft" in args
    state["release"] = {"isDraft": True, "prerelease": "--prerelease" in args, "assets": {}}
elif command == "upload":
    assert release is not None and "--clobber" in args
    for path in args[4:]:
        name = os.path.basename(path)
        if name in release["assets"] and not release["isDraft"]:
            state.setdefault("replaced_published", []).append(name)
        release["assets"][name] = "sha256:" + hashlib.sha256(open(path, "rb").read()).hexdigest()
elif command == "edit":
    for arg in args[3:]:
        if arg.startswith("--prerelease="):
            release["prerelease"] = arg.endswith("true")
        if arg == "--draft=false":
            release["isDraft"] = False
json.dump(state, open(state_path, "w"))
'''


class ReleaseTests(unittest.TestCase):
    ARCHIVE = "polkameter-0.3.0-x86_64-unknown-linux-gnu.tar.gz"

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.work = Path(directory.name)
        bin_dir = self.work / "bin"
        bin_dir.mkdir()
        (bin_dir / "gh").write_text(f"#!{sys.executable}\n{FAKE_GH}")
        (bin_dir / "gh").chmod(0o755)
        self.assets = []
        for name in (self.ARCHIVE, self.ARCHIVE + ".sha256", "polkameter.spdx.json"):
            path = self.work / name
            path.write_bytes(name.encode())
            self.assets.append(path)
        self.state = self.work / "state.json"
        self.log = self.work / "log"
        self.environment = patch.dict(os.environ, PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                                      FAKE_GH_STATE=str(self.state), FAKE_GH_LOG=str(self.log))
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.set_release(None)

    def set_release(self, release, tags=()):
        self.state.write_text(json.dumps({"release": release, "tags": list(tags)}))

    def publish(self, tag="v0.3.0"):
        releaser.publish(tag, self.assets, "owner/repo")

    def digest(self, path):
        return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()

    def published(self, **changes):
        assets = {p.name: self.digest(p) for p in self.assets}
        assets.update(changes)
        return {"isDraft": False, "prerelease": False, "assets": {k: v for k, v in assets.items() if v}}

    def calls(self):
        return [json.loads(line)[1] for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def commands(self, name):
        return [json.loads(line) for line in self.log.read_text().splitlines() if json.loads(line)[1] == name]

    def uploaded(self):
        return sorted(Path(p).name for p in self.commands("upload")[0][4:])

    def final(self):
        return json.loads(self.state.read_text())

    def test_new_release_is_drafted_uploaded_then_published(self):
        for tag in ("v0.3.0", "v0.3.0-rc.1"):
            with self.subTest(tag=tag):
                self.set_release(None)
                self.log.unlink(missing_ok=True)
                self.publish(tag)
                self.assertEqual(self.calls(), ["view", "--paginate", "create", "upload", "edit"])
                create, edit = self.commands("create")[0], self.commands("edit")[0]
                self.assertIn("--draft", create)
                self.assertIn("--verify-tag", create)
                self.assertIn("--draft=false", edit)
                state = self.final()
                self.assertFalse(state["release"]["isDraft"])
                self.assertEqual(state["release"]["assets"], {p.name: self.digest(p) for p in self.assets})
                if "-" in tag:
                    self.assertIn("--prerelease", create)
                    self.assertIn("--latest=false", create)
                    self.assertIn("--prerelease=true", edit)
                    self.assertIn("--latest=false", edit)
                else:
                    self.assertNotIn("--prerelease", create)
                    self.assertIn("--prerelease=false", edit)
                    self.assertNotIn("--latest=false", edit)

    def test_draft_recovery_replaces_changed_and_uploads_missing_assets(self):
        self.set_release({"isDraft": True, "prerelease": False, "assets": {self.ARCHIVE: "sha256:" + "0" * 64}})
        self.publish()
        self.assertEqual(self.uploaded(), sorted(p.name for p in self.assets))
        self.assertFalse(self.final()["release"]["isDraft"])
        self.assertNotIn("replaced_published", self.final())

    def test_published_recovery_skips_identical_assets_and_reconciles_flags(self):
        release = self.published()
        release["prerelease"] = True
        self.set_release(release)
        self.publish()
        self.assertEqual(self.calls(), ["view", "edit"])
        self.assertNotIn("--draft=false", self.commands("edit")[0])
        self.assertFalse(self.final()["release"]["prerelease"])

    def test_published_recovery_uploads_only_missing_assets(self):
        self.set_release(self.published(**{"polkameter.spdx.json": None}))
        self.publish()
        self.assertEqual(self.uploaded(), ["polkameter.spdx.json"])
        self.assertNotIn("replaced_published", self.final())

    def test_unreadable_release_fails_without_creating_one(self):
        with patch.dict(os.environ, FAKE_GH_VIEW_ERROR="1"), self.assertRaises(SystemExit) as raised:
            self.publish()
        self.assertIn("Could not read release", str(raised.exception))
        self.assertEqual(self.calls(), ["view"])

    def test_tag_ref_guard_runs_before_builds(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        guard = workflow.split("Require the release tag as the workflow ref for provenance", 1)[1].split("run: |\n", 1)[1].split("      #", 1)[0]
        for ref, code in (("refs/tags/v0.3.0", 0), ("refs/heads/main", 1), ("refs/tags/v0.2.0", 1)):
            with self.subTest(ref=ref):
                result = subprocess.run(["bash", "-c", guard], env={**os.environ, "GITHUB_REF": ref, "RELEASE_TAG": "v0.3.0"}, capture_output=True)
                self.assertEqual(result.returncode, code)

    def test_every_action_is_pinned_to_a_commit_with_version_comment(self):
        files = [ROOT / "action.yml", *sorted((ROOT / ".github").rglob("*.yml"))]
        for path in files:
            for line in path.read_text().splitlines():
                match = re.match(r"\s*(?:- )?uses: (\S+)(.*)", line)
                if not match or match[1].startswith("./"):
                    continue
                with self.subTest(path=path.name, uses=match[1]):
                    self.assertRegex(match[1], r"^[\w.-]+/[\w./-]+@[0-9a-f]{40}$")
                    self.assertRegex(match[2], r"^ # \S+")

    def test_container_uses_init_and_non_root_user(self):
        dockerfile = (ROOT / "Dockerfile").read_text()
        self.assertIn('ENTRYPOINT ["/usr/bin/tini", "--", "polkameter"]', dockerfile)
        self.assertIn("STOPSIGNAL SIGINT", dockerfile)
        self.assertIn("USER 10001:10001", dockerfile)


class ContainerTests(unittest.TestCase):
    def test_numeric_semver_order(self):
        self.assertGreater(promoter.stable_version("0.10.0"), promoter.stable_version("0.9.9"))
        self.assertGreater(promoter.stable_version("1.0.0"), promoter.stable_version("0.99.99"))
        self.assertEqual(promoter.stable_version("1.2.3+build"), (1, 2, 3))
        for invalid in ("", "0.3.0-rc.1", "01.2.3", "latest"):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                promoter.stable_version(invalid)

    def test_prerelease_no_aliases(self):
        with patch.object(promoter, "inspect") as inspect:
            promoter.promote("image", "0.3.0-rc.1")
            inspect.assert_not_called()
        self.assertEqual(promoter.aliases("0.3.0"), ["latest", "0", "0.3"])

    @staticmethod
    def image(version, revision="commit"):
        return {"Digest": "sha256:" + "a" * 64, "Labels": {"org.opencontainers.image.version": version, "org.opencontainers.image.revision": revision}}

    def test_each_alias_compared_separately(self):
        with patch.object(promoter, "inspect", side_effect=[self.image("0.3.1"), self.image("1.0.0"), self.image("0.4.0"), self.image("0.3.0")]), patch.object(promoter.subprocess, "run") as run:
            promoter.promote("image", "0.3.1")
            run.assert_called_once_with(["docker", "buildx", "imagetools", "create", "--tag", "image:0.3", "image@sha256:" + "a" * 64], check=True)

    def test_equal_and_newer_aliases_preserved(self):
        for current in ("0.3.0", "0.10.0"):
            with patch.object(promoter, "inspect", side_effect=[self.image("0.3.0")] + [self.image(current)] * 3), patch.object(promoter.subprocess, "run") as run:
                promoter.promote("image", "0.3.0")
                run.assert_not_called()

    def test_missing_aliases_created(self):
        with patch.object(promoter, "inspect", side_effect=[self.image("0.3.0"), None, None, None]), patch.object(promoter.subprocess, "run") as run:
            promoter.promote("image", "0.3.0")
            self.assertEqual(run.call_count, 3)

    def test_unknown_alias_label_fails_closed(self):
        with patch.object(promoter, "inspect", side_effect=[self.image("0.3.0"), self.image("")]), self.assertRaises(ValueError):
            promoter.promote("image", "0.3.0")

    def test_inspect_missing_vs_error(self):
        with patch.object(promoter.subprocess, "run", return_value=Mock(returncode=1, stderr="manifest unknown")):
            self.assertIsNone(promoter.inspect("image:0"))
        for message in ("unauthorized", "connection timeout", "429"):
            with patch.object(promoter.subprocess, "run", return_value=Mock(returncode=1, stderr=message)), self.assertRaises(RuntimeError):
                promoter.inspect("image:0")

    def test_immutable_version_check(self):
        with patch.object(promoter, "inspect", return_value=None):
            self.assertFalse(promoter.existing_version("image", "0.3.0", "commit"))
        with patch.object(promoter, "inspect", return_value=self.image("0.3.0")):
            self.assertTrue(promoter.existing_version("image", "0.3.0", "commit"))
            with self.assertRaises(ValueError):
                promoter.existing_version("image", "0.3.0", "different-commit")


class MetadataTests(unittest.TestCase):
    def test_binstall_contract_for_every_target(self):
        metadata = json.loads(subprocess.check_output(["cargo", "+1.93.0", "metadata", "--no-deps", "--format-version", "1", "--locked"], cwd=ROOT))
        package = next(p for p in metadata["packages"] if p["name"] == "polkameter")
        metadata = package["metadata"]["binstall"]
        for target in TARGETS:
            config = {**metadata, **metadata.get("overrides", {}).get(target, {})}
            values = {"repo": package["repository"], "version": package["version"], "target": target, "bin": "polkameter", "binary-ext": ".exe" if "windows" in target else ""}
            url = config["pkg-url"]
            binary = config["bin-dir"]
            for key, value in values.items():
                url = url.replace("{ " + key + " }", value)
                binary = binary.replace("{ " + key + " }", value)
            extension = "zip" if "windows" in target else "tar.gz"
            self.assertEqual(url, f"https://github.com/agustinustheo/polkameter/releases/download/v{package['version']}/polkameter-{package['version']}-{target}.{extension}")
            self.assertEqual(binary, "polkameter.exe" if "windows" in target else "polkameter")
            self.assertEqual(config["pkg-fmt"], "zip" if "windows" in target else "tgz")


if __name__ == "__main__":
    unittest.main()
