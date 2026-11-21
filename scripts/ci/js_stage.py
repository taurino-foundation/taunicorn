"""Install only the two verified local TGZs, outside the checkout, with an empty npm cache."""

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from artifacts import verify


dist, abi, *python = sys.argv[1:]
tarballs = []
for name in ("root", abi):
    directory = Path(dist).resolve() / f"js-{name}"
    verify(directory)
    [tgz] = directory.glob("*.tgz")
    tarballs.append(str(tgz))
with tempfile.TemporaryDirectory() as tmp:
    work = Path(tmp)
    (work / "package.json").write_text('{"private":true}')
    shutil.copy(Path(__file__).with_name("js_smoke.mjs"), work)
    subprocess.run(
        [
            shutil.which("npm") or "npm",
            "install",
            "--offline",
            "--cache",
            str(work / "cache"),
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            *tarballs,
        ],
        cwd=work,
        check=True,
    )
    env = {**os.environ, "NAPI_RS_ENFORCE_VERSION_CHECK": "1"}
    for key in ("NODE_PATH", "NAPI_RS_NATIVE_LIBRARY_PATH"):
        env.pop(key, None)
    subprocess.run(
        ["node", "js_smoke.mjs", abi], cwd=work, env=env, timeout=45, check=True
    )
    if python:
        interop = Path(__file__).resolve().with_name("interop.py")
        subprocess.run(
            [*python, "-I", str(interop)], cwd=work, env=env, timeout=45, check=True
        )
        print("Installed Python wheel / Node TGZ IPC passed")
