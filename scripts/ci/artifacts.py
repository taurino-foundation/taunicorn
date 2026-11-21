"""Seal or verify the complete, flat contents of a release artifact."""

import argparse
import hashlib
import re
from pathlib import Path


MANIFEST = "SHA256SUMS"


def inventory(root: Path) -> str:
    lines = []
    for path in sorted(root.iterdir()):
        if path.name == MANIFEST:
            continue
        if (
            path.is_symlink()
            or not path.is_file()
            or not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9._+-]*", path.name)
        ):
            raise ValueError("Unexpected entry in release artifact")
        digest = hashlib.sha256()
        with path.open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
        lines.append(f"{digest.hexdigest()}  {path.name}\n")
    if not lines:
        raise ValueError("Empty release artifact")
    return "".join(lines)


def seal(root: Path) -> None:
    listing = inventory(root)
    with (root / MANIFEST).open("x", encoding="ascii", newline="\n") as stream:
        stream.write(listing)


def verify(root: Path) -> None:
    manifest = root / MANIFEST
    if manifest.is_symlink() or not manifest.is_file():
        raise ValueError("Missing or unsafe release manifest")
    if inventory(root).encode("ascii") != manifest.read_bytes():
        raise ValueError("Release inventory or checksum mismatch")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("seal", "verify"))
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    (seal if args.command == "seal" else verify)(args.directory)
