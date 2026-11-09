#!/usr/bin/env python3
"""Package the public CLI asset contract, with a checksum alongside each archive.

An archive holds the CLI, LICENSE and README.md, plus any bundled plugin executables, such as
polkameter-substrate-plugin, at its root."""

import argparse
import hashlib
from pathlib import Path
import tarfile
import tempfile
import shutil


def package(version, target, binary, output, plugins=()):
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"polkameter-{version}-{target}.tar.gz"
    with tempfile.TemporaryDirectory() as temporary:
        staging = Path(temporary)
        shutil.copy2(binary, staging / "polkameter")
        (staging / "polkameter").chmod(0o755)
        for plugin in plugins:
            plugin = Path(plugin)
            if plugin.name in ("polkameter", "LICENSE", "README.md"):
                raise ValueError(f"plugin {plugin} would replace {plugin.name}")
            shutil.copy2(plugin, staging / plugin.name)
            (staging / plugin.name).chmod(0o755)
        for document in ("LICENSE", "README.md"):
            shutil.copy2(document, staging / document)
        with tarfile.open(archive, "w:gz") as bundle:
            for entry in sorted(staging.iterdir()):
                bundle.add(entry, arcname=entry.name)
    checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(archive.name + ".sha256").write_text(f"{checksum}  {archive.name}\n")
    return archive


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version")
    parser.add_argument("target")
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, default=Path("release-assets"))
    parser.add_argument("--plugin", type=Path, action="append", default=[],
                        help="a plugin executable to bundle; repeat for more")
    args = parser.parse_args()
    print(package(args.version, args.target, args.binary, args.output, args.plugin))
