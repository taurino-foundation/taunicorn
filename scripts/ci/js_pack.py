"""Pack one release TGZ from an allowlisted scratch tree, never the checkout."""

import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from artifacts import seal


def pack(kind, build, output):
    repo = Path.cwd()
    output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        shutil.copy("package.json", root)
        subprocess.run(
            [
                "node",
                ".yarn/releases/yarn-4.18.1.cjs",
                "napi",
                "create-npm-dirs",
                "--cwd",
                str(root),
            ],
            check=True,
        )
        platform_dirs = sorted((root / "npm").iterdir())
        if kind == "root":
            package = json.loads((root / "package.json").read_text(encoding="utf-8"))
            deps = {}
            for directory in platform_dirs:
                meta = json.loads(
                    (directory / "package.json").read_text(encoding="utf-8")
                )
                deps[meta["name"]] = meta["version"]
            assert len(deps) == len(package["napi"]["targets"]) == 4
            package["optionalDependencies"] = deps
            for key in (
                "scripts",
                "devDependencies",
                "ava",
                "lint-staged",
                "packageManager",
            ):
                package.pop(key, None)
            entry = Path("javascript/taunicorn")
            (root / entry).mkdir(parents=True)
            for name in ("index.cjs", "index.js", "index.d.ts"):
                # The single native build generates CJS and declarations; keep the project's ESM loader.
                shutil.copy(
                    (repo / entry if name == "index.js" else build) / name, root / entry
                )
            shutil.copy("README.md", root)
            source = root
            expected = {
                f"{entry.as_posix()}/{name}"
                for name in ("index.cjs", "index.js", "index.d.ts")
            }
        else:
            source = next(
                directory for directory in platform_dirs if directory.name == kind
            )
            package = json.loads((source / "package.json").read_text(encoding="utf-8"))
            native = package["main"]
            shutil.copy(build / native, source)
            expected = {native}
        for license_file in repo.glob("LICENSE*"):
            shutil.copy(license_file, source)
            expected.add(license_file.name)
        manifest = source / "package.json"
        package["files"] = sorted(expected)
        manifest.write_text(json.dumps(package, indent=2) + "\n", encoding="utf-8")
        result = subprocess.check_output(
            [
                shutil.which("npm") or "npm",
                "pack",
                "--ignore-scripts",
                "--json",
                "--pack-destination",
                str(output),
            ],
            cwd=source,
        )
        [packed] = json.loads(result)
        assert {f["path"] for f in packed["files"]} == expected | {
            "package.json",
            "README.md",
        }, packed
        print(packed["filename"], packed["integrity"])
    seal(output)


if __name__ == "__main__":
    pack(sys.argv[1], Path(sys.argv[2]).resolve(), Path(sys.argv[3]).resolve())
