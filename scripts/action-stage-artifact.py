#!/usr/bin/env python3
"""Stage the composite action's run artifact: an allowlist of PolkaMeter reports by default.

The action runs this after `polkameter run` has exited. Plugin processes that
outlived it may still write to the run directory, so staging takes a copy, not
links: every directory and file is opened through file descriptors that refuse
symbolic links, and no file contributes more bytes than it had when selected.
"""
import errno
import fnmatch
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import sys
from urllib.parse import urlsplit

# Files PolkaMeter itself writes, redacted or free of credential values. Kept even
# when they exceed the size limit: they are small and explain the run.
REPORTS = (
    "execution.json", "summary.md", "summary.json", "junit.xml", "plan.json",
    "resolved-plan.json", "resolved-targets.json", "calibration.json", "plugin-checks.json",
    "plots/*.svg",
)
# Larger PolkaMeter records and plugin stderr, in the order they are kept when
# the size limit cannot hold them all.
RECORDS = (
    "events.jsonl", "plugins/*/stderr.log", "steps.jsonl", "lost.jsonl", "samples.jtl",
    "blocks.jsonl", "load.jsonl", "node.jsonl", "chain.jsonl", "transactions.jsonl", "scrapes.jsonl",
)
# PolkaMeter redacts these only when the run finishes. A run killed before then
# (SIGKILL, out of memory, a panic) leaves them as the plugins wrote them.
REDACTED_AT_FINISH = ("plugins/*/stderr.log", "summary.md", "summary.json", "plugin-checks.json")
FINAL_STATES = {"completed", "completed_with_failures", "stopped", "failed"}
OMITTED = "polkameter-artifact-omitted.txt"
DIRECTORY = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | os.O_NOFOLLOW
# O_NONBLOCK: a file swapped for a FIFO after selection must not block the open.
FILE = os.O_RDONLY | os.O_NOFOLLOW | getattr(os, "O_NONBLOCK", 0)


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


def walk(directory_fd, prefix=""):
    """(relative path, size) of each regular file; links and special files are skipped."""
    found = []
    for name in sorted(os.listdir(directory_fd)):
        try:
            info = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
        except FileNotFoundError:
            continue
        if stat.S_ISDIR(info.st_mode):
            try:
                child = os.open(name, DIRECTORY, dir_fd=directory_fd)
            except OSError:
                continue
            try:
                found += walk(child, f"{prefix}{name}/")
            finally:
                os.close(child)
        elif stat.S_ISREG(info.st_mode):
            found.append((prefix + name, info.st_size))
    return found


def open_file(root_fd, relative):
    """Opens root/relative for reading without following a link at any component."""
    *parents, name = relative.split("/")
    current = os.dup(root_fd)
    try:
        for parent in parents:
            child = os.open(parent, DIRECTORY, dir_fd=current)
            os.close(current)
            current = child
        descriptor = os.open(name, FILE, dir_fd=current)
    finally:
        os.close(current)
    if not stat.S_ISREG(os.fstat(descriptor).st_mode):
        os.close(descriptor)
        raise OSError(errno.EINVAL, "not a regular file", relative)
    return descriptor


def read_text(root_fd, relative, limit=1024 * 1024):
    try:
        descriptor = open_file(root_fd, relative)
    except OSError:
        return None
    with os.fdopen(descriptor, "rb") as file:
        return file.read(limit).decode("utf-8", "replace")


def last_event(root_fd):
    """The last event in events.jsonl, read from its end."""
    try:
        descriptor = open_file(root_fd, "events.jsonl")
    except OSError:
        return None
    with os.fdopen(descriptor, "rb") as file:
        size = os.fstat(file.fileno()).st_size
        file.seek(max(0, size - 1024 * 1024))
        lines = [line for line in file.read().splitlines() if line.strip()]
    try:
        return json.loads(lines[-1]) if lines else None
    except ValueError:
        return None


def finished(root_fd):
    """True when PolkaMeter wrote its outcome: execution.json and the final event."""
    try:
        execution = json.loads(read_text(root_fd, "execution.json") or "")
        event = last_event(root_fd) or {}
    except ValueError:
        return False
    return (isinstance(execution, dict) and execution.get("state") in FINAL_STATES
            and event.get("event") == "completed")


def rank(relative, patterns):
    for index, pattern in enumerate(patterns):
        if fnmatch.fnmatchcase(relative, pattern) and relative.count("/") == pattern.count("/"):
            return index
    return None


def select(files, raw, limit, complete):
    """Returns (kept, omitted, excluded, withheld): kept as (path, size) pairs."""
    size = dict(files)
    names = [name for name, _ in files]
    withheld = [] if complete else [f for f in names if rank(f, REDACTED_AT_FINISH) is not None]
    names = [f for f in names if f not in withheld]
    reports = [f for f in names if rank(f, REPORTS) is not None]
    records = sorted((f for f in names if rank(f, RECORDS) is not None), key=lambda f: (rank(f, RECORDS), f))
    others = [f for f in names if f not in reports and f not in records]
    total = sum(size[f] for f in reports)
    kept, omitted = [(f, size[f]) for f in reports], []
    for relative in records + (others if raw else []):
        if limit and total + size[relative] > limit:
            omitted.append(relative)
            continue
        kept.append((relative, size[relative]))
        total += size[relative]
    return kept, omitted, ([] if raw else others), withheld


def copy(root_fd, relative, budget, destination):
    """Copies at most budget bytes of root/relative; a vanished or swapped file is skipped."""
    try:
        source = open_file(root_fd, relative)
    except OSError:
        return False
    target = destination / relative
    target.parent.mkdir(parents=True, exist_ok=True)
    with os.fdopen(source, "rb") as src, open(target, "xb") as out:
        remaining = budget
        while remaining:
            chunk = src.read(min(remaining, 1024 * 1024))
            if not chunk:
                break
            out.write(chunk)
            remaining -= len(chunk)
    return True


def stage(run_dir, destination, raw, limit):
    root_fd = os.open(run_dir, DIRECTORY)
    try:
        complete = finished(root_fd)
        kept, omitted, excluded, withheld = select(walk(root_fd), raw, limit, complete)
        destination.mkdir(parents=True)
        staged = [relative for relative, size in kept if copy(root_fd, relative, size, destination)]
    finally:
        os.close(root_fd)
    return staged, omitted, excluded, withheld


def main():
    limit = check_inputs()
    if sys.argv[1:] == ["--check"]:
        return
    if sys.argv[1:2] == ["--finished"]:
        root_fd = os.open(sys.argv[2], DIRECTORY)
        try:
            sys.exit(0 if finished(root_fd) else 1)
        finally:
            os.close(root_fd)
    run_dir = os.environ["RUN_DIR"]
    raw = boolean("RAW_PLUGIN_FILES", "upload-raw-plugin-files", "false")
    name = os.environ.get("ARTIFACT_NAME", "").strip() or default_name(run_dir)
    digest = hashlib.sha256(name.encode()).hexdigest()[:16]
    destination = Path(os.environ["RUNNER_TEMP"]) / f"polkameter-artifact-{digest}"
    if destination.exists():
        shutil.rmtree(destination)
    _, omitted, excluded, withheld = stage(run_dir, destination, raw, limit)
    if omitted:
        (destination / OMITTED).write_text(
            f"Omitted to stay within artifact-size-limit-mb ({limit // (1024 * 1024)} MiB):\n"
            + "".join(f"{f}\n" for f in omitted))
        print(f"::warning::The run artifact omits {len(omitted)} file(s) over artifact-size-limit-mb; "
              f"see {OMITTED} in the artifact. They remain in {run_dir} on the runner.")
    if withheld:
        print(f"::notice::The run did not finish normally, so PolkaMeter did not redact plugin logs and "
              f"summaries. The run artifact leaves out {len(withheld)} file(s): {', '.join(withheld)}. "
              f"They remain in {run_dir} on the runner.")
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
