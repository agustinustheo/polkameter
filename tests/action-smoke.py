#!/usr/bin/env python3
"""Run the composite action's shell blocks with a stub CLI, without GitHub or a chain."""
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
import shutil
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
        (self.bin / "git").symlink_to(shutil.which("git"))
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
        self.release = self.add_release()

    def route(self, path, value):
        self.routes[self.prefix + path] = value

    def add_release(self, version="0.3.0", target="x86_64-unknown-linux-gnu", windows=False):
        name = f"polkameter-{version}-{target}." + ("zip" if windows else "tar.gz")
        data = io.BytesIO()
        if windows:
            with zipfile.ZipFile(data, "w") as archive:
                archive.writestr("polkameter.exe", b"fake binary")
        else:
            with tarfile.open(fileobj=data, mode="w:gz") as archive:
                member = tarfile.TarInfo("polkameter")
                member.size = len(b"fake binary")
                archive.addfile(member, io.BytesIO(b"fake binary"))
        content = data.getvalue()
        self.routes[f"/assets/{name}"] = content
        self.routes[f"/assets/{name}.sha256"] = f"{hashlib.sha256(content).hexdigest()}  {name}\n".encode()
        release = dict(tag_name=f"v{version}", draft=False, assets=[
            dict(name=n, url=f"{self.api}/assets/{n}") for n in (name, f"{name}.sha256")])
        self.route(f"releases/tags/v{version}", release)
        return release

    def tag(self, name, sha="action-commit", kind="commit"):
        self.route(f"git/ref/tags/{name}", {"object": {"type": kind, "sha": sha}})

    def run_install(self, expected=0, **env):
        self.env.update(env)
        (self.work / "path").unlink(missing_ok=True)
        (self.work / "outputs").unlink(missing_ok=True)
        result = subprocess.run(["/bin/bash", "-euo", "pipefail", "-c",
                                 shell_block("Install released PolkaMeter or select source")],
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        if expected:
            self.assertFalse((self.work / "path").exists())
            return result
        outputs = dict(line.split("=", 1) for line in (self.work / "outputs").read_text().splitlines())
        if outputs["install-method"] == "release":
            binary = "polkameter.exe" if self.env["RUNNER_OS"] == "Windows" else "polkameter"
            installed = Path((self.work / "path").read_text().strip()) / binary
            self.assertEqual(installed.read_bytes(), b"fake binary")
            self.assertTrue(os.access(installed, os.X_OK))
        else:
            self.assertFalse((self.work / "path").exists())
        return outputs

    def gh(self, code=0):
        stub = self.bin / "gh"
        stub.write_text(f'#!/bin/sh\nprintf "%s\\n" "$@" > "{self.work}/gh-args"\nexit {code}\n')
        stub.chmod(0o755)

    def test_full_tag_downloads_authenticated_archive_and_checksum(self):
        self.assertEqual(self.run_install(), {"version": "0.3.0", "install-method": "release", "needs-toolchain": "false"})
        self.assertEqual(len(self.requests), 3)
        self.assertTrue(all(auth == "Bearer test-runner-token" for _, auth in self.requests))

    def test_moving_major_resolves_matching_release_not_latest(self):
        self.tag("v0")
        self.tag("v0.3.0")
        newer = dict(tag_name="v0.4.0", draft=False)
        self.tag("v0.4.0", "other-commit")
        self.route("releases?per_page=100&page=1", [newer, self.release])
        self.assertEqual(self.run_install(ACTION_REF="v0")["version"], "0.3.0")

    def test_moving_tag_ignores_deleted_release_tags(self):
        self.tag("v0")
        self.tag("v0.3.0")
        deleted = dict(tag_name="v0.4.0", draft=False)
        self.route("releases?per_page=100&page=1", [deleted, self.release])
        self.assertEqual(self.run_install(ACTION_REF="v0")["install-method"], "release")

    def test_moving_tag_paginates_releases(self):
        self.tag("v0")
        self.tag("v0.3.0")
        self.route("releases?per_page=100&page=1", [dict(tag_name="v1.0.0", draft=False)] * 100)
        self.route("releases?per_page=100&page=2", [self.release])
        self.assertEqual(self.run_install(ACTION_REF="v0")["install-method"], "release")

    def test_annotated_moving_tag(self):
        self.tag("v0", "annotation", "tag")
        self.route("git/tags/annotation", {"object": {"type": "commit", "sha": "action-commit"}})
        self.tag("v0.3.0")
        self.route("releases?per_page=100&page=1", [self.release])
        self.assertEqual(self.run_install(ACTION_REF="v0")["install-method"], "release")

    def test_moving_tag_package_version_fallback(self):
        self.tag("v0")
        self.tag("v0.3.0")
        self.route("releases?per_page=100&page=1", [])
        self.assertEqual(self.run_install(ACTION_REF="v0")["install-method"], "release")

    def test_moving_tag_mismatched_package_commit_uses_source(self):
        self.tag("v0")
        self.tag("v0.3.0", "other-commit")
        self.route("releases?per_page=100&page=1", [])
        self.assertEqual(self.run_install(ACTION_REF="v0")["install-method"], "source")

    def test_checkout_commit_takes_precedence_over_moved_major_tag(self):
        subprocess.run(["git", "init", "-q", str(self.action)], check=True)
        subprocess.run(["git", "-C", str(self.action), "-c", "user.name=Test", "-c",
                        "user.email=test@example.com", "commit", "-q", "--allow-empty", "-m", "test"], check=True)
        commit = subprocess.check_output(["git", "-C", str(self.action), "rev-parse", "HEAD"], text=True).strip()
        self.tag("v0", "moved-commit")
        self.tag("v0.3.0", commit)
        self.route("releases?per_page=100&page=1", [self.release])
        self.assertEqual(self.run_install(ACTION_REF="v0")["install-method"], "release")
        self.assertNotIn(self.prefix + "git/ref/tags/v0", [path for path, _ in self.requests])

    def test_branch_sha_and_local_refs_build_source(self):
        for ref in ("main", "abc123def", ""):
            with self.subTest(ref=ref):
                self.assertEqual(self.run_install(ACTION_REF=ref)["install-method"], "source")
        self.assertEqual(self.requests, [])

    def test_explicit_version_overrides_branch(self):
        self.assertEqual(self.run_install(ACTION_REF="main", VERSION="0.3.0")["install-method"], "release")

    def test_forced_source_skips_network_and_needs_toolchain(self):
        self.assertEqual(self.run_install(INSTALL="source")["needs-toolchain"], "true")
        self.assertEqual(self.requests, [])

    def test_missing_release_falls_back(self):
        self.assertEqual(self.run_install(ACTION_REF="v0.2.0")["install-method"], "source")

    def test_missing_asset_falls_back(self):
        self.release["assets"].pop()
        self.assertEqual(self.run_install()["install-method"], "source")

    def test_missing_asset_forced_release_fails(self):
        self.release["assets"] = []
        self.run_install(1, INSTALL="release")

    def test_forced_release_without_release_ref_fails(self):
        self.run_install(1, INSTALL="release", ACTION_REF="main")

    def test_bad_checksum_fails_even_in_auto(self):
        name = self.release["assets"][0]["name"]
        self.routes[f"/assets/{name}.sha256"] = f'{"0" * 64}  {name}\n'.encode()
        self.assertIn("Checksum verification failed", self.run_install(1).stderr)

    def test_attestation_disabled_does_not_call_gh(self):
        self.gh(1)
        self.run_install()
        self.assertFalse((self.work / "gh-args").exists())

    def test_attestation_required(self):
        self.gh()
        self.run_install(VERIFY_ATTESTATION="true")
        args = (self.work / "gh-args").read_text().splitlines()
        self.assertEqual(args[:2], ["attestation", "verify"])
        self.assertEqual(args[-2:], ["--repo", "agustinustheo/polkameter"])
        self.assertTrue(Path(args[2]).exists())

    def test_attestation_default_enabled_when_gh_exists(self):
        self.gh(1)
        self.run_install(1, VERIFY_ATTESTATION="")

    def test_attestation_failure_is_fatal(self):
        self.gh(1)
        self.run_install(1, VERIFY_ATTESTATION="true")

    def test_attestation_requested_without_gh_fails(self):
        self.run_install(1, VERIFY_ATTESTATION="true")

    def test_default_without_gh_uses_checksum(self):
        self.run_install(VERIFY_ATTESTATION="")

    def test_download_redirect_strips_token_for_storage_host(self):
        name = self.release["assets"][0]["name"]
        content = self.routes[f"/assets/{name}"]
        self.routes[f"/assets/{name}"] = (f"http://localhost:{self.server.server_port}/storage",)
        self.routes["/storage"] = content
        self.run_install()
        self.assertIn(("/storage", None), self.requests)

    def test_moving_tag_package_version_when_listing_unavailable(self):
        self.tag("v0")
        self.tag("v0.3.0")
        self.assertEqual(self.run_install(ACTION_REF="v0")["install-method"], "release")

    def test_moving_tag_package_version_with_unknown_commit(self):
        self.assertEqual(self.run_install(ACTION_REF="v0")["install-method"], "release")

    def test_package_version_can_be_declared_directly(self):
        (self.action / "crates/cli/Cargo.toml").write_text('[package]\nversion = "0.4.0"\n')
        self.add_release(version="0.4.0")
        self.assertEqual(self.run_install(ACTION_REF="v0")["version"], "0.4.0")

    def test_forced_release_with_missing_release_fails(self):
        self.run_install(1, ACTION_REF="v0.2.0", INSTALL="release")

    def test_source_build_keeps_action_target_and_puts_cli_on_path(self):
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
        self.assertEqual(self.run_install(PLUGIN_CRATES="plugins/example:example")["needs-toolchain"], "true")

    def test_all_release_platforms(self):
        for system, arch, target in (("Linux", "ARM64", "aarch64-unknown-linux-gnu"),
                                     ("macOS", "X64", "x86_64-apple-darwin"),
                                     ("macOS", "ARM64", "aarch64-apple-darwin"),
                                     ("Windows", "X64", "x86_64-pc-windows-msvc")):
            with self.subTest(system=system, arch=arch):
                self.add_release(target=target, windows=system == "Windows")
                self.assertEqual(self.run_install(RUNNER_OS=system, RUNNER_ARCH=arch)["install-method"], "release")

    def test_unsupported_platform_uses_source(self):
        self.assertEqual(self.run_install(RUNNER_ARCH="ARM")["install-method"], "source")

    def test_invalid_inputs_fail(self):
        for env in (dict(INSTALL="bad"), dict(VERSION="bad"), dict(VERIFY_ATTESTATION="bad")):
            with self.subTest(env=env):
                self.env.update(INSTALL="auto", VERSION="", VERIFY_ATTESTATION="false")
                self.run_install(1, **env)

    def test_toolchain_and_build_steps_are_gated(self):
        action = ACTION.read_text()
        self.assertEqual(action.count("if: steps.install.outputs.needs-toolchain == 'true'"), 1)
        self.assertEqual(action.count("if: steps.install.outputs.install-method == 'source'"), 2)
        self.assertIn(".polkameter-action-target", shell_block("Build PolkaMeter"))

    def test_plugin_package_paths_and_binaries_with_spaces(self):
        manifest = self.work / "plugin directory"
        manifest.mkdir()
        (manifest / "Cargo.toml").touch()
        self.env.update(PLUGIN_CRATES=f"  {manifest}:example  \n\n{manifest}/Cargo.toml",
                        TOOLCHAIN="1.93.0")
        cargo = self.bin / "cargo"
        payload = json.dumps({"reason": "compiler-artifact", "target": {"kind": ["bin"]},
                              "executable": "/a path/plugin"})
        cargo.write_text(f'''#!/bin/sh
printf '%s\\n' "$@" >> "{self.work}/cargo-args"
echo '{payload}'
''')
        cargo.chmod(0o755)
        cli = self.bin / "polkameter"
        cli.write_text('#!/bin/sh\nprintf "%s\\n" "$@" >> "' + str(self.work / "plugin-args") + '"\n')
        cli.chmod(0o755)
        result = subprocess.run([sys.executable, str(ACTION.parent / "scripts/action-build-plugins.py")],
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        args = (self.work / "cargo-args").read_text().splitlines()
        self.assertEqual(args.count("--release"), 2)
        self.assertEqual(args.count("--package"), 1)
        self.assertIn(str(manifest / "Cargo.toml"), args)
        self.assertEqual((self.work / "plugin-args").read_text().splitlines(),
                         ["plugin", "install", "/a path/plugin"] * 2)


if __name__ == "__main__":
    unittest.main()
