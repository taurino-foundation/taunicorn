"""Exercise the downloaded distribution, never the checkout or an index candidate."""

import os
import subprocess
import sys
import tempfile
from pathlib import Path


TESTS = Path(__file__).resolve().parents[2] / "python" / "tests"
CHECK = """\
import sys, taunicorn
from importlib.metadata import version
assert version('taunicorn') == taunicorn.__version__ == sys.argv[1]
"""


def main(directory: Path, version: str) -> None:
    (dist,) = [p for p in directory.resolve().iterdir() if p.name != "SHA256SUMS"]
    sdist = dist.name == f"taunicorn-{version}.tar.gz"
    if not sdist and not (
        dist.name.startswith(f"taunicorn-{version}-cp39-abi3-")
        and dist.suffix == ".whl"
    ):
        raise ValueError(f"Unexpected release file: {dist.name}")
    requirement = f"taunicorn[test] @ {dist.as_uri()}"
    with tempfile.TemporaryDirectory() as tmp:
        for py in ["3.9"] if sdist else ["3.9", "3.10", "3.11", "3.12", "3.13", "3.14"]:
            print(f"Testing {dist.name} on Python {py}", flush=True)
            subprocess.run(
                [
                    "uv",
                    "run",
                    "--isolated",
                    "--no-project",
                    "--refresh-package",
                    "taunicorn",
                    "--python",
                    py,
                    "--with",
                    requirement,
                    "python",
                    "-I",
                    "-c",
                    CHECK + "\nimport pytest; sys.exit(pytest.main(sys.argv[2:]))",
                    version,
                    "-q",
                    "--import-mode=importlib",
                    str(TESTS),
                ],
                cwd=tmp,
                check=True,
            )
        if not sdist and os.environ.get("PY_ROLLBACK_DRILL") == "1":
            env = Path(tmp) / "rollback"
            python = env / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
            subprocess.run(["uv", "venv", "--python", "3.12", env], check=True)
            for package, expected in [
                (str(dist), version),
                ("taunicorn==0.1.9", "0.1.9"),
            ]:
                subprocess.run(
                    [
                        "uv",
                        "pip",
                        "install",
                        "--python",
                        python,
                        "--only-binary",
                        ":all:",
                        package,
                    ],
                    check=True,
                )
                subprocess.run(
                    [python, "-I", "-c", CHECK, expected], cwd=tmp, check=True
                )
            print(f"Rollback {version} -> 0.1.9 verified", flush=True)


if __name__ == "__main__":
    main(Path(sys.argv[1]), sys.argv[2])
