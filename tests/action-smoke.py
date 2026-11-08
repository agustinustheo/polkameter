#!/usr/bin/env python3
"""Run the composite action's shell blocks with a stub CLI, without GitHub or a chain."""
import hashlib
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import unittest
import zipfile

ACTION = Path(__file__).resolve().parents[1] / "action.yml"
STUB_CLI = r"""#!/usr/bin/env bash
set -eu
if [ "$1" != run ]; then exit 0; fi
shift
flags=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output) output=$2; shift ;;
    --fail-on-*) flags="$flags $1" ;;
  esac
  shift
done
test "$flags" = "$EXPECT_FLAGS"
run="$output/run-1-2-3"
mkdir -p "$run/plugins/p" "$run/plots"
echo 'test summary' > "$run/summary.md"
echo '[redacted]' > "$run/plugins/p/stderr.log"
echo '{"suri": "//Alice"}' > "$run/plugins/p/raw.json"
echo 'plugin output' > "$run/plugin-dump.txt"
echo '<svg/>' > "$run/plots/rate.svg"
if [ -n "$TEST_LINK" ]; then ln -s "$TEST_LINK" "$run/plugins/p/link"; fi
if [ "$TEST_VERDICT" = MALFORMED ]; then
  printf '{' > "$run/summary.json"
elif [ -n "$TEST_VERDICT" ]; then
  printf '{"verdict":{"result":"%s"}}\n' "$TEST_VERDICT" > "$run/summary.json"
fi
if [ "$TEST_KILLED" = true ]; then
  echo '{"event": "phase"}' > "$run/events.jsonl"
  exit 137
fi
code=$TEST_CODE
reason=""
if [ "$code" = 0 ] && { [[ "$flags" = *verdict* && "$TEST_VERDICT" = FAIL ]] \
    || [[ "$flags" = *inconclusive* && "$TEST_VERDICT" = INCONCLUSIVE ]]; }; then
  code=1
  reason="verdict $TEST_VERDICT: 50% lost; exit 1 because of a flag"
fi
jq -n --arg reason "$reason" '{exit_reason: ($reason | select(. != ""))}' > "$run/execution.json"
echo '<testsuites/>' > "$run/junit.xml"
echo '{"event": "completed"}' > "$run/events.jsonl"
exit "$code"
"""
STUB_INSTALLER = """\
printf '%s' "$1" > "$POLKAMETER_INSTALL_DIR.args"
mkdir -p "$POLKAMETER_INSTALL_DIR"
printf '#!/bin/sh\\necho "polkameter 0.4.1"\\n' > "$POLKAMETER_INSTALL_DIR/polkameter"
chmod +x "$POLKAMETER_INSTALL_DIR/polkameter"
"""


def shell_block(name):
    step = ACTION.read_text().split(f"    - name: {name}\n", 1)[1]
    step = step.split("\n    - ", 1)[0]
    return textwrap.dedent(step.split("      run: ", 1)[1].removeprefix("|\n"))


def outputs(path):
    """Reads a GITHUB_OUTPUT file, including name<<DELIMITER values."""
    values, lines = {}, iter(path.read_text().splitlines())
    for line in lines:
        if "=" not in line:
            name, delimiter = line.split("<<", 1)
            values[name] = "\n".join(iter(lambda: next(lines), delimiter))
        else:
            name, value = line.split("=", 1)
            values[name] = value
    return values


class RunTest(unittest.TestCase):
    def run_plan(self, policy, verdict="", code=0, *, killed=False, link="", artifact_name=""):
        """Runs the run step with a stub CLI; returns (step result, outputs, job exit code)."""
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.work = work = Path(directory.name)
        cli = work / "polkameter"
        cli.write_text(STUB_CLI)
        cli.chmod(0o755)
        flags = {"verdict": " --fail-on-verdict",
                 "verdict-or-inconclusive": " --fail-on-verdict --fail-on-inconclusive"}.get(policy, "")
        env = dict(os.environ, PATH=f"{work}:{os.environ['PATH']}", RUNNER_TEMP=str(work),
                   GITHUB_ENV=str(work / "env"), GITHUB_OUTPUT=str(work / "outputs"),
                   GITHUB_STEP_SUMMARY=str(work / "summary"), GITHUB_JOB="load", GITHUB_RUN_ATTEMPT="2",
                   PLAN="test.xml", PLUGINS="", CREDENTIALS="", OUTPUT=str(work / "runs"),
                   FAIL_ON=policy, ARTIFACT_NAME=artifact_name, EXPECT_FLAGS=flags,
                   TEST_VERDICT=verdict, TEST_CODE=str(code), TEST_KILLED=str(killed).lower(), TEST_LINK=link)
        result = subprocess.run(["bash", "-euo", "pipefail", "-c", shell_block("Run the plan")],
                                env=env, capture_output=True, text=True)
        if policy == "invalid":
            return result, None, None
        self.assertEqual(result.returncode, 0, result.stderr)
        values = outputs(work / "outputs")
        job = subprocess.run(["bash", "-c", shell_block("Fail the job if the plan failed")],
                             env=dict(env, EXIT_CODE=values["exit-code"])).returncode
        return result, values, job

    def test_policy_verdict_and_runtime_code_decide_the_exit_code(self):
        inputs = ACTION.read_text().split("\n  fail-on:\n", 1)[1].split("\n  upload:\n", 1)[0]
        self.assertTrue(inputs.endswith("\n    default: verdict-or-inconclusive"), inputs)
        for policy, failing in (("error", ()), ("verdict", ("FAIL",)),
                                ("verdict-or-inconclusive", ("FAIL", "INCONCLUSIVE"))):
            for verdict in ("PASS", "WARN", "FAIL", "INCONCLUSIVE", ""):
                with self.subTest(policy=policy, verdict=verdict):
                    result, values, job = self.run_plan(policy, verdict)
                    self.assertEqual(job, int(verdict in failing))
                    self.assertEqual(values["verdict"], verdict)
                    self.assertEqual(values["junit"], f"{values['run-dir']}/junit.xml")
                    self.assertEqual((self.work / "summary").read_text(), "test summary\n")
                    # The CLI's exit_reason, with % escaped for the workflow command.
                    self.assertEqual(f"::error title=PolkaMeter exited 1::verdict {verdict}: 50%25 lost"
                                     in result.stdout, verdict in failing)
            for code in (2, 130):
                with self.subTest(policy=policy, code=code):
                    self.assertEqual(self.run_plan(policy, "FAIL", code)[2], code)

    def test_invalid_policy_fails_before_the_run(self):
        result, _, _ = self.run_plan("invalid")
        self.assertEqual(result.returncode, 2)
        self.assertFalse((self.work / "runs").exists())

    def test_malformed_summary_warns(self):
        result, values, _ = self.run_plan("verdict", "MALFORMED")
        self.assertIn("::warning::Could not read verdict", result.stdout)
        self.assertEqual(values["verdict"], "")

    def test_artifact_lists_only_polkameter_files(self):
        _, values, _ = self.run_plan("error")
        self.assertEqual(values["artifact-name"], "polkameter-run-load-2-run-1-2-3")
        prefix = values["run-dir"] + "/"
        paths = values["artifact-paths"].splitlines()
        self.assertTrue(all(p.startswith(prefix) for p in paths))
        names = {p[len(prefix):] for p in paths}
        self.assertTrue({"execution.json", "summary.md", "plugins/*/stderr.log"} <= names)
        self.assertFalse({n for n in names if "raw" in n or "dump" in n})
        self.assertEqual(self.run_plan("error", artifact_name="mine")[1]["artifact-name"], "mine")

    def test_killed_run_withholds_files_redacted_at_finish(self):
        result, values, job = self.run_plan("verdict", killed=True)
        self.assertEqual(job, 137)
        self.assertIn("::notice::", result.stdout)
        self.assertFalse((self.work / "summary").exists())
        for name in ("summary.md", "summary.json", "plugin-checks.json", "stderr.log"):
            self.assertNotIn(name, values["artifact-paths"])
        self.assertIn("execution.json", values["artifact-paths"])

    def test_symbolic_link_skips_the_upload(self):
        result, values, _ = self.run_plan("error", link="/etc/passwd")
        self.assertIn("::warning::", result.stdout)
        self.assertNotIn("artifact-paths", values)
        upload = ACTION.read_text().split("- name: Upload the run\n", 1)[1]
        self.assertIn("if: always() && inputs.upload == 'true' && steps.run.outputs.artifact-paths", upload)


class InstallTest(unittest.TestCase):
    def install(self, version, *, error=False):
        """Runs the install step with a stub install.sh; returns the version it was given."""
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        work = Path(directory.name)
        action = work / "action"
        action.mkdir()
        (action / "Cargo.toml").write_text('[workspace]\n\n[workspace.package]\nversion = "0.4.1"\n')
        (action / "install.sh").write_text(STUB_INSTALLER)
        bindir = work / "bin"
        env = dict(os.environ, VERSION=version, GITHUB_ACTION_PATH=str(action), POLKAMETER_INSTALL_DIR=str(bindir),
                   GITHUB_OUTPUT=str(work / "outputs"), GITHUB_PATH=str(work / "path"))
        result = subprocess.run(["bash", "-eo", "pipefail", "-c", shell_block("Install PolkaMeter")],
                                env=env, capture_output=True, text=True)
        if error:
            self.assertEqual(result.returncode, 1)
            self.assertIn("::error::Use the action at a release tag", result.stdout)
            return None
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((work / "path").read_text(), f"{bindir}\n")
        self.assertEqual(outputs(work / "outputs"), {"version": "0.4.1"})
        return Path(f"{bindir}.args").read_text()

    def test_release_tags_and_versions_pass_through(self):
        for version in ("v0.3.0", "0.3.0", "v0.4.0-rc.1", "latest"):
            with self.subTest(version=version):
                self.assertEqual(self.install(version), version)

    def test_moving_major_tag_installs_the_package_version(self):
        self.assertEqual(self.install("v0"), "0.4.1")

    def test_branch_sha_and_local_refs_fail(self):
        for ref in ("", "main", "simplify/action", "0123456789abcdef0123456789abcdef01234567"):
            with self.subTest(ref=ref):
                self.install(ref, error=True)


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
                    expected = f"polkameter-0.3.0-{target}.{'zip' if 'windows' in target else 'tar.gz'}"
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
