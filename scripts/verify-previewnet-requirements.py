#!/usr/bin/env python3
"""Validate exported requirements against the PreviewNet adapter before starting nodes."""
import json
import sys
from pathlib import Path
requirements = json.loads(Path(sys.argv[1]).read_text())
manifest = json.loads(Path(sys.argv[2]).read_text())
roles = {"collator", "collator-relay", "validator"}
for requirement in requirements.get("requirements", []):
    if not requirement.get("required"):
        continue
    kind = requirement["kind"]
    if kind not in {"metric", "rpc"}:
        raise SystemExit(f"PreviewNet adapter cannot supply required {kind}: {requirement['name']}")
    if kind == "metric" and requirement["target"] not in roles:
        raise SystemExit(f"Unsupported node role: {requirement['target']}")
monitors = requirements.get("monitorRequirements") or {}
for metric in monitors.get("metric", []):
    if metric["@role"] not in roles:
        raise SystemExit(f"Unsupported node role: {metric['@role']}")
print(json.dumps({"version": 1, "adapter": "previewnet", "bundle": manifest,
                  "requirements": requirements, "livePreflightRequired": True}, indent=2))
