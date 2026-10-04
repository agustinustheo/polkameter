#!/usr/bin/env python3
"""Compare retained upstream/migrated bundles; health differences remain visible for review."""
import argparse
import json
from pathlib import Path
parser = argparse.ArgumentParser()
parser.add_argument("upstream", type=Path)
parser.add_argument("migrated", type=Path)
args = parser.parse_args()
def workload(params):
    if "ramp" in params:
        ramp = params["ramp"]
        rates = [ramp["start"] * ramp["growth"] ** i if ramp.get("growth") else ramp["start"] + ramp["step"] * i for i in range(ramp["steps"])]
        return {"rates": rates, "seconds": [ramp["intervalS"]] * len(rates), "connections": params["connections"], "members": params["scenario"]["members"], "slots": params["scenario"]["slots"], "recoverySeconds": ramp["recoveryS"], "baselineProbes": ramp["probes"]}
    plan = params["plan"]
    load = plan["load"]
    rates = load["rate"]
    if not rates:
        ramp = load["ramp"]
        rates = [{"@tx-per-second": ramp["@start"] * ramp["@growth"] ** i if ramp.get("@growth") else ramp["@start"] + ramp["@step"] * i, "@seconds": ramp["@seconds"]} for i in range(ramp["@steps"])]
    literals = {item["@name"]: item["@value"] for step in plan["setup"]["step"] for item in step["input"] if item.get("@value") is not None}
    return {"rates": [r["@tx-per-second"] for r in rates], "seconds": [r["@seconds"] for r in rates], "connections": load["@connections"], "members": int(literals["members"]), "slots": int(literals["slots"]), "recoverySeconds": load["@recovery-seconds"], "baselineProbes": load["@baseline-probes"]}

def read(directory):
    summary = json.loads((directory / "summary.json").read_text())
    steps = [json.loads(line) for line in (directory / "steps.jsonl").read_text().splitlines()]
    return {
        "workload": workload(summary["params"]),
        "run": summary["runId"], "mode": summary["mode"], "network": summary["network"],
        "rates": [step["targetRate"] for step in steps],
        "sentPerSecond": [step["sentPerS"] for step in steps],
        "baselineProbes": summary["baseline"]["probes"],
        "loss": summary["loss"], "recovery": summary["recovery"], "stop": summary["stop"],
        "checks": {check["check"]: {key:check[key] for key in ("status","detail")} for check in summary["checks"]},
    }
a, b = read(args.upstream), read(args.migrated)
keys = ["sent", "included", "failedInBlock", "refused", "dropped", "inPool", "lost"]
report = {
    "sameWorkload": a["workload"] == b["workload"],
    "sameRequestedRates": a["workload"]["rates"] == b["workload"]["rates"],
    "sameExecutedRates": a["rates"] == b["rates"],
    "sameBaselineProbeCount": a["baselineProbes"] == b["baselineProbes"],
    "accounting": {key: {"upstream": a["loss"].get(key), "migrated": b["loss"].get(key)} for key in keys},
    "changedCheckStatuses": {name: {"upstream": a["checks"].get(name, {}).get("status"), "migrated": verdict["status"]}
                             for name, verdict in b["checks"].items() if a["checks"].get(name, {}).get("status") != verdict["status"]},
    "upstream": a, "migrated": b,
}
print(json.dumps(report, indent=2))
if not report["sameWorkload"] or not report["sameBaselineProbeCount"]:
    raise SystemExit(1)
