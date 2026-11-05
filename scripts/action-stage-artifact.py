#!/usr/bin/env python3
"""Stage the composite action's run artifact: an allowlist of PolkaMeter reports by default."""
import fnmatch
import hashlib
import os
from pathlib import Path
import re
import shutil
import sys
from urllib.parse import urlsplit

# Files PolkaMeter itself writes, redacted or free of credential values. Kept even
# when they exceed the size limit: they are small and explain the run.
REPORTS = (
    "execution.json", "summary.md", "summary.json", "junit.xml", "plan.json",
    "resolved-plan.json", "resolved-targets.json", "calibration.json", "plugin-checks.json",
    "plots/*.svg",
)
# Larger PolkaMeter records and redacted plugin stderr, in the order they are kept
# when the size limit cannot hold them all.
RECORDS = (
    "events.jsonl", "plugins/*/stderr.log", "steps.jsonl", "lost.jsonl", "samples.jtl",
    "blocks.jsonl", "load.jsonl", "node.jsonl", "chain.jsonl", "transactions.jsonl", "scrapes.jsonl",
)
OMITTED = "polkameter-artifact-omitted.txt"


def boolean(name, label, default):
    value = os.environ.get(name, "") or default
    if value not in ("true", "false"):
        raise ValueError(f"{label} must be true or false")
    return value == "true"


def check_inputs():
    boolean("UPLOAD", "upload", "true")
    boolean("RAW_PLUGIN_FILES", "upload-raw-plugin-files", "false")
    limit = os.environ.get("SIZE_LIMIT_MB", "") or "500"
    if not re.fullmatch(r"[0-9]+", limit):
        raise ValueError("artifact-size-limit-mb must be a whole number of MiB (0 for no limit)")
    retention = os.environ.get("RETENTION_DAYS", "")
    if retention and not (re.fullmatch(r"[0-9]+", retention) and 1 <= int(retention) <= 90):
        raise ValueError("artifact-retention-days must be empty or 1 to 90")
    return int(limit) * 1024 * 1024


def upload_supported(server_url):
    # actions/upload-artifact v4 and newer run only on github.com and GHE.com.
    host = (urlsplit(server_url).hostname or "").lower()
    return host == "github.com" or host.endswith(".ghe.com")


def default_name(run_dir):
    job = re.sub(r"[^A-Za-z0-9_.-]", "-", os.environ.get("GITHUB_JOB", "job"))
    attempt = os.environ.get("GITHUB_RUN_ATTEMPT", "1")
    # GITHUB_ACTION differs for each use in a job and RUNNER_NAME for each matrix
    # job on hosted runners; the run directory name differs for every run.
    identity = "\n".join([job, attempt, os.environ.get("GITHUB_ACTION", ""),
                          os.environ.get("RUNNER_NAME", ""), Path(run_dir).name])
    return f"polkameter-run-{job}-{attempt}-{hashlib.sha256(identity.encode()).hexdigest()[:10]}"


def regular_files(root):
    """Regular files under root, relative and sorted; symbolic links are never followed."""
    found = []
    for directory, subdirectories, files in os.walk(root):
        subdirectories[:] = sorted(d for d in subdirectories if not os.path.islink(os.path.join(directory, d)))
        for name in files:
            path = Path(directory, name)
            if path.is_file() and not path.is_symlink():
                found.append(path.relative_to(root).as_posix())
    return sorted(found)


def rank(relative, patterns):
    for index, pattern in enumerate(patterns):
        if fnmatch.fnmatchcase(relative, pattern) and relative.count("/") == pattern.count("/"):
            return index
    return None


def select(run_dir, raw, limit):
    """Returns (kept, omitted) relative paths."""
    files = regular_files(run_dir)
    reports = [f for f in files if rank(f, REPORTS) is not None]
    records = sorted((f for f in files if rank(f, RECORDS) is not None), key=lambda f: (rank(f, RECORDS), f))
    others = [f for f in files if f not in reports and f not in records]
    candidates = records + (others if raw else [])
    size = {f: (Path(run_dir) / f).stat().st_size for f in files}
    total = sum(size[f] for f in reports)
    kept, omitted = list(reports), []
    for relative in candidates:
        if limit and total + size[relative] > limit:
            omitted.append(relative)
            continue
        kept.append(relative)
        total += size[relative]
    return kept, omitted, ([] if raw else others)


def stage(run_dir, destination, kept):
    if destination.exists():
        shutil.rmtree(destination)
    for relative in kept:
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        try:
            os.link(Path(run_dir) / relative, target)
        except OSError:
            shutil.copy2(Path(run_dir) / relative, target)


def main():
    limit = check_inputs()
    if sys.argv[1:] == ["--check"]:
        return
    run_dir = os.environ["RUN_DIR"]
    raw = boolean("RAW_PLUGIN_FILES", "upload-raw-plugin-files", "false")
    name = os.environ.get("ARTIFACT_NAME", "").strip() or default_name(run_dir)
    kept, omitted, excluded = select(run_dir, raw, limit)
    digest = hashlib.sha256(name.encode()).hexdigest()[:16]
    destination = Path(os.environ["RUNNER_TEMP"]) / f"polkameter-artifact-{digest}"
    stage(run_dir, destination, kept)
    if omitted:
        (destination / OMITTED).write_text(
            f"Omitted to stay within artifact-size-limit-mb ({limit // (1024 * 1024)} MiB):\n"
            + "".join(f"{f}\n" for f in omitted))
        print(f"::warning::The run artifact omits {len(omitted)} file(s) over artifact-size-limit-mb; "
              f"see {OMITTED} in the artifact. They remain in {run_dir} on the runner.")
    if excluded:
        print(f"::notice::The run artifact excludes {len(excluded)} file(s) that PolkaMeter did not write "
              "(plugin-written files are not redacted). Set upload-raw-plugin-files: \"true\" to include them.")
    supported = upload_supported(os.environ.get("GITHUB_SERVER_URL", "https://github.com"))
    if not supported:
        print("::notice::Skipping the run artifact upload: actions/upload-artifact v4 and newer are not "
              "supported on GitHub Enterprise Server. Upload the artifact-path output with an uploader "
              "your server supports, such as actions/upload-artifact@v3.")
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"artifact-name={name}\nartifact-path={destination}\n"
                     f"upload={str(supported).lower()}\n")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError) as error:
        print(f"::error::{error}", file=sys.stderr)
        sys.exit(1)
