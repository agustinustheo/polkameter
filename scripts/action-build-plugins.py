#!/usr/bin/env python3
"""Build plugin packages and register their executable artifacts with the CLI."""
import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    unlocked = os.environ.get("PLUGIN_UNLOCKED", "false")
    if unlocked not in ("true", "false"):
        raise ValueError("plugin-unlocked must be true or false")
    if unlocked == "true":
        print("::warning::plugin-unlocked=true: plugin dependency resolution may update Cargo.lock")
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
        if unlocked != "true":
            args.append("--locked")
        if separator:
            if not package:
                raise ValueError("plugin-crates expects path or path:package")
            args.extend(["--package", package])
        try:
            result = subprocess.run(args, stdout=subprocess.PIPE, text=True, check=True)
        except subprocess.CalledProcessError as error:
            if unlocked != "true":
                raise ValueError(f"Locked plugin build failed for {entry}. Commit an up-to-date "
                                 "Cargo.lock for the package/workspace; for intentional dependency "
                                 "resolution, opt in with plugin-unlocked: true. See Cargo diagnostics above.") from error
            raise
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
