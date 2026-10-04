#!/usr/bin/env python3
"""Fail if feature unification reintroduces nested proof thread pools."""
import json
import subprocess
metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--locked"]))
names = {package["id"]: package["name"] for package in metadata["packages"]}
for node in metadata["resolve"]["nodes"]:
    if names[node["id"]] == "ark-vrf" and "parallel" in node["features"]:
        raise SystemExit("ark-vrf/parallel must remain disabled; see docs/plugins.md")
print("Proof feature guard passed")
