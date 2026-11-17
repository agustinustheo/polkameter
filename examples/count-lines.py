#!/usr/bin/env python3
"""Fixed line-count rule (00-protocol.md). usage: count-lines.py <dir>
Counts non-blank, non-comment lines of files the subject wrote. Excludes node_modules, .papi, generated
descriptors, lockfiles, package.json, *.md, data/output files (.jsonl .log .csv .json except *config*.json), and
anything under dirs named out/ results/ logs/ evidence/."""
import os, re, sys
CODE = {".ts", ".mts", ".js", ".mjs", ".cjs", ".py", ".sh", ".toml", ".yaml", ".yml", ".xml", ".rs"}
SKIPD = {"node_modules", ".papi", "descriptors", "out", "results", "logs", "evidence", ".git", "target"}
tot = {}; per = []
for root, ds, fs in os.walk(sys.argv[1]):
    ds[:] = [d for d in ds if d not in SKIPD]
    for f in fs:
        ext = os.path.splitext(f)[1]
        ok = ext in CODE or (ext == ".json" and "config" in f and f not in ("package.json", "package-lock.json"))
        if not ok: continue
        p = os.path.join(root, f); n = 0; blk = False
        for line in open(p, errors="ignore"):
            s = line.strip()
            if blk:
                if "*/" in s: blk = False
                continue
            if not s or s.startswith(("//", "#")): continue
            if s.startswith("/*"):
                if "*/" not in s: blk = True
                continue
            n += 1
        per.append((n, os.path.relpath(p, sys.argv[1]))); kind = "config" if ext in {".toml", ".yaml", ".yml", ".json", ".xml"} else "code"
        tot[kind] = tot.get(kind, 0) + n
for n, p in sorted(per, reverse=True): print(f"{n:6d}  {p}")
print(f"TOTAL code={tot.get('code',0)} config={tot.get('config',0)} all={sum(tot.values())}")
