#!/usr/bin/env python3
"""SIGTERM stops `polkameter calls` and every process it started, on the dev chain.

Two runs of the template call file, each sent SIGTERM: one during setup, while the call file's
runner (node) is alive, and one during the load. Each must exit within the shutdown grace period
(20 s) and leave no substrate plugin, no runner and no runner file behind.

Needs the dev chain (scripts/dev-chain.sh start), the CLI and the substrate plugin built next to
each other, and the template call file's dependencies with descriptors for the dev chain (npm
install and npx papi add dev -w ws://127.0.0.1:9944 in examples/dev-chain). From the repository root:

    tests/calls-signals.py target/debug/polkameter target/dev-chain-runs/calls-signals
"""

import pathlib
import signal
import subprocess
import sys
import time

GRACE_S = 20
CALLS = "examples/dev-chain/template.calls.ts"


def ours(pattern):
    """The pids of processes whose command line has `pattern`."""
    found = subprocess.run(["pgrep", "-f", pattern], capture_output=True, text=True)
    return {int(p) for p in found.stdout.split()}


def wait_for(what, check, timeout_s):
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.2)
    sys.exit(f"calls-signals: no {what} within {timeout_s} s")


def run(polkameter, out, when):
    out.mkdir(parents=True, exist_ok=True)
    log = out / f"{when}.log"
    args = [polkameter, "calls", "--chain", "ws://127.0.0.1:9944", "--seed", "//Alice",
            "--accounts", "4", "--rate", "2", "--duration", "60s", "--calls", CALLS,
            "--output", str(out)]
    before = ours("polkameter-substrate-plugin") | ours(".polkameter-calls-")
    with log.open("w") as sink:
        child = subprocess.Popen(args, stdout=sink, stderr=subprocess.STDOUT)
        if when == "setup":
            # The call file's runner, which setup starts after funding.
            wait_for("call file runner", lambda: ours(".polkameter-calls-") - before, 180)
        else:
            wait_for("load phase", lambda: "Phase: load" in log.read_text(), 240)
            time.sleep(3)
        started = time.monotonic()
        child.send_signal(signal.SIGTERM)
        try:
            code = child.wait(GRACE_S + 5)
        except subprocess.TimeoutExpired:
            child.kill()
            sys.exit(f"calls-signals: SIGTERM during {when}: still running after {GRACE_S + 5} s")
    took = time.monotonic() - started
    time.sleep(1)
    left = (ours("polkameter-substrate-plugin") | ours(".polkameter-calls-")) - before
    print(log.read_text()[-2000:])
    failures = []
    if took > GRACE_S:
        failures.append(f"exited {took:.1f} s after SIGTERM, more than the {GRACE_S} s grace")
    if code != 143:
        failures.append(f"exit code {code}, not 143")
    if left:
        failures.append(f"processes left behind: {sorted(left)}")
    runners = sorted(pathlib.Path(CALLS).parent.glob(".polkameter-calls-*.mjs"))
    if runners:
        failures.append(f"runner files left behind: {runners}")
    if failures:
        sys.exit(f"calls-signals: SIGTERM during {when}:\n  " + "\n  ".join(failures))
    print(f"calls-signals: ok: SIGTERM during {when}: exit {code} after {took:.1f} s, nothing left")


def main():
    polkameter, out = sys.argv[1], pathlib.Path(sys.argv[2])
    for when in ("setup", "load"):
        run(polkameter, out / when, when)


if __name__ == "__main__":
    main()
