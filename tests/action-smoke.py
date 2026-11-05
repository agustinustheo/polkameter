#!/usr/bin/env python3
"""Run the composite action's shell blocks with a stub CLI, without GitHub or a chain."""
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
import re
import runpy
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import threading
import unittest
import zipfile

ACTION = Path(__file__).resolve().parents[1] / "action.yml"


def shell_block(name):
    step = ACTION.read_text().split(f"    - name: {name}\n", 1)[1]
    step = step.split("\n    - ", 1)[0]
    return textwrap.dedent(step.split("      run: ", 1)[1].removeprefix("|\n"))


class ActionTest(unittest.TestCase):
    def run_action(self, policy, verdict, expected, runtime_code=0, *, malformed=False,
                   preexisting=False, junit=False):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            cli = work / "polkameter"
            cli.write_text(textwrap.dedent('''\
                #!/usr/bin/env bash
                set -eu
                if [ "$1" != run ]; then exit 0; fi
                shift
                flag=false
                while [ "$#" -gt 0 ]; do
                  case "$1" in
                    --output) output=$2; shift ;;
                    --fail-on-verdict) flag=true ;;
                  esac
                  shift
                done
                test "$flag" = "$EXPECT_FLAG"
                mkdir -p "$output/run-test"
                echo 'test summary' > "$output/run-test/summary.md"
                if [ "$JUNIT" = true ]; then
                  echo '<testsuites/>' > "$output/run-test/junit.xml"
                fi
                if [ "$MALFORMED" = true ]; then
                  printf '{"verdict":{"result":"FAIL"}}\\n{' > "$output/run-test/summary.json"
                elif [ -n "$TEST_VERDICT" ]; then
                  printf '{"verdict":{"result":"%s"}}\\n' "$TEST_VERDICT" > "$output/run-test/summary.json"
                fi
                if [ "$TEST_CODE" -ne 0 ]; then exit "$TEST_CODE"; fi
                if [ "$flag" = true ] && [ "$TEST_VERDICT" = FAIL ]; then exit 1; fi
                exit "$TEST_CODE"
                '''))
            cli.chmod(0o755)
            env = dict(os.environ, PATH=f"{work}:{os.environ['PATH']}",
                       RUNNER_TEMP=str(work), GITHUB_ENV=str(work / "env"),
                       GITHUB_OUTPUT=str(work / "outputs"), GITHUB_STEP_SUMMARY=str(work / "summary"),
                       PLAN="test.xml", PLUGINS="", PLUGIN_CRATES="", CREDENTIALS="", TOPOLOGIES="", CAPABILITIES="",
                       OUTPUT=str(work / "runs"), FAIL_ON=policy, TEST_VERDICT=verdict,
                       TEST_CODE=str(runtime_code), JUNIT=str(junit).lower(), MALFORMED=str(malformed).lower(),
                       EXPECT_FLAG=str(policy == "verdict").lower())
            result = subprocess.run(["bash", "-euo", "pipefail", "-c",
                                     shell_block("Validate failure policy")],
                                    env=env, capture_output=True, text=True)
            if policy == "invalid":
                self.assertEqual(result.returncode, 2)
                self.assertIn("fail-on must be error or verdict", result.stdout)
                self.assertFalse((work / "runs").exists())
                self.assertFalse((work / "outputs").exists())
                return
            self.assertEqual(result.returncode, 0, result.stderr)
            if preexisting:
                for name in ("run-aaa", "run-zzz"):
                    old = work / "runs" / name
                    old.mkdir(parents=True)
                    (old / "summary.json").write_text('{"verdict":{"result":"FAIL"}}')
            result = subprocess.run(["bash", "-euo", "pipefail", "-c", shell_block("Run the plan")],
                                    env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            if malformed:
                self.assertIn("::warning::Could not read verdict", result.stdout)
            outputs = dict(line.split("=", 1) for line in (work / "outputs").read_text().splitlines())
            self.assertEqual(outputs["exit-code"], str(expected))
            self.assertEqual(outputs["verdict"], "" if malformed else verdict)
            self.assertEqual(Path(outputs["run-dir"]).name, "run-test")
            self.assertEqual(outputs["junit"], str(work / "runs/run-test/junit.xml") if junit else "")
            self.assertEqual((work / "summary").read_text(), "test summary\n")
            result = subprocess.run(["bash", "-c", shell_block("Fail the job if the plan failed")],
                                    env=dict(env, EXIT_CODE=outputs["exit-code"]))
            self.assertEqual(result.returncode, expected)

    def test_default_preserves_fail_verdict_exit_code(self):
        self.run_action("error", "FAIL", 0)

    def test_verdict_policy_fails(self):
        self.run_action("verdict", "FAIL", 1)

    def test_verdict_policy_passes(self):
        self.run_action("verdict", "PASS", 0)

    def test_verdict_policy_warns(self):
        self.run_action("verdict", "WARN", 0)

    def test_verdict_policy_inconclusive(self):
        self.run_action("verdict", "INCONCLUSIVE", 0)

    def test_no_load_has_empty_verdict(self):
        self.run_action("verdict", "", 0)

    def test_runtime_failure_is_preserved(self):
        self.run_action("error", "", 1, runtime_code=1)

    def test_verdict_policy_preserves_invalid_run_exit_code(self):
        self.run_action("verdict", "FAIL", 2, runtime_code=2)

    def test_verdict_policy_preserves_interrupted_exit_code(self):
        self.run_action("verdict", "FAIL", 130, runtime_code=130)

    def test_preexisting_run_directories_are_ignored(self):
        self.run_action("verdict", "PASS", 0, preexisting=True)

    def test_malformed_summary_preserves_artifact_outputs_and_exit_code(self):
        self.run_action("verdict", "FAIL", 1, runtime_code=1, malformed=True)

    def test_junit_output_when_present(self):
        self.run_action("error", "", 0, junit=True)

    def test_invalid_policy(self):
        self.assertLess(ACTION.read_text().index("- name: Validate failure policy"),
                        ACTION.read_text().index("- name: Build PolkaMeter"))
        self.run_action("invalid", "", 2)


class InstallTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.work = Path(self.directory.name)
        self.bin = self.work / "bin"
        self.bin.mkdir()
        (self.bin / "python3").symlink_to(sys.executable)
        self.action = self.work / "action"
        (self.action / "crates/cli").mkdir(parents=True)
        (self.action / "scripts").symlink_to(ACTION.parent / "scripts")
        (self.action / "Cargo.toml").write_text('[workspace.package]\nversion = "0.3.0"\n')
        (self.action / "crates/cli/Cargo.toml").write_text('[package]\nversion.workspace = true\n')
        self.routes = {}
        self.requests = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                owner.requests.append((self.path, self.headers.get("Authorization")))
                data = owner.routes.get(self.path)
                if isinstance(data, tuple):
                    self.send_response(302)
                    self.send_header("Location", data[0])
                    self.end_headers()
                    return
                self.send_response(200 if data is not None else 404)
                self.end_headers()
                if data is not None:
                    self.wfile.write(data if isinstance(data, bytes) else json.dumps(data).encode())

            def log_message(self, *_args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=0.01), daemon=True)
        thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(thread.join)
        self.addCleanup(self.server.shutdown)
        self.api = f"http://127.0.0.1:{self.server.server_port}"
        self.prefix = "/repos/agustinustheo/polkameter/"
        self.env = dict(os.environ, PATH=str(self.bin),
                        GITHUB_ACTION_PATH=str(self.action), GITHUB_API_URL=self.api,
                        GITHUB_TOKEN="test-runner-token", RUNNER_TEMP=str(self.work),
                        GITHUB_OUTPUT=str(self.work / "outputs"), GITHUB_PATH=str(self.work / "path"),
                        RUNNER_OS="Linux", RUNNER_ARCH="X64", INSTALL="auto", VERSION="",
                        ACTION_REF="v0.3.0", VERIFY_ATTESTATION="false", PLUGIN_CRATES="")
        self.env.update(GITHUB_API_URL="https://api.github.com", GITHUB_SERVER_URL="https://github.com",
                        GITHUB_STEP_SUMMARY=str(self.work / "summary"))
        self.harness = self.work / "install.py"
        self.harness.write_text(textwrap.dedent(f'''\
            import runpy
            from urllib.request import Request, build_opener
            from unittest.mock import patch
            class Transport:
                def __init__(self, *handlers):
                    self.opener = build_opener(*handlers)
                def open(self, request, **kwargs):
                    with open({str(self.work / "urls")!r}, "a") as log:
                        log.write(request.full_url + "\\n")
                    assert request.full_url.startswith("https://api.github.com/")
                    url = request.full_url.replace("https://api.github.com", {self.api!r}, 1)
                    return self.opener.open(Request(url, headers=dict(request.header_items())), **kwargs)
            with patch("urllib.request.build_opener", Transport):
                runpy.run_path({str(ACTION.parent / "scripts/action-install.py")!r}, run_name="__main__")
            '''))
        self.release = self.add_release()

    def route(self, path, value):
        self.routes[self.prefix + path] = value

    def add_release(self, version="0.3.0", target="x86_64-unknown-linux-gnu", windows=False,
                    payload=b"fake binary"):
        name = f"polkameter-{version}-{target}." + ("zip" if windows else "tar.gz")
        data = io.BytesIO()
        if windows:
            with zipfile.ZipFile(data, "w") as archive:
                archive.writestr("polkameter.exe", payload)
        else:
            with tarfile.open(fileobj=data, mode="w:gz") as archive:
                member = tarfile.TarInfo("polkameter")
                member.size = len(payload)
                archive.addfile(member, io.BytesIO(payload))
        content = data.getvalue()
        self.routes[f"/assets/{name}"] = content
        self.routes[f"/assets/{name}.sha256"] = f"{hashlib.sha256(content).hexdigest()}  {name}\n".encode()
        release = dict(tag_name=f"v{version}", draft=False, assets=[
            dict(name=n, url=f"https://api.github.com/assets/{n}") for n in (name, f"{name}.sha256")])
        self.route(f"releases/tags/v{version}", release)
        return release

    def run_install(self, method="release", version="0.3.0", *, error=None, **env):
        self.env.update(env)
        for name in ("path", "outputs", "summary", "gh-args", "urls"):
            (self.work / name).unlink(missing_ok=True)
        result = subprocess.run([sys.executable, str(self.harness)],
                                env=self.env, capture_output=True, text=True)
        self.result = result
        self.assertEqual(result.returncode, 1 if error else 0, result.stdout + result.stderr)
        if error:
            self.assertIn(error, result.stdout + result.stderr)
            self.assertFalse((self.work / "path").exists())
            self.assertFalse((self.work / "outputs").exists())
            self.assertIn("**failed**", (self.work / "summary").read_text())
            return result
        outputs = dict(line.split("=", 1) for line in (self.work / "outputs").read_text().splitlines())
        self.assertEqual(outputs, {"version": version, "install-method": method,
                                  "needs-toolchain": str(method == "source" or bool(
                                      self.env["PLUGIN_CRATES"].strip())).lower()})
        summary = (self.work / "summary").read_text()
        self.assertIn(f"**{method}** (version {version})", summary)
        self.assertIn("::notice::", result.stdout)
        if method == "release":
            binary = "polkameter.exe" if self.env["RUNNER_OS"] == "Windows" else "polkameter"
            installed = Path((self.work / "path").read_text().strip()) / binary
            self.assertEqual(installed.read_bytes(), b"fake binary")
            self.assertTrue(os.access(installed, os.X_OK))
        else:
            self.assertFalse((self.work / "path").exists())
            if self.env["INSTALL"] == "auto":
                self.assertIn("::warning::Using source build:", result.stdout)
        return outputs

    def gh(self, code=0):
        stub = self.bin / "gh"
        stub.write_text(f'''#!/bin/sh
printf "%s\\n" "$@" > "{self.work}/gh-args"
printf '%s\\n' "$GH_HOST" "${{GH_TOKEN-}}" "${{GITHUB_TOKEN-}}" > "{self.work}/gh-env"
exit {code}
''')
        stub.chmod(0o755)

    def assert_requests(self, *paths):
        self.assertEqual([path for path, _ in self.requests], list(paths))

    def test_full_tag_downloads_authenticated_archive_and_checksum(self):
        # Only this release exists: an incorrect lookup cannot succeed by fallback.
        self.routes.pop(self.prefix + "releases/tags/v0.3.0")
        self.add_release(version="0.5.0")
        self.run_install(ACTION_REF="v0.5.0", version="0.5.0")
        self.assertEqual(self.requests[0][0], self.prefix + "releases/tags/v0.5.0")
        self.assertEqual(len(self.requests), 3)
        self.assertTrue(all(auth == "Bearer test-runner-token" for _, auth in self.requests))

    def test_moving_major_uses_package_version_after_tag_advances(self):
        self.add_release(version="0.4.0")
        self.route("git/ref/tags/v0", {"object": {"type": "commit", "sha": "new-commit"}})
        self.run_install(ACTION_REF="v0")
        self.assertEqual(self.requests[0][0], self.prefix + "releases/tags/v0.3.0")
        self.assertFalse(any("git/" in path or "releases?" in path for path, _ in self.requests))

    def test_moving_major_missing_package_release_does_not_install_newer(self):
        self.routes.pop(self.prefix + "releases/tags/v0.3.0")
        newer = self.add_release(version="0.4.0")
        self.route("releases?per_page=100&page=1", [newer])
        self.route("git/ref/tags/v0", {"object": {"type": "commit", "sha": "new-commit"}})
        self.run_install("source", ACTION_REF="v0")
        self.assert_requests(self.prefix + "releases/tags/v0.3.0")

    def test_moving_major_must_match_package_major(self):
        self.run_install("source", ACTION_REF="v1")
        self.assert_requests()

    def test_branch_sha_and_local_refs_build_source(self):
        for ref in ("main", "abc123def", ""):
            with self.subTest(ref=ref):
                self.run_install("source", ACTION_REF=ref)
        self.assert_requests()

    def test_explicit_version_overrides_branch_or_tag(self):
        self.routes.pop(self.prefix + "releases/tags/v0.3.0")
        self.add_release(version="0.5.0")
        for ref, version in (("main", "0.5.0"), ("v0.3.0", "v0.5.0")):
            with self.subTest(ref=ref, version=version):
                self.run_install(ACTION_REF=ref, VERSION=version, version="0.5.0")
        self.assertEqual([p for p, _ in self.requests if "releases/tags/" in p],
                         [self.prefix + "releases/tags/v0.5.0"] * 2)

    def test_forced_source_skips_network_and_version_override(self):
        self.run_install("source", INSTALL="source", VERSION="0.5.0")
        self.assert_requests()

    def test_missing_release_falls_back(self):
        self.run_install("source", ACTION_REF="v0.2.0")

    def test_missing_explicit_release_reports_source_version(self):
        self.run_install("source", ACTION_REF="main", VERSION="0.9.0")
        self.assert_requests(self.prefix + "releases/tags/v0.9.0")

    def test_missing_asset_falls_back(self):
        self.release["assets"].pop()
        self.run_install("source")

    def test_missing_asset_forced_release_fails(self):
        self.release["assets"] = []
        self.run_install(error="has no archive and checksum", INSTALL="release")

    def test_forced_release_without_release_ref_fails(self):
        self.run_install(error="not a release tag", INSTALL="release", ACTION_REF="main")

    def test_bad_checksum_fails_even_in_auto(self):
        name = self.release["assets"][0]["name"]
        self.routes[f"/assets/{name}.sha256"] = f'{"0" * 64}  {name}\n'.encode()
        self.run_install(error="Checksum verification failed")

    def test_substituted_old_attested_archive_is_rejected(self):
        old = self.add_release(version="0.2.0", payload=b"older binary")
        # An attacker renames an older, legitimately attested archive and updates
        # the checksum. gh must still demand the *requested* source tag.
        name = "polkameter-0.3.0-x86_64-unknown-linux-gnu.tar.gz"
        self.release["assets"] = [dict(name=n, url=f"https://api.github.com/assets/{n}")
                                  for n in (name, name + ".sha256")]
        content = self.routes["/assets/" + old["assets"][0]["name"]]
        self.routes[f"/assets/{name}"] = content
        self.routes[f"/assets/{name}.sha256"] = f"{hashlib.sha256(content).hexdigest()}  {name}\n".encode()
        self.gh()
        # Model an attestation valid only for v0.2.0. A loose gh invocation would
        # accept it; the correctly constrained invocation rejects it.
        stub = self.bin / "gh"
        stub.write_text(stub.read_text().replace(
            "exit 0", 'case "$*" in *"--source-ref refs/tags/v0.3.0"*) exit 1;; *) exit 0;; esac'))
        self.run_install(error="returned non-zero exit status", VERIFY_ATTESTATION="true")
        self.assertIn("refs/tags/v0.3.0", (self.work / "gh-args").read_text())

    def test_attestation_disabled_warns_and_does_not_call_gh(self):
        self.gh(1)
        self.run_install(VERIFY_ATTESTATION="false")
        self.assertFalse((self.work / "gh-args").exists())
        self.assertIn("::warning::verify-attestation=false", self.result.stdout)
        self.assertIn("provenance verification explicitly disabled", (self.work / "summary").read_text())

    def test_attestation_required_binds_workflow_source_tag_and_digest(self):
        self.gh()
        self.run_install(VERIFY_ATTESTATION="true")
        args = (self.work / "gh-args").read_text().splitlines()
        self.assertEqual(args[:2], ["attestation", "verify"])
        self.assertEqual(args[3:], ["--repo", "agustinustheo/polkameter", "--hostname", "github.com",
                                   "--signer-workflow", "agustinustheo/polkameter/.github/workflows/release.yml",
                                   "--source-ref", "refs/tags/v0.3.0", "--digest-alg", "sha256"])
        self.assertTrue(Path(args[2]).exists())

    def test_attestation_default_requires_gh(self):
        self.run_install("source", VERIFY_ATTESTATION="")
        self.assertIn("requires gh", self.result.stdout)
        self.assertIn("requires gh", (self.work / "summary").read_text())

    def test_attestation_unset_requires_gh(self):
        self.env.pop("VERIFY_ATTESTATION")
        self.run_install("source")

    def test_attestation_default_verifies_when_gh_exists(self):
        self.gh()
        self.run_install(VERIFY_ATTESTATION="")
        self.assertTrue((self.work / "gh-args").exists())

    def test_attestation_failure_is_fatal_in_auto_and_release(self):
        self.gh(1)
        for mode in ("auto", "release"):
            with self.subTest(mode=mode):
                self.run_install(error="returned non-zero exit status", INSTALL=mode, VERIFY_ATTESTATION="true")

    def test_attestation_required_without_gh_falls_back_in_auto(self):
        self.run_install("source", VERIFY_ATTESTATION="true")

    def test_default_without_gh_fails_in_release_mode(self):
        self.run_install(error="requires gh", INSTALL="release", VERIFY_ATTESTATION="")

    def test_ghes_uses_public_distribution_without_enterprise_token(self):
        self.gh()
        self.run_install(GITHUB_API_URL="https://ghe.example/api/v3",
                         GITHUB_SERVER_URL="https://ghe.example", VERIFY_ATTESTATION="true",
                         GH_HOST="ghe.example", GH_TOKEN="enterprise-token")
        self.assertTrue(all(auth is None for _, auth in self.requests))
        self.assertEqual((self.work / "gh-env").read_text().splitlines(), ["github.com", "", ""])
        urls = (self.work / "urls").read_text().splitlines()
        self.assertTrue(all(url.startswith("https://api.github.com/") for url in urls))
        self.assertIn("github.com", (self.work / "gh-args").read_text())

    def test_public_runner_passes_token_only_to_public_gh(self):
        self.gh()
        self.run_install(VERIFY_ATTESTATION="true", GH_TOKEN="ignored-token", GH_HOST="ghe.example")
        self.assertEqual((self.work / "gh-env").read_text().splitlines(),
                         ["github.com", "test-runner-token", ""])

    def test_download_redirect_strips_token_for_storage_host(self):
        name = self.release["assets"][0]["name"]
        content = self.routes[f"/assets/{name}"]
        self.routes[f"/assets/{name}"] = (f"http://localhost:{self.server.server_port}/storage",)
        self.routes["/storage"] = content
        self.run_install()
        self.assertIn(("/storage", None), self.requests)

    def test_package_version_can_be_declared_directly(self):
        (self.action / "crates/cli/Cargo.toml").write_text('[package]\nversion = "0.4.0"\n')
        self.add_release(version="0.4.0")
        self.run_install(ACTION_REF="v0", version="0.4.0")
        self.assertEqual(self.requests[0][0], self.prefix + "releases/tags/v0.4.0")

    def test_forced_release_with_missing_release_fails(self):
        self.run_install(error="GitHub request failed", ACTION_REF="v0.2.0", INSTALL="release")

    def test_release_response_must_match_requested_tag_and_be_published(self):
        for update in (dict(tag_name="v0.4.0"), dict(draft=True)):
            with self.subTest(update=update):
                self.release.update(tag_name="v0.3.0", draft=False)
                self.release.update(update)
                self.run_install(error="did not return the requested published release")

    def test_source_build_keeps_action_target_and_puts_cli_on_path(self):
        self.run_install("source", INSTALL="source")
        cargo = self.bin / "cargo"
        cargo.write_text(f'''#!/bin/sh
printf '%s\\n' "$@" > "{self.work}/cargo-args"
''')
        cargo.chmod(0o755)
        self.env["TOOLCHAIN"] = "1.93.0"
        result = subprocess.run(["/bin/bash", "-euo", "pipefail", "-c", shell_block("Build PolkaMeter")],
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        args = (self.work / "cargo-args").read_text().splitlines()
        self.assertIn("--locked", args)
        self.assertEqual(args[-2:], ["--target-dir", str(self.action / ".polkameter-action-target")])
        self.assertEqual((self.work / "path").read_text().strip(),
                         str(self.action / ".polkameter-action-target/release"))

    def test_plugin_crates_require_toolchain_with_release(self):
        self.run_install(PLUGIN_CRATES="plugins/example:example")

    def test_all_release_archive_platforms(self):
        # Archive handling only; the full composite action supports Linux/macOS.
        for system, arch, target in (("Linux", "ARM64", "aarch64-unknown-linux-gnu"),
                                     ("macOS", "X64", "x86_64-apple-darwin"),
                                     ("macOS", "ARM64", "aarch64-apple-darwin"),
                                     ("Windows", "X64", "x86_64-pc-windows-msvc")):
            with self.subTest(system=system, arch=arch):
                self.add_release(target=target, windows=system == "Windows")
                self.run_install(RUNNER_OS=system, RUNNER_ARCH=arch)

    def test_unsupported_platform_uses_source(self):
        self.run_install("source", RUNNER_ARCH="ARM")

    def test_invalid_inputs_fail(self):
        for env in (dict(INSTALL="bad"), dict(VERSION="bad"), dict(VERIFY_ATTESTATION="bad")):
            with self.subTest(env=env):
                self.env.update(INSTALL="auto", VERSION="", VERIFY_ATTESTATION="false")
                self.run_install(error="must be", **env)

    def test_toolchain_and_build_steps_are_gated(self):
        action = ACTION.read_text()
        self.assertEqual(action.count("if: steps.install.outputs.needs-toolchain == 'true'"), 1)
        self.assertEqual(action.count("if: steps.install.outputs.install-method == 'source'"), 2)
        self.assertIn(".polkameter-action-target", shell_block("Build PolkaMeter"))
        self.assertEqual(shell_block("Install released PolkaMeter or select source").strip(),
                         'python3 "$GITHUB_ACTION_PATH/scripts/action-install.py"')
        self.assertIn('PLUGIN_UNLOCKED: ${{ inputs.plugin-unlocked }}', action)
        self.assertIn('default: "true"', action.split('  verify-attestation:', 1)[1].split('  credentials:', 1)[0])

    def run_plugins(self, *, unlocked="false", cargo_code=0, expected=0):
        self.run_install(PLUGIN_CRATES="plugin")
        manifest = self.work / "plugin directory"
        manifest.mkdir(exist_ok=True)
        (manifest / "Cargo.toml").touch()
        self.env.update(PLUGIN_CRATES=f"  {manifest}:example  \n\n{manifest}/Cargo.toml",
                        TOOLCHAIN="1.93.0", PLUGIN_UNLOCKED=unlocked)
        cargo = self.bin / "cargo"
        payload = json.dumps({"reason": "compiler-artifact", "target": {"kind": ["bin"]},
                              "executable": "/a path/plugin"})
        cargo.write_text(f'''#!/bin/sh
printf '%s\\n' "$@" >> "{self.work}/cargo-args"
echo '{payload}'
exit {cargo_code}
''')
        cargo.chmod(0o755)
        cli = self.bin / "polkameter"
        cli.write_text('#!/bin/sh\nprintf "%s\\n" "$@" >> "' + str(self.work / "plugin-args") + '"\n')
        cli.chmod(0o755)
        result = subprocess.run([sys.executable, str(ACTION.parent / "scripts/action-build-plugins.py")],
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, expected, result.stderr)
        return result

    def test_plugin_package_paths_and_binaries_with_spaces_are_locked(self):
        self.run_plugins()
        args = (self.work / "cargo-args").read_text().splitlines()
        self.assertEqual(args.count("--release"), 2)
        self.assertEqual(args.count("--locked"), 2)
        self.assertEqual(args.count("--package"), 1)
        self.assertIn(str(self.work / "plugin directory/Cargo.toml"), args)
        self.assertEqual((self.work / "plugin-args").read_text().splitlines(),
                         ["plugin", "install", "/a path/plugin"] * 2)

    def test_plugin_unlocked_requires_explicit_opt_in_and_warns(self):
        result = self.run_plugins(unlocked="true")
        self.assertNotIn("--locked", (self.work / "cargo-args").read_text().splitlines())
        self.assertIn("::warning::plugin-unlocked=true", result.stdout)

    def test_locked_plugin_failure_explains_missing_or_stale_lockfile(self):
        result = self.run_plugins(cargo_code=101, expected=1)
        self.assertIn("Commit an up-to-date Cargo.lock", result.stderr)
        self.assertIn("plugin-unlocked: true", result.stderr)
        self.assertFalse((self.work / "plugin-args").exists())

    def test_invalid_plugin_unlocked_input_fails(self):
        self.assertIn("plugin-unlocked must be", self.run_plugins(unlocked="bad", expected=1).stderr)


class ReleaseContractTest(unittest.TestCase):
    def assert_release_contract(self, root):
        workflow = (root / ".github/workflows/release.yml").read_text()
        # Skip ONLY the known pre-#97 desktop-bundle naming. Unknown or broken
        # future packaging must fail rather than silently disable this test.
        legacy = 'tar -C "$artifact" -czf "release-assets/polkameter-${target}.tar.gz" .'
        if legacy in workflow and "  bundle:" in workflow and "  cli-linux:" not in workflow:
            reason = "release.yml still publishes legacy unversioned bundles; CLI asset contract waits for #97"
            print(f"SKIP: {reason}")
            self.skipTest(reason)

        # Parse the release matrix and its packaging command, then run the actual
        # packager selected by release.yml. No second copy of its naming formula.
        targets = set(re.findall(r"^\s+target: ([a-z0-9_]+-(?:unknown-linux-gnu|apple-darwin|pc-windows-msvc))$",
                                 workflow, re.MULTILINE))
        self.assertEqual(len(targets), 5, "release.yml must define all five CLI targets")
        commands = re.findall(r'python3 (\S*scripts/[^\s]+\.py) "\$VERSION" "\$TARGET"', workflow)
        self.assertEqual(len(commands), 2, "both CLI jobs must package versioned target assets")
        self.assertEqual(len(set(commands)), 1)
        cli_jobs = re.findall(r"^  cli-[a-z-]+:\n(.*?)(?=^  [a-z][a-z-]*:|\Z)",
                              workflow, re.MULTILINE | re.DOTALL)
        self.assertEqual(len(cli_jobs), 2)
        for job in cli_jobs:
            self.assertIn('VERSION: ${{ needs.prepare.outputs.version }}', job)
            self.assertIn('TARGET: ${{ matrix.target }}', job)
        packager = root / "scripts" / Path(commands[0]).name
        self.assertTrue(packager.is_file(), f"release.yml packager missing: {packager}")
        name_for = runpy.run_path(str(ACTION.parent / "scripts/action-install.py"))["asset_name"]
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            binary = work / "fake-cli"
            binary.write_bytes(b"contract CLI")
            for document in ("LICENSE", "README.md"):
                (work / document).write_text("contract fixture")
            for target in sorted(targets):
                with self.subTest(target=target):
                    output = work / target
                    result = subprocess.run([sys.executable, str(packager), "0.3.0", target,
                                             str(binary), "--output", str(output)],
                                            cwd=work, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    expected = name_for("0.3.0", target)
                    self.assertEqual(sorted(p.name for p in output.iterdir()),
                                     sorted([expected, expected + ".sha256"]))
                    archive = output / expected
                    self.assertEqual(archive.with_name(expected + ".sha256").read_text(),
                                     f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {expected}\n")
                    if expected.endswith(".zip"):
                        with zipfile.ZipFile(archive) as bundle:
                            self.assertEqual(bundle.read("polkameter.exe"), b"contract CLI")
                    else:
                        with tarfile.open(archive) as bundle:
                            self.assertEqual(bundle.extractfile("polkameter").read(), b"contract CLI")

    def test_release_workflow_assets_match_installer(self):
        self.assert_release_contract(ACTION.parent)


if __name__ == "__main__":
    unittest.main()
