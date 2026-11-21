"""Promote an already sealed native library. Only `publish` writes to crates.io."""

import hashlib
import io
import json
import os
import re
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
from http.client import HTTPSConnection
from pathlib import Path
from urllib.request import Request, urlopen

NAME = "taunicorn"
INDEX = "https://index.crates.io/ta/un/taunicorn"
MANIFEST = Path("crates/taunicorn/Cargo.toml")
AGENT = "taunicorn-release (github.com/taurino-foundation/taunicorn)"


def stable(version):
    # shortcut: stable releases only; implement full SemVer before publishing prereleases.
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)(?:\.(?:0|[1-9][0-9]*)){2}", version):
        raise ValueError(f"expected stable major.minor.patch, got {version!r}")
    return tuple(map(int, version.split(".")))


def previous(rows, version):
    older = [r for r in rows if not r["yanked"] and stable(r["vers"]) < stable(version)]
    if not older:
        raise ValueError("no earlier non-yanked artifact for rollback exercise")
    return max(older, key=lambda r: stable(r["vers"]))


def require_new(rows, version):
    candidate = stable(version)
    if not rows or any(stable(r["vers"]) >= candidate for r in rows):
        raise ValueError(
            f"{NAME} {version} is not a new version above the registry history"
        )


def fetch(url):
    with urlopen(Request(url, headers={"User-Agent": AGENT}), timeout=30) as r:
        return r.read()


def history():
    rows = [json.loads(line) for line in fetch(INDEX).splitlines()]
    for r in rows:
        if (
            r["name"] != NAME
            or type(r["yanked"]) is not bool
            or not re.fullmatch("[a-f0-9]{64}", r["cksum"])
        ):
            raise ValueError("invalid crates.io index entry")
    return rows


def frame(meta, crate):
    return struct.pack("<I", len(meta)) + meta + struct.pack("<I", len(crate)) + crate


def unpack_capture(body):
    if len(body) < 8:
        raise ValueError("truncated upload")
    end = 4 + int.from_bytes(body[:4], "little")
    if (
        end + 4 >= len(body)
        or int.from_bytes(body[end : end + 4], "little") != len(body) - end - 4
    ):
        raise ValueError("invalid Cargo upload lengths")
    meta = json.loads(body[4:end])
    for dep in meta["deps"]:
        # In Cargo's capture request, null means *capture registry*, not crates.io.
        if dep["registry"] not in (
            "https://github.com/rust-lang/crates.io-index",
            "sparse+https://index.crates.io/",
        ):
            raise ValueError(f"non-crates.io dependency: {dep['name']}")
        dep["registry"] = None
    return meta, body[end + 4 :]


def artifact(folder):
    metadata = (folder / "metadata.json").read_bytes()
    meta = json.loads(metadata)
    pkg = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))["package"]
    v = meta["vers"]
    stable(v)
    if (pkg["name"], pkg["version"], meta["name"]) != (
        NAME,
        v,
        NAME,
    ) or "crates-io" not in pkg.get("publish", ["crates-io"]):
        raise ValueError("manifest does not authorize this package/version")
    crate = (folder / f"{NAME}-{v}.crate").read_bytes()
    with tarfile.open(fileobj=io.BytesIO(crate), mode="r:gz") as tar:
        member = tar.getmember(f"{NAME}-{v}/Cargo.toml")
        stream = tar.extractfile(member) if member.isfile() else None
        if stream is None:
            raise ValueError("missing packaged manifest")
        manifest = tomllib.load(stream)["package"]
    if (manifest["name"], manifest["version"]) != (NAME, v):
        raise ValueError("archive identity differs from upload metadata")
    return v, metadata, crate


def smoke(root, v, crate):
    package = f"{NAME}-{v}"
    with tarfile.open(fileobj=io.BytesIO(crate), mode="r:gz") as tar:
        for member in tar:
            if (
                not member.name.startswith(package + "/")
                or not member.isfile()
                or "\\" in member.name
                or ".." in member.name.split("/")
            ):
                raise ValueError(f"unexpected archive member {member.name!r}")
        tar.extractall(root, filter="data")
    # Same consumer, changed dependency only; never use the checked-out library.
    manifest = root / "Cargo.toml"
    manifest.write_text(
        f'[package]\nname="consumer"\nversion="0.0.0"\nedition="2024"\n'
        f'[dependencies]\ntaunicorn={{ path="{package}", version="={v}" }}\n',
        encoding="utf-8",
    )
    subprocess.run(["cargo", "run", "--manifest-path", str(manifest)], check=True)
    print(
        f"consumer OK: {package} sha256={hashlib.sha256(crate).hexdigest()}", flush=True
    )


def stage(folder):
    v, _, crate = artifact(folder)
    old = previous(history(), v)
    pv = old["vers"]
    prior = fetch(f"https://static.crates.io/crates/{NAME}/{NAME}-{pv}.crate")
    if hashlib.sha256(prior).hexdigest() != old["cksum"]:
        raise ValueError("rollback archive differs from registry checksum")
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / "src").mkdir()
        (root / "src/main.rs").write_text(
            'fn main() { assert_eq!(taunicorn::Endpoint::new("smoke").unwrap().as_str(), "smoke"); }'
        )
        smoke(root, v, crate)
        smoke(root, pv, prior)


def put(connection, body, token):
    connection.request(
        "PUT",
        "/api/v1/crates/new",
        body,
        headers={
            "Authorization": token,
            "Content-Type": "application/octet-stream",
            "Accept": "application/json",
            "User-Agent": AGENT,
        },
    )
    r = connection.getresponse()
    raw = r.read()
    if not 200 <= r.status < 300:
        raise ValueError(f"registry rejected upload: HTTP {r.status}: {raw[:1000]!r}")
    reply = json.loads(raw)
    if not isinstance(reply, dict) or "errors" in reply or reply.get("ok") is False:
        raise ValueError(f"registry rejected upload: {reply!r}")
    if reply.get("warnings"):
        print("registry warnings:", reply["warnings"])


def preflight(folder):
    v, metadata, crate = artifact(folder)
    if v != os.environ.get("RELEASE_VERSION"):
        raise ValueError("dispatch version must exactly match the sealed artifact")
    require_new(history(), v)
    return v, metadata, crate


def publish(folder):
    v, metadata, crate = preflight(folder)
    # No redirects or retries: a lost response may mean the write already succeeded.
    con = HTTPSConnection("crates.io", timeout=60)
    try:
        put(con, frame(metadata, crate), os.environ["CARGO_REGISTRY_TOKEN"])
    finally:
        con.close()
    digest = hashlib.sha256(crate).hexdigest()
    print(f"accepted {NAME}-{v}; checking registry checksum", flush=True)
    for _ in range(12):
        for row in history():
            if row["vers"] == v:
                if row["cksum"] != digest or row["yanked"]:
                    raise ValueError(
                        "published checksum or yank status differs from staged artifact"
                    )
                print(f"confirmed {NAME}-{v} sha256={digest}")
                return
        time.sleep(5)
    raise ValueError("accepted but not indexed yet; inspect crates.io before retrying")


if __name__ == "__main__":
    command, folder = sys.argv[1:]
    {"stage": stage, "preflight": preflight, "publish": publish}[command](Path(folder))
