#!/usr/bin/env python3
"""Build plugin packages and register their executable artifacts with the CLI."""
import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    for entry in os.environ.get("PLUGIN_CRATES", "").splitlines():
        entry = entry.strip()
        if not entry:
            continue
        path, separator, package = entry.partition(":")
        manifest = Path(path)
        if manifest.is_dir():
            manifest /= "Cargo.toml"
        args = ["cargo", f"+{os.environ['TOOLCHAIN']}", "build", "--release",
                "--manifest-path", str(manifest), "--message-format=json-render-diagnostics"]
        if separator:
            if not package:
                raise ValueError("plugin-crates expects path or path:package")
            args.extend(["--package", package])
        result = subprocess.run(args, stdout=subprocess.PIPE, text=True, check=True)
        binaries = set()
        for line in result.stdout.splitlines():
            artifact = json.loads(line)
            if (artifact.get("reason") == "compiler-artifact"
                    and "bin" in artifact["target"]["kind"] and artifact.get("executable")):
                binaries.add(artifact["executable"])
        if not binaries:
            raise ValueError(f"No plugin executables were built for {entry}")
        for binary in sorted(binaries):
            subprocess.run(["polkameter", "plugin", "install", binary], check=True)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"::error::{error}", file=sys.stderr)
        sys.exit(1)
