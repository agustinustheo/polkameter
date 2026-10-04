#!/usr/bin/env python3
"""Measure JSONL round-trip overhead; preparation stays outside timed transaction load."""
import json
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
with tempfile.TemporaryDirectory() as directory:
    process = subprocess.Popen([str(Path(sys.argv[1]).resolve())], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    times = []
    try:
        for index in range(1000):
            start = time.perf_counter()
            request = {"protocol": 1, "id": index, "operation": "double", "inputs": {"value": index},
                       "context": {"run_id": "benchmark", "artifact_dir": directory, "user": None, "iteration": None}}
            process.stdin.write(json.dumps(request) + "\n")
            process.stdin.flush()
            result = json.loads(process.stdout.readline())
            assert result["id"] == index and result["result"]["value"] == index * 2
            times.append((time.perf_counter() - start) * 1000)
    finally:
        process.stdin.close()
        process.wait(timeout=5)
print(json.dumps({"calls":len(times),"p50_ms":statistics.median(times),"p95_ms":sorted(times)[949],"mean_ms":statistics.mean(times)}))
