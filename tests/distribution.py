#!/usr/bin/env python3
"""Offline tests for release packaging, installation and resumable publishing."""

import contextlib
import functools
import hashlib
from http.server import BaseHTTPRequestHandler, SimpleHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import threading
import unittest
from unittest.mock import Mock, patch
from urllib.error import HTTPError
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


class PublisherTests(unittest.TestCase):
    def setUp(self):
        self.package = {"name": "cli", "version": "0.2.0"}
        self.credentials = Mock()
        self.credentials.environment.return_value = {}
        self.credentials.preferred_mode.return_value = "trusted-publishing"
        self.credentials.fallback = ""
        self.output = contextlib.redirect_stdout(io.StringIO())
        self.output.__enter__()

    def tearDown(self):
        self.output.__exit__(None, None, None)

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

    def test_dev_dependency_does_not_create_cycle(self):
        metadata = self.metadata()
        metadata["packages"][1]["dependencies"] = [{"name": "cli", "kind": "dev", "path": "/local"}]
        self.assertEqual(len(publisher.publish_order(metadata)), 2)

    def test_external_packages_excluded(self):
        metadata = self.metadata()
        metadata["packages"].append({"id": "external", "name": "external", "publish": None})
        self.assertEqual(len(publisher.publish_order(metadata)), 2)

    def test_tag_matches(self):
        self.assertEqual(publisher.release_version(self.metadata(), "v0.2.0"), "0.2.0")

    def test_tag_mismatch(self):
        with self.assertRaises(ValueError):
            publisher.release_version(self.metadata(), "v0.3.0")

    def test_moving_or_invalid_tag_rejected(self):
        for tag in ("v0", "main", "v0.2.0/path", "v00.2.0"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                publisher.release_version(self.metadata(), tag)

    @patch.object(publisher, "published", return_value=True)
    @patch.object(publisher.subprocess, "run")
    def test_existing_version_skipped(self, run, _published):
        publisher.publish(self.package, self.credentials)
        run.assert_not_called()

    @patch.object(publisher, "published", return_value=False)
    @patch.object(publisher.subprocess, "run", return_value=Mock(returncode=0, stdout="published\n"))
    def test_success(self, run, _published):
        publisher.publish(self.package, self.credentials)
        self.assertEqual(run.call_args.args[0][-2:], ["-p", "cli"])

    @patch.object(publisher.time, "sleep")
    @patch.object(publisher, "published", return_value=False)
    @patch.object(publisher.subprocess, "run", side_effect=[Mock(returncode=1, stdout="HTTP 429 Too Many Requests"), Mock(returncode=0, stdout="published\n")])
    def test_publish_rate_limit_retry(self, run, _published, sleep):
        publisher.publish(self.package, self.credentials)
        self.assertEqual(run.call_count, 2)
        sleep.assert_called_once_with(30)

    @patch.object(publisher.time, "sleep")
    @patch.object(publisher, "published", return_value=False)
    @patch.object(publisher.subprocess, "run", return_value=Mock(returncode=1, stdout="rate limit exceeded"))
    def test_rate_limit_exhaustion(self, run, _published, sleep):
        with self.assertRaises(RuntimeError):
            publisher.publish(self.package, self.credentials)
        self.assertEqual(run.call_count, 6)
        self.assertEqual(sum(call.args[0] for call in sleep.call_args_list), 900)

    @patch.object(publisher, "published", side_effect=[False, True])
    @patch.object(publisher.subprocess, "run", return_value=Mock(returncode=1, stdout="upload response lost"))
    def test_successful_upload_with_lost_response(self, run, _published):
        publisher.publish(self.package, self.credentials)
        self.assertEqual(run.call_count, 1)

    @patch.object(publisher.time, "sleep")
    @patch.object(publisher, "published", return_value=False)
    @patch.object(publisher.subprocess, "run", return_value=Mock(returncode=1, stdout="invalid token"))
    def test_auth_failure_does_not_retry(self, _run, _published, sleep):
        with self.assertRaises(RuntimeError):
            publisher.publish(self.package, self.credentials)
        sleep.assert_not_called()

    @patch.object(publisher, "request_json", side_effect=HTTPError("url", 404, "missing", {}, None))
    def test_registry_404_missing(self, _request):
        self.assertFalse(publisher.published("cli", "0.2.0"))

    @patch.object(publisher, "request_json", side_effect=HTTPError("url", 403, "forbidden", {}, None))
    def test_registry_errors_not_treated_as_missing(self, _request):
        with self.assertRaises(HTTPError):
            publisher.published("cli", "0.2.0")

    @patch.object(publisher.time, "sleep")
    @patch.object(publisher, "request_json", side_effect=[HTTPError("url", 429, "limited", {}, None), {}])
    def test_registry_api_backoff(self, _request, sleep):
        self.assertTrue(publisher.published("cli", "0.2.0"))
        sleep.assert_called_once_with(30)

    def test_generic_user_agent(self):
        with patch.object(publisher, "urlopen") as urlopen:
            urlopen.return_value.__enter__.return_value.read.return_value = b"{}"
            publisher.request_json("https://crates.io/api/v1/crates/cli/0.2.0")
            self.assertEqual(urlopen.call_args.args[0].get_header("User-agent"), "polkameter-release")

    @patch.dict(os.environ, {"CARGO_REGISTRY_TOKEN": "fallback", "TRUSTED_PUBLISHING_TOKEN": ""})
    def test_fallback_token(self):
        self.assertEqual(publisher.Credentials().environment("fallback-secret")["CARGO_REGISTRY_TOKEN"], "fallback")

    @patch.dict(os.environ, {"CARGO_REGISTRY_TOKEN": "", "TRUSTED_PUBLISHING_TOKEN": ""})
    def test_missing_credentials(self):
        with self.assertRaises(ValueError):
            publisher.Credentials().environment("fallback-secret")

    @patch.dict(os.environ, {"TRUSTED_PUBLISHING_TOKEN": "bootstrap", "ACTIONS_ID_TOKEN_REQUEST_URL": "https://github.test/token?x=1", "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "request"})
    @patch.object(publisher, "request_json", side_effect=[{"value": "jwt"}, {"token": "renewed"}, None])
    def test_trusted_token_refresh_and_revoke(self, request):
        credentials = publisher.Credentials()
        credentials.created -= 601
        self.assertEqual(credentials.environment("trusted-publishing")["CARGO_REGISTRY_TOKEN"], "renewed")
        credentials.close()
        self.assertEqual(request.call_args.kwargs["method"], "DELETE")
        self.assertIn("audience=crates.io", request.call_args_list[0].args[0])


class RegistryTests(unittest.TestCase):
    """Real HTTP stub registry plus a fake cargo process; no crates.io writes."""

    def run_mixed_publish(self, status, fallback):
        attempts, uploaded = [], set()

        class Registry(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_GET(self):
                name = self.path.split("/")[-2]
                self.send_response(200 if name in uploaded else 404)
                self.end_headers()
                self.wfile.write(b"{}")

            def do_POST(self):
                name = self.path.removeprefix("/")
                token = self.headers["Authorization"]
                attempts.append((name, token))
                allowed = token == ("fallback" if name == "unconfigured" else "trusted")
                if allowed:
                    uploaded.add(name)
                self.send_response(200 if allowed else status)
                self.end_headers()

        with tempfile.TemporaryDirectory() as directory:
            cargo = Path(directory) / "cargo"
            cargo.write_text("#!" + __import__("sys").executable + "\n" + """
import os, sys
from urllib.request import Request, urlopen
from urllib.error import HTTPError
try:
    with urlopen(Request(os.environ['STUB_REGISTRY'] + '/' + sys.argv[-1], method='POST', headers={'Authorization': os.environ['CARGO_REGISTRY_TOKEN']})):
        print('published')
except HTTPError as error:
    print(error)
    sys.exit(1)
""")
            cargo.chmod(0o755)
            server = ThreadingHTTPServer(("127.0.0.1", 0), Registry)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            base = f"http://127.0.0.1:{server.server_port}"
            try:
                with patch.dict(os.environ, {"PATH": directory + ":" + os.environ["PATH"], "STUB_REGISTRY": base,
                                            "TRUSTED_PUBLISHING_TOKEN": "trusted", "CARGO_REGISTRY_TOKEN": fallback}), patch.object(publisher, "API", base), contextlib.redirect_stdout(io.StringIO()) as log:
                    credentials = publisher.Credentials()
                    if not fallback:
                        with self.assertRaises(RuntimeError):
                            publisher.publish({"name": "unconfigured", "version": "0.3.0"}, credentials)
                    else:
                        for name in ("configured", "unconfigured", "configured-again"):
                            publisher.publish({"name": name, "version": "0.3.0"}, credentials)
                        # Resume skips the successful version without invoking cargo again.
                        publisher.publish({"name": "unconfigured", "version": "0.3.0"}, credentials)
                        self.assertIn("using trusted-publishing", log.getvalue())
                        self.assertIn("using fallback-secret", log.getvalue())
            finally:
                server.shutdown()
                server.server_close()
                thread.join()
        return attempts

    def test_mixed_coverage_401(self):
        self.assertEqual(self.run_mixed_publish(401, "fallback"), [
            ("configured", "trusted"), ("unconfigured", "trusted"), ("unconfigured", "fallback"), ("configured-again", "trusted")])

    def test_mixed_coverage_403(self):
        self.assertEqual(len(self.run_mixed_publish(403, "fallback")), 4)

    def test_no_fallback_auth_failure(self):
        self.assertEqual(self.run_mixed_publish(403, ""), [("unconfigured", "trusted")])


class ReleaseTests(unittest.TestCase):
    def test_create_and_recovery_flags(self):
        for tag in ("v0.3.0", "v0.3.0-rc.1"):
            for exists in (False, True):
                with self.subTest(tag=tag, exists=exists), patch.object(releaser.subprocess, "run") as run:
                    run.return_value.returncode = 0 if exists else 1
                    releaser.publish(tag, ["archive", "archive.sha256"])
                    command = run.call_args_list[1].args[0]
                    self.assertEqual(command[2], "edit" if exists else "create")
                    if "-" in tag:
                        self.assertIn("--latest=false", command)
                        self.assertIn("--prerelease=true" if exists else "--prerelease", command)
                    elif exists:
                        self.assertIn("--prerelease=false", command)
                    self.assertEqual(run.call_args_list[2].args[0][-2:], ["archive", "archive.sha256"])

    def test_tag_ref_guard_runs_before_builds(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        guard = workflow.split("Require the release tag as the workflow ref for provenance", 1)[1].split("run: |\n", 1)[1].split("      #", 1)[0]
        for ref, code in (("refs/tags/v0.3.0", 0), ("refs/heads/main", 1), ("refs/tags/v0.2.0", 1)):
            with self.subTest(ref=ref):
                result = subprocess.run(["bash", "-c", guard], env={**os.environ, "GITHUB_REF": ref, "RELEASE_TAG": "v0.3.0"}, capture_output=True)
                self.assertEqual(result.returncode, code)

    def test_workflow_alias_serialization_and_permissions(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        container = workflow.split("\n  container:\n", 1)[1].split("\n  container-aliases:", 1)[0]
        self.assertNotIn("id-token: write", container)
        self.assertNotIn("metadata-action", container)
        aliases = workflow.split("\n  container-aliases:\n", 1)[1].split("\n  publish:", 1)[0]
        self.assertIn("group: release-container-aliases\n", aliases)
        self.assertIn("needs: [prepare, container]", aliases)
        self.assertIn("!contains(needs.prepare.outputs.version, '-')", aliases)
        self.assertIn("!contains(needs.prepare.outputs.version, '-')", workflow.split("\n  major-tag:", 1)[1])


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
