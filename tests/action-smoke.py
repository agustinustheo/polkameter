#!/usr/bin/env python3
"""Run the composite action's shell blocks with a stub CLI, without GitHub or a chain."""
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest

ACTION = Path(__file__).resolve().parents[1] / "action.yml"


def shell_block(name):
    step = ACTION.read_text().split(f"    - name: {name}\n", 1)[1]
    step = step.split("\n    - ", 1)[0]
    return textwrap.dedent(step.split("      run: ", 1)[1].removeprefix("|\n"))


class ActionTest(unittest.TestCase):
    def run_action(self, policy, verdict, expected, runtime_code=0, *, malformed=False,
                   preexisting=False):
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
                       PLAN="test.xml", PLUGINS="", CREDENTIALS="", TOPOLOGIES="", CAPABILITIES="",
                       OUTPUT=str(work / "runs"), FAIL_ON=policy, TEST_VERDICT=verdict,
                       TEST_CODE=str(runtime_code), MALFORMED=str(malformed).lower(),
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

    def test_invalid_policy(self):
        self.assertLess(ACTION.read_text().index("- name: Validate failure policy"),
                        ACTION.read_text().index("- name: Build PolkaMeter"))
        self.run_action("invalid", "", 2)


if __name__ == "__main__":
    unittest.main()
