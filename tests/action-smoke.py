#!/usr/bin/env python3
"""Run the composite action's shell blocks with a stub CLI, without GitHub or a chain."""
import hashlib
import importlib.util
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
import re
import runpy
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import threading
import unittest
import zipfile

ACTION = Path(__file__).resolve().parents[1] / "action.yml"
# The last event of a run that PolkaMeter finished, after redacting its reports.
COMPLETED = '{"event": "completed", "outcome": {"state": "completed"}}\n'


def shell_block(name):
    step = ACTION.read_text().split(f"    - name: {name}\n", 1)[1]
    step = step.split("\n    - ", 1)[0]
    return textwrap.dedent(step.split("      run: ", 1)[1].removeprefix("|\n"))


class ActionTest(unittest.TestCase):
    def run_action(self, policy, verdict, expected, runtime_code=0, *, malformed=False,
                   preexisting=False, junit=False, killed=False, old_cli=False):
        """Runs the action's policy and run steps with a stub CLI. `old_cli` stands for a CLI
        from before v0.3.0, without --fail-on-inconclusive. Returns the run step's stdout."""
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            cli = work / "polkameter"
            cli.write_text(textwrap.dedent('''\
                #!/usr/bin/env bash
                set -eu
                if [ "$1" != run ]; then exit 0; fi
                if [ "${2:-}" = --help ]; then
                  echo "      --fail-on-verdict"
                  if [ "$OLD_CLI" != true ]; then echo "      --fail-on-inconclusive"; fi
                  exit 0
                fi
                shift
                flag=false
                inconclusive_flag=false
                while [ "$#" -gt 0 ]; do
                  case "$1" in
                    --output) output=$2; shift ;;
                    --fail-on-verdict) flag=true ;;
                    --fail-on-inconclusive) inconclusive_flag=true ;;
                  esac
                  shift
                done
                test "$flag" = "$EXPECT_FLAG"
                test "$inconclusive_flag" = "$EXPECT_INCONCLUSIVE_FLAG"
                mkdir -p "$output/run-test"
                echo 'test summary' > "$output/run-test/summary.md"
                if [ "$KILLED" = true ]; then
                  echo '{"event": "phase", "phase": "setup"}' > "$output/run-test/events.jsonl"
                  exit 137
                fi
                code=$TEST_CODE
                if [ "$code" -eq 0 ] && { { [ "$flag" = true ] && [ "$TEST_VERDICT" = FAIL ]; } ||
                  { [ "$inconclusive_flag" = true ] && [ "$TEST_VERDICT" = INCONCLUSIVE ]; }; }; then
                  code=1
                fi
                if [ "$code" -eq 1 ] && [ "$TEST_CODE" -eq 0 ]; then
                  echo '{"state": "completed_with_failures", "exit_code": 1}' > "$output/run-test/execution.json"
                else
                  echo '{"state": "completed", "exit_code": 0}' > "$output/run-test/execution.json"
                fi
                echo '{"event": "completed"}' > "$output/run-test/events.jsonl"
                if [ "$JUNIT" = true ]; then
                  echo '<testsuites/>' > "$output/run-test/junit.xml"
                fi
                if [ "$MALFORMED" = true ]; then
                  printf '{"verdict":{"result":"FAIL"}}\\n{' > "$output/run-test/summary.json"
                elif [ "$TEST_VERDICT" = INCONCLUSIVE ]; then
                  # Every step ran, but a required check has no result; optional ones do not count.
                  printf '%s\\n' '{"verdict":{"result":"INCONCLUSIVE"},"stop":{"rule":"rate cap"},"checks":[{"check":"no silent loss","status":"no result","detail":"state check timed out"},{"check":"collation funnel","status":"no result","optional":true,"detail":"not applicable"},{"check":"refusals are clean","status":"pass","detail":"0 refused"}]}' > "$output/run-test/summary.json"
                elif [ -n "$TEST_VERDICT" ]; then
                  printf '{"verdict":{"result":"%s"}}\\n' "$TEST_VERDICT" > "$output/run-test/summary.json"
                fi
                exit "$code"
                '''))
            cli.chmod(0o755)
            env = dict(os.environ, PATH=f"{work}:{os.environ['PATH']}",
                       RUNNER_TEMP=str(work), GITHUB_ENV=str(work / "env"),
                       GITHUB_OUTPUT=str(work / "outputs"), GITHUB_STEP_SUMMARY=str(work / "summary"),
                       PLAN="test.xml", PLUGINS="", PLUGIN_CRATES="", CREDENTIALS="", TOPOLOGIES="", CAPABILITIES="",
                       OUTPUT=str(work / "runs"), FAIL_ON=policy, TEST_VERDICT=verdict,
                       TEST_CODE=str(runtime_code), JUNIT=str(junit).lower(), MALFORMED=str(malformed).lower(),
                       EXPECT_FLAG=str(policy in ("verdict", "verdict-or-inconclusive")).lower(),
                       EXPECT_INCONCLUSIVE_FLAG=str(policy == "verdict-or-inconclusive" and not old_cli).lower(),
                       OLD_CLI=str(old_cli).lower(), KILLED=str(killed).lower(),
                       GITHUB_ACTION_PATH=str(ACTION.parent))
            result = subprocess.run(["bash", "-euo", "pipefail", "-c",
                                     shell_block("Validate failure policy")],
                                    env=env, capture_output=True, text=True)
            if policy == "invalid":
                self.assertEqual(result.returncode, 2)
                self.assertIn("fail-on must be error, verdict or verdict-or-inconclusive", result.stdout)
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
            failed_inconclusive = verdict == "INCONCLUSIVE" and expected == 1 and runtime_code == 0
            if killed:
                self.assertFalse((work / "summary").exists())
                self.assertIn("::notice::The run did not finish normally", result.stdout)
            elif failed_inconclusive:
                summary = (work / "summary").read_text()
                self.assertTrue(summary.startswith("test summary\n"), summary)
                self.assertIn("The load verdict is **INCONCLUSIVE**: required checks with no result: "
                              "no silent loss. ", summary)
                self.assertIn("set `fail-on: verdict` on this step", summary)
                self.assertIn("::error title=PolkaMeter verdict INCONCLUSIVE::required checks with no "
                              "result: no silent loss. ", result.stdout)
            else:
                self.assertEqual((work / "summary").read_text(), "test summary\n")
                self.assertNotIn("::error", result.stdout)
            self.assertEqual("--fail-on-inconclusive" in result.stdout,
                             old_cli and policy == "verdict-or-inconclusive")
            result_code = subprocess.run(["bash", "-c", shell_block("Fail the job if the plan failed")],
                                         env=dict(env, EXIT_CODE=outputs["exit-code"])).returncode
            self.assertEqual(result_code, expected)
            return result.stdout

    def test_killed_run_keeps_unredacted_summary_out_of_job_summary(self):
        self.run_action("error", "", 137, killed=True)

    def test_default_policy_fails_on_fail_or_inconclusive(self):
        inputs = ACTION.read_text().split("\n  fail-on:\n", 1)[1].split("\n  upload:\n", 1)[0]
        self.assertTrue(inputs.endswith("\n    default: verdict-or-inconclusive"), inputs)
        for verdict, expected in (("PASS", 0), ("WARN", 0), ("FAIL", 1), ("INCONCLUSIVE", 1), ("", 0)):
            with self.subTest(verdict=verdict):
                self.run_action("verdict-or-inconclusive", verdict, expected)

    def test_default_policy_preserves_runtime_exit_codes(self):
        for code in (1, 2, 130, 143):
            with self.subTest(code=code):
                self.run_action("verdict-or-inconclusive", "INCONCLUSIVE", code, runtime_code=code)

    def test_older_cli_degrades_to_fail_verdict_with_a_warning(self):
        for verdict, expected in (("INCONCLUSIVE", 0), ("FAIL", 1), ("PASS", 0)):
            with self.subTest(verdict=verdict):
                stdout = self.run_action("verdict-or-inconclusive", verdict, expected, old_cli=True)
                self.assertIn("::warning::This PolkaMeter CLI has no --fail-on-inconclusive", stdout)
                self.assertIn("set fail-on: verdict", stdout)

    def test_older_cli_is_not_asked_for_the_flag_under_other_policies(self):
        for policy, verdict, expected in (("verdict", "FAIL", 1), ("error", "INCONCLUSIVE", 0)):
            with self.subTest(policy=policy):
                stdout = self.run_action(policy, verdict, expected, old_cli=True)
                self.assertNotIn("::warning::", stdout)

    def test_error_policy_ignores_inconclusive(self):
        self.run_action("error", "INCONCLUSIVE", 0)

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
        self.statuses = {}
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                owner.requests.append((self.path, self.headers.get("Authorization")))
                if self.path in owner.statuses:
                    self.send_error(owner.statuses[self.path])
                    return
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
        self.payload = b"fake binary"

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
            self.assertEqual(installed.read_bytes(), self.payload)
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

    def assert_no_asset_downloads(self):
        self.assert_requests(self.prefix + "releases/tags/v0.3.0")
        self.assertFalse(list(self.work.glob("polkameter-cli-*")))

    def test_attestation_default_requires_gh(self):
        self.run_install("source", VERIFY_ATTESTATION="")
        self.assertIn("requires gh", self.result.stdout)
        self.assertIn("requires gh", (self.work / "summary").read_text())
        self.assert_no_asset_downloads()

    def test_attestation_unset_requires_gh(self):
        self.env.pop("VERIFY_ATTESTATION")
        self.run_install("source")
        self.assert_no_asset_downloads()

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
        self.assert_no_asset_downloads()

    def test_default_without_gh_fails_in_release_mode(self):
        self.run_install(error="requires gh", INSTALL="release", VERIFY_ATTESTATION="")
        self.assert_no_asset_downloads()

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

    def test_ghes_anonymous_release_rate_limit_falls_back_to_source(self):
        self.gh()
        self.statuses[self.prefix + "releases/tags/v0.3.0"] = 403
        self.run_install("source", GITHUB_API_URL="https://ghe.example/api/v3",
                         GITHUB_SERVER_URL="https://ghe.example", VERIFY_ATTESTATION="true",
                         GH_TOKEN="enterprise-token")
        self.assertIn("HTTP Error 403", self.result.stdout)
        self.assertTrue(all(auth is None for _, auth in self.requests))
        self.assert_no_asset_downloads()
        self.assertFalse((self.work / "gh-args").exists())

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

    def test_prerelease_ref_or_version_uses_release_path(self):
        self.add_release(version="0.3.0-rc.1")["prerelease"] = True
        self.gh()
        self.run_install(INSTALL="release", ACTION_REF="v0.3.0-rc.1", version="0.3.0-rc.1",
                         VERIFY_ATTESTATION="true")
        self.assertIn("refs/tags/v0.3.0-rc.1", (self.work / "gh-args").read_text())
        for version in ("0.3.0-rc.1", "v0.3.0-rc.1"):
            with self.subTest(version=version):
                self.run_install(INSTALL="release", ACTION_REF="main", VERSION=version, version="0.3.0-rc.1",
                                 VERIFY_ATTESTATION="true")
        self.assertEqual([p for p, _ in self.requests if "releases/tags/" in p],
                         [self.prefix + "releases/tags/v0.3.0-rc.1"] * 3)

    def test_moving_major_never_resolves_a_prerelease(self):
        (self.action / "Cargo.toml").write_text('[workspace.package]\nversion = "0.3.0-rc.1"\n')
        self.add_release(version="0.3.0-rc.1")
        self.run_install("source", ACTION_REF="v0", version="0.3.0-rc.1")
        self.assert_requests()
        self.run_install(error="does not match the moving major tag", INSTALL="release", ACTION_REF="v0")

    def test_malformed_prerelease_versions_fail(self):
        for version in ("0.3.0-", "0.3.0-rc..1", "0.3.0+build", "0.3.0-rc/1"):
            with self.subTest(version=version):
                self.run_install(error="version must be", VERSION=version)

    def test_unauthenticated_gh_on_ghes_falls_back_to_source(self):
        # gh cannot verify attestations anonymously; it exits 4 without github.com credentials.
        self.gh(4)
        ghes = dict(GITHUB_API_URL="https://ghe.example/api/v3", GITHUB_SERVER_URL="https://ghe.example",
                    VERIFY_ATTESTATION="true")
        self.run_install("source", **ghes)
        self.assertIn("needs gh authenticated to github.com", self.result.stdout)
        self.assertIn("verify-attestation: false", self.result.stdout)
        self.run_install(error="needs gh authenticated to github.com", INSTALL="release", **ghes)

    def test_gh_auth_failure_with_runner_token_is_fatal(self):
        self.gh(4)
        self.run_install(error="returned non-zero exit status 4", VERIFY_ATTESTATION="true")

    def test_release_install_path_runs_plan_and_stages_artifact(self):
        # Package a stub CLI with the real release packager, serve it as a mocked
        # release, install it as the action does, then run the plan and stage the
        # artifact with the installed binary.
        stub = self.work / "stub-cli"
        stub.write_text(textwrap.dedent('''\
            #!/usr/bin/env bash
            set -eu
            if [ "$1" != run ]; then exit 0; fi
            output=$4
            mkdir -p "$output/run-release/plugins/p"
            echo '# released CLI' > "$output/run-release/summary.md"
            echo '{"state": "completed"}' > "$output/run-release/execution.json"
            echo '{"event": "completed"}' > "$output/run-release/events.jsonl"
            echo '<testsuites/>' > "$output/run-release/junit.xml"
            echo '[redacted]' > "$output/run-release/plugins/p/stderr.log"
            echo '{"key": "secret"}' > "$output/run-release/plugins/p/raw.json"
            '''))
        stub.chmod(0o755)
        packaged = self.work / "packaged"
        subprocess.run([sys.executable, str(ACTION.parent / "scripts/package-cli.py"), "0.3.0",
                        "x86_64-unknown-linux-gnu", str(stub), "--output", str(packaged)],
                       cwd=ACTION.parent, check=True, capture_output=True)
        for path in packaged.iterdir():
            self.routes[f"/assets/{path.name}"] = path.read_bytes()
        self.payload = stub.read_bytes()
        self.gh()
        self.run_install(INSTALL="release", VERIFY_ATTESTATION="true")
        bindir = (self.work / "path").read_text().strip()
        env = dict(self.env, PATH=f"{bindir}{os.pathsep}{self.bin}{os.pathsep}{os.environ['PATH']}",
                   GITHUB_ENV=str(self.work / "env"), PLAN="test.xml", PLUGINS="", CREDENTIALS="",
                   TOPOLOGIES="", CAPABILITIES="", OUTPUT=str(self.work / "runs"), FAIL_ON="error",
                   PLUGIN_CRATES="")
        (self.work / "outputs").unlink()
        result = subprocess.run(["bash", "-euo", "pipefail", "-c", shell_block("Run the plan")],
                                env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        outputs = dict(line.split("=", 1) for line in (self.work / "outputs").read_text().splitlines())
        self.assertEqual(outputs["exit-code"], "0")
        self.assertIn("# released CLI", (self.work / "summary").read_text())
        (self.work / "outputs").unlink()
        env.update(GITHUB_ACTION_PATH=str(ACTION.parent), RUN_DIR=outputs["run-dir"], ARTIFACT_NAME="",
                   UPLOAD="true", RAW_PLUGIN_FILES="false", SIZE_LIMIT_MB="500", RETENTION_DAYS="",
                   GITHUB_JOB="test", GITHUB_RUN_ATTEMPT="1", GITHUB_ACTION="step", RUNNER_NAME="runner")
        result = subprocess.run(["bash", "-euo", "pipefail", "-c", shell_block("Stage the run artifact")],
                                env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        staged = dict(line.split("=", 1) for line in (self.work / "outputs").read_text().splitlines())
        root = Path(staged["artifact-path"])
        self.assertEqual(sorted(p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file()),
                         ["events.jsonl", "execution.json", "junit.xml", "plugins/p/stderr.log", "summary.md"])


class ArtifactTest(unittest.TestCase):
    """The artifact staging step: allowlisted reports by default, raw files on request."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.work = Path(directory.name)
        self.run_dir = self.work / "polkameter-runs/run-1-2-3"
        files = {
            "execution.json": '{"state": "completed", "exit_code": 0}', "summary.md": "# plan\n",
            "summary.json": "{}", "junit.xml": "<testsuites/>",
            "plan.json": "{}", "resolved-plan.json": "{}", "plots/throughput.svg": "<svg/>",
            "events.jsonl": '{"event": "phase"}\n' * 9 + COMPLETED, "samples.jtl": "row\n" * 10,
            "steps.jsonl": "{}\n",
            "transactions.jsonl": "x" * 4000, "plugins/example/stderr.log": "[redacted]\n",
            # Plugin-written files: PolkaMeter never redacts these.
            "plugins/example/diagnostic.json": '{"suri": "//Alice secret"}',
            "plugins/example/series.jsonl": "{}\n", "plugin-dump.txt": "secret",
            "plots/nested/extra.svg": "<svg/>",
        }
        for name, content in files.items():
            path = self.run_dir / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        (self.work / "outside-secret").write_text("do not upload")
        (self.run_dir / "events-link.jsonl").symlink_to(self.work / "outside-secret")
        (self.run_dir / "plugins/linked").symlink_to(self.work, target_is_directory=True)
        self.env = dict(os.environ, GITHUB_ACTION_PATH=str(ACTION.parent), RUNNER_TEMP=str(self.work),
                        GITHUB_OUTPUT=str(self.work / "outputs"), RUN_DIR=str(self.run_dir),
                        ARTIFACT_NAME="", UPLOAD="true", RAW_PLUGIN_FILES="false", SIZE_LIMIT_MB="500",
                        RETENTION_DAYS="", GITHUB_SERVER_URL="https://github.com", GITHUB_JOB="load-test",
                        GITHUB_RUN_ATTEMPT="1", GITHUB_ACTION="__agustinustheo_polkameter",
                        RUNNER_NAME="GitHub Actions 7")

    def stage(self, code=0, block="Stage the run artifact", **env):
        (self.work / "outputs").unlink(missing_ok=True)
        result = subprocess.run(["bash", "-euo", "pipefail", "-c", shell_block(block)],
                                env=dict(self.env, **env), capture_output=True, text=True)
        self.assertEqual(result.returncode, code, result.stdout + result.stderr)
        self.result = result
        if code or block != "Stage the run artifact":
            return None
        outputs = dict(line.split("=", 1) for line in (self.work / "outputs").read_text().splitlines())
        self.outputs = outputs
        root = Path(outputs["artifact-path"])
        return sorted(p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file())

    def test_default_uploads_only_polkameter_reports(self):
        files = self.stage()
        self.assertEqual(files, sorted([
            "execution.json", "summary.md", "summary.json", "junit.xml", "plan.json", "resolved-plan.json",
            "plots/throughput.svg", "events.jsonl", "samples.jtl", "steps.jsonl", "transactions.jsonl",
            "plugins/example/stderr.log"]))
        self.assertEqual(self.outputs["upload"], "true")
        self.assertIn("excludes 4 file(s) that PolkaMeter did not write", self.result.stdout)
        self.assertIn('upload-raw-plugin-files: "true"', self.result.stdout)

    def test_raw_plugin_files_require_opt_in_and_never_follow_links(self):
        files = self.stage(RAW_PLUGIN_FILES="true")
        self.assertIn("plugins/example/diagnostic.json", files)
        self.assertIn("plugin-dump.txt", files)
        self.assertNotIn("events-link.jsonl", files)
        self.assertFalse(any(f.startswith("plugins/linked") for f in files))
        self.assertNotIn("excludes", self.result.stdout)

    def test_size_limit_keeps_reports_and_lists_omitted_records(self):
        (self.run_dir / "transactions.jsonl").write_bytes(b"x" * (2 * 1024 * 1024))
        files = self.stage(SIZE_LIMIT_MB="1")
        self.assertIn("summary.md", files)
        self.assertIn("events.jsonl", files)
        self.assertNotIn("transactions.jsonl", files)
        omitted = Path(self.outputs["artifact-path"]) / "polkameter-artifact-omitted.txt"
        self.assertIn("transactions.jsonl", omitted.read_text())
        self.assertIn("::warning::The run artifact omits 1 file(s)", self.result.stdout)
        self.assertIn("transactions.jsonl", self.stage(SIZE_LIMIT_MB="0"))

    def test_default_name_is_unique_per_matrix_job_and_step(self):
        names = set()
        for env in ({}, dict(RUNNER_NAME="GitHub Actions 8"), dict(GITHUB_ACTION="__agustinustheo_polkameter_2"),
                    dict(GITHUB_RUN_ATTEMPT="2"), dict(GITHUB_JOB="other")):
            self.stage(**env)
            name = self.outputs["artifact-name"]
            self.assertRegex(name, r"^polkameter-run-[A-Za-z0-9_.-]+-[0-9]+-[0-9a-f]{10}$")
            names.add(name)
        self.assertEqual(len(names), 5)
        self.stage()
        self.assertIn(self.outputs["artifact-name"], names)

    def test_explicit_name_is_kept(self):
        self.stage(ARTIFACT_NAME="load-results")
        self.assertEqual(self.outputs["artifact-name"], "load-results")

    def test_ghes_skips_upload_with_notice_but_stages_files(self):
        files = self.stage(GITHUB_SERVER_URL="https://ghe.example")
        self.assertEqual(self.outputs["upload"], "false")
        self.assertIn("summary.md", files)
        self.assertIn("not supported on GitHub Enterprise Server", self.result.stdout)
        self.stage(GITHUB_SERVER_URL="https://octo.ghe.com")
        self.assertEqual(self.outputs["upload"], "true")

    def test_invalid_artifact_inputs_fail_before_the_run(self):
        for env, message in ((dict(UPLOAD="yes"), "upload must be true or false"),
                             (dict(RAW_PLUGIN_FILES="1"), "upload-raw-plugin-files must be true or false"),
                             (dict(SIZE_LIMIT_MB="-1"), "artifact-size-limit-mb"),
                             (dict(RETENTION_DAYS="91"), "artifact-retention-days"),
                             (dict(RETENTION_DAYS="x"), "artifact-retention-days")):
            with self.subTest(env=env):
                self.stage(1, block="Validate artifact inputs", **env)
                self.assertIn(message, self.result.stderr)
        self.stage(block="Validate artifact inputs", RETENTION_DAYS="7")
        action = ACTION.read_text()
        self.assertLess(action.index("- name: Validate artifact inputs"), action.index("- name: Run the plan"))

    def test_upload_step_uses_staged_allowlist(self):
        action = ACTION.read_text()
        upload = action.split("    - name: Upload the run\n", 1)[1].split("\n    - ", 1)[0]
        self.assertIn("if: always() && steps.stage.outputs.upload == 'true'", upload)
        self.assertIn("name: ${{ steps.stage.outputs.artifact-name }}", upload)
        self.assertIn("path: ${{ steps.stage.outputs.artifact-path }}", upload)
        self.assertIn("retention-days: ${{ inputs.artifact-retention-days }}", upload)
        self.assertNotIn("steps.run.outputs.run-dir", upload)
        self.assertIn('default: ""', action.split("  artifact-name:\n", 1)[1].split("  artifact-retention-days:", 1)[0])

    def stager(self):
        # A real module, so replacing walk also changes what stage() calls.
        spec = importlib.util.spec_from_file_location(
            "stage_artifact", ACTION.parent / "scripts/action-stage-artifact.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module.__dict__

    def make_unfinished(self, execution):
        sentinel = "credential-sentinel-0451"
        (self.run_dir / "plugins/example/stderr.log").write_text(f"login with {sentinel}\n")
        (self.run_dir / "summary.md").write_text(f"check failed: {sentinel}\n")
        (self.run_dir / "events.jsonl").write_text('{"event": "phase", "phase": "setup"}\n')
        if execution is None:
            (self.run_dir / "execution.json").unlink()
        else:
            (self.run_dir / "execution.json").write_text(execution)
        return sentinel

    def assert_withheld(self, files, sentinel):
        root = Path(self.outputs["artifact-path"])
        for name in ("plugins/example/stderr.log", "summary.md", "summary.json"):
            self.assertNotIn(name, files)
        self.assertIn("events.jsonl", files)
        self.assertFalse(any(sentinel in p.read_text() for p in root.rglob("*") if p.is_file()))
        self.assertIn("::notice::The run did not finish normally", self.result.stdout)
        self.assertIn("plugins/example/stderr.log", self.result.stdout)

    def test_killed_run_withholds_files_redacted_only_at_finish(self):
        # Exit 137: SIGKILL or the OOM killer stopped the CLI before it wrote its outcome.
        sentinel = self.make_unfinished(None)
        self.assert_withheld(self.stage(), sentinel)
        self.assert_withheld(self.stage(RAW_PLUGIN_FILES="true"), sentinel)

    def test_forced_stop_without_final_event_withholds_files(self):
        # A second interrupt during teardown: execution.json may exist, the final event does not.
        sentinel = self.make_unfinished('{"state": "stopped", "exit_code": 130}')
        self.assert_withheld(self.stage(), sentinel)

    def test_unfinished_state_or_malformed_records_withhold_files(self):
        for execution, events in (('{"state": "running"}', COMPLETED), ("{", COMPLETED),
                                  ('{"state": "completed"}', COMPLETED + "{"),
                                  ('{"state": "completed"}', "")):
            with self.subTest(execution=execution, events=events):
                (self.run_dir / "execution.json").write_text(execution)
                (self.run_dir / "events.jsonl").write_text(events)
                self.assertNotIn("plugins/example/stderr.log", self.stage())

    def test_finished_run_check(self):
        script = str(ACTION.parent / "scripts/action-stage-artifact.py")
        self.assertEqual(subprocess.run([sys.executable, script, "--finished", str(self.run_dir)]).returncode, 0)
        self.make_unfinished(None)
        self.assertEqual(subprocess.run([sys.executable, script, "--finished", str(self.run_dir)]).returncode, 1)

    def test_staged_files_are_independent_copies(self):
        self.stage()
        staged = Path(self.outputs["artifact-path"]) / "events.jsonl"
        self.assertEqual(staged.stat().st_nlink, 1)
        before = staged.read_text()
        with (self.run_dir / "events.jsonl").open("a") as events:
            events.write("written by a surviving plugin\n")
        self.assertEqual(staged.read_text(), before)

    def test_growth_after_selection_cannot_exceed_budget(self):
        module = self.stager()
        original = module["walk"]

        def walk_then_grow(directory_fd, prefix=""):
            found = original(directory_fd, prefix)
            if not prefix:
                with (self.run_dir / "transactions.jsonl").open("a") as grown:
                    grown.write("y" * 100000)
            return found

        module["walk"] = walk_then_grow
        destination = self.work / "snapshot"
        module["stage"](str(self.run_dir), destination, False, 0)
        self.assertEqual((destination / "transactions.jsonl").read_text(), "x" * 4000)

    def test_directory_swapped_for_link_after_selection_is_not_followed(self):
        outside = self.work / "outside"
        (outside / "example").mkdir(parents=True)
        (outside / "example/stderr.log").write_text("outside sentinel")
        module = self.stager()
        original = module["walk"]

        def walk_then_swap(directory_fd, prefix=""):
            found = original(directory_fd, prefix)
            if not prefix:
                shutil.rmtree(self.run_dir / "plugins")
                (self.run_dir / "plugins").symlink_to(outside, target_is_directory=True)
                (self.run_dir / "summary.md").unlink()
                (self.run_dir / "summary.md").symlink_to(outside / "example/stderr.log")
            return found

        module["walk"] = walk_then_swap
        destination = self.work / "snapshot"
        staged, _, _, _ = module["stage"](str(self.run_dir), destination, True, 0)
        self.assertNotIn("plugins/example/stderr.log", staged)
        self.assertNotIn("summary.md", staged)
        self.assertFalse(any("outside sentinel" in p.read_text() for p in destination.rglob("*") if p.is_file()))


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
