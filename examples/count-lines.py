#!/usr/bin/env python3
"""Count the lines of a stress test, the rule the examples' table uses.

usage: count-lines.py <dir>                 every file in <dir>, file by file
       count-lines.py --check <README.md>   recount every linked row of the table; exit 1 on a mismatch

Counts non-blank, non-comment lines of code and config. Skips node_modules, .papi, generated
descriptors, lockfiles, package.json, markdown, data and output files, and plain-papi/ when it
counts a calls-mode test.
"""
import os, re, statistics, sys

CODE = {".ts", ".mts", ".js", ".mjs", ".cjs", ".py", ".sh", ".toml", ".yaml", ".yml", ".xml", ".rs"}
SKIPD = {"node_modules", ".papi", "descriptors", "out", "results", "logs", "evidence", ".git", "target"}


def lines(path):
    n, block = 0, False
    for line in open(path, errors="ignore"):
        s = line.strip()
        if block:
            block = "*/" not in s
            continue
        if not s or s.startswith(("//", "#")):
            continue
        if s.startswith("/*"):
            block = "*/" not in s
            continue
        n += 1
    return n


def count(top, skip=()):
    per = []
    for root, dirs, files in os.walk(top):
        dirs[:] = [d for d in dirs if d not in SKIPD and d not in skip]
        for f in files:
            ext = os.path.splitext(f)[1]
            config = ext == ".json" and "config" in f and f not in ("package.json", "package-lock.json")
            if ext in CODE or config:
                p = os.path.join(root, f)
                per.append((lines(p), os.path.relpath(p, top)))
    return per


def check(readme):
    here, bad, table = os.path.dirname(os.path.abspath(readme)), 0, []
    text = open(readme).read()
    for row in text.splitlines():
        m = re.match(r"\| \[[^]]+\]\((?:[^)]*/)?([a-z-]+)/?\) \|[^|]*\| (\d+) \| \d+ \| (\d+) \|", row)
        if not m:
            continue
        name, plain, calls = m.group(1), int(m.group(2)), int(m.group(3))
        table.append((plain, calls))
        got_plain = sum(n for n, _ in count(os.path.join(here, name, "plain-papi")))
        got_calls = sum(n for n, _ in count(os.path.join(here, name), skip={"plain-papi"}))
        ok = (got_plain, got_calls) == (plain, calls)
        bad += not ok
        print(f"{'ok ' if ok else 'BAD'} {name}: plain-papi {got_plain} (table {plain}), calls mode {got_calls} (table {calls})")
    # The chart's two bar series and the Median row must agree with the rows above.
    bars = [[int(n) for n in b.split(",")] for b in re.findall(r"^\s*bar \[([\d, ]+)\]", text, re.M)]
    want = [[p for p, _ in table], [c for _, c in table]]
    ok = bars == want
    bad += not ok
    print(f"{'ok ' if ok else 'BAD'} chart: {bars} (table {want})")
    med = re.search(r"^\| \*\*Median\*\* \| \| \*\*(\d+)\*\* \| \*\*\d+\*\* \| \*\*(\d+)\*\* \|", text, re.M)
    got = [int(statistics.median(col)) for col in want]
    ok = bool(med) and [int(med.group(1)), int(med.group(2))] == got
    bad += not ok
    print(f"{'ok ' if ok else 'BAD'} median: {med.groups() if med else None} (computed {got})")
    return 1 if bad else 0


if __name__ == "__main__":
    if sys.argv[1:2] == ["--check"]:
        sys.exit(check(sys.argv[2]))
    per = count(sys.argv[1])
    for n, p in sorted(per, reverse=True):
        print(f"{n:6d}  {p}")
    print(f"TOTAL {sum(n for n, _ in per)}")
