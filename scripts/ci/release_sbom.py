"""Write a detached CycloneDX 1.5 inventory for one already-built release archive."""

import argparse
import hashlib
import io
import json
import re
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile
from email.parser import BytesParser
from pathlib import Path
from urllib.request import urlopen


PYTHON = Path("crates/taunicorn-python/Cargo.toml")
JS = Path("crates/taunicorn-javascript/Cargo.toml")
NATIVE = re.compile(r"\.(?:node|pyd|dll|dylib|so(?:\.[\d.]+)?|a|lib)$", re.I)


def component(ecosystem, name, version, license):
    if not name or not version:
        raise ValueError("Missing component identity")
    c: dict = dict(
        type="library",
        name=name,
        version=version,
        purl=f"pkg:{ecosystem}/{name}@{version}",
    )
    # Preserve verbatim manifest declarations, including non-SPDX npm spellings.
    if license:
        c["licenses"] = [{"license": {"name": license}}]
    else:
        c["description"] = "License not declared in the inspected metadata."
    return c


def cargo(manifest, target):
    package = tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]["name"]
    lines = subprocess.check_output(
        [
            "cargo",
            "tree",
            "--color",
            "never",
            "--locked",
            "--manifest-path",
            manifest,
            "-p",
            package,
            "--target",
            target,
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}|{l}",
        ],
        encoding="utf-8",
    )
    crates = {}
    for line in lines.splitlines():
        identity, license = line.removesuffix(" (*)").split("|")
        name, version, *_ = identity.split()
        crates[name, version.removeprefix("v")] = license
    return crates


def verify_embedded(wheel, binding, crates):
    [path] = [
        n
        for n in wheel.namelist()
        if re.fullmatch(r"[^/]+\.dist-info/sboms/[^/]+\.cyclonedx\.json", n)
    ]
    bom = json.loads(wheel.read(path))
    root = bom["metadata"]["component"]
    if (
        bom["bomFormat"] != "CycloneDX"
        or root["name"] != binding
        or (binding, root["version"]) not in crates
    ):
        raise ValueError("Wrong embedded wheel SBOM root")
    entries = {(c["name"], c["version"]): c for c in [root, *bom["components"]]}
    for key, license in crates.items():
        if key not in entries:
            raise ValueError(f"Missing embedded wheel SBOM component: {key}")
        declared = {
            item.get("expression")
            or item.get("license", {}).get("id")
            or item.get("license", {}).get("name")
            for item in entries[key].get("licenses", [])
        }
        if license and license.replace("/", " OR ") not in declared:
            raise ValueError(
                f"Embedded wheel SBOM license mismatch: {key}: "
                f"Cargo declares {license!r}, wheel declares {sorted(map(repr, declared))}"
            )


def python_runtime():
    bom = json.loads(
        subprocess.check_output(
            [
                "uv",
                "export",
                "--locked",
                "--no-dev",
                "--no-emit-project",
                "--format",
                "cyclonedx1.5",
                "--quiet",
            ]
        )
    )
    locked = {
        (p["name"], p.get("version")): p
        for p in tomllib.loads(Path("uv.lock").read_text(encoding="utf-8"))["package"]
    }
    for entry in bom["components"]:
        name, version = entry["name"], entry["version"]
        package = locked[name, version]
        wheel = package["wheels"][0]
        with urlopen(wheel["url"], timeout=60) as response:
            data = response.read()
        if "sha256:" + hashlib.sha256(data).hexdigest() != wheel["hash"]:
            raise ValueError(f"Locked wheel checksum mismatch: {name}")
        with zipfile.ZipFile(io.BytesIO(data)) as z:
            [path] = [n for n in z.namelist() if n.endswith(".dist-info/METADATA")]
            meta = BytesParser().parsebytes(z.read(path))
        if (meta["Name"].lower(), meta["Version"]) != (name, version):
            raise ValueError(f"Wrong locked wheel metadata: {name}")
        c = component(
            "pypi", name, version, meta["License-Expression"] or meta["License"]
        )
        c["description"] = "External runtime resolution (not bundled): " + " or ".join(
            package.get("resolution-markers", ["all"])
        )
        yield c


def locked_ids(path):
    return {
        tuple(p.get(k) for k in ("name", "version", "source", "checksum"))
        for p in tomllib.loads(path.read_text(encoding="utf-8"))["package"]
    }


def inventory(archive, target):
    crates, components = {}, []
    if archive.suffix == ".whl":
        if not target:
            raise ValueError("Wheel requires its build target")
        with zipfile.ZipFile(archive) as z:
            [path] = [n for n in z.namelist() if n.endswith(".dist-info/METADATA")]
            meta = BytesParser().parsebytes(z.read(path))
            subject = component(
                "pypi",
                meta["Name"],
                meta["Version"],
                meta["License-Expression"] or meta["License"],
            )
            crates = cargo(PYTHON, target)
            verify_embedded(z, "taunicorn-python", crates)
            names = z.namelist()
            if not any(
                n.startswith("taunicorn/_taunicorn.") and NATIVE.search(n)
                for n in names
            ):
                raise ValueError("Missing native wheel extension")
        components.extend(python_runtime())
    else:
        with tarfile.open(archive, "r:gz") as tar:
            names = tar.getnames()
            if archive.suffix == ".tgz":
                packed = tar.extractfile("package/package.json")
                if packed is None:
                    raise ValueError("Missing packed manifest")
                meta = json.load(packed)
                subject = component(
                    "npm", meta["name"], meta["version"], meta["license"]
                )
                if (
                    meta.get("dependencies")
                    or meta.get("peerDependencies")
                    or meta.get("bundledDependencies")
                ):
                    raise ValueError(
                        "Packed npm runtime dependencies require inventory support"
                    )
                if target:
                    if (
                        not meta["main"].endswith(".node")
                        or "package/" + meta["main"] not in names
                    ):
                        raise ValueError("Missing packed native addon")
                    crates = cargo(JS, target)
                else:
                    if any(NATIVE.search(n) for n in names):
                        raise ValueError("Native npm archive requires its build target")
                    for name, version in meta["optionalDependencies"].items():
                        c = component("npm", name, version, None)
                        c.update(
                            scope="optional",
                            description="Separately delivered; license in platform SBOM.",
                        )
                        components.append(c)
            elif archive.suffix == ".crate" or archive.name.endswith(".tar.gz"):
                with tempfile.TemporaryDirectory() as tmp:
                    if not all(m.isfile() or m.isdir() for m in tar):
                        raise ValueError("Unexpected source archive member type")
                    tar.extractall(tmp, filter="data")
                    [root] = Path(tmp).iterdir()
                    manifest = root / "Cargo.toml"
                    if archive.suffix == ".crate":
                        pkg = tomllib.loads(manifest.read_text(encoding="utf-8"))[
                            "package"
                        ]
                        subject = component(
                            "cargo", pkg["name"], pkg["version"], pkg["license"]
                        )
                    else:
                        meta = BytesParser().parsebytes(
                            (root / "PKG-INFO").read_bytes()
                        )
                        subject = component(
                            "pypi",
                            meta["Name"],
                            meta["Version"],
                            meta["License-Expression"] or meta["License"],
                        )
                        manifest = root / PYTHON
                        # Maturin drops unrelated workspace members but ships the workspace lock.
                        lock = root / "Cargo.lock"
                        original = locked_ids(lock)
                        subprocess.run(
                            ["cargo", "tree", "--manifest-path", manifest],
                            stdout=subprocess.DEVNULL,
                            check=True,
                        )
                        if not locked_ids(lock) <= original:
                            raise ValueError(
                                "Pruning sdist workspace changed a locked package"
                            )
                        components.extend(python_runtime())
                    crates = cargo(manifest, "all")
            else:
                raise ValueError(f"Unsupported archive: {archive}")
    for (name, version), license in sorted(crates.items()):
        c = component("cargo", name, version, license)
        if c["purl"] != subject["purl"]:
            components.append(c)
    for name in names:
        if NATIVE.search(name):
            components.append(
                dict(
                    type="file",
                    name=name,
                    description="Native archive member; linkage not inferred.",
                )
            )
    return subject, components


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--target")
    args = parser.parse_args()
    archive = args.archive
    subject, components = inventory(archive, args.target)
    with archive.open("rb") as stream:
        subject["hashes"] = [
            {
                "alg": "SHA-256",
                "content": hashlib.file_digest(stream, "sha256").hexdigest(),
            }
        ]
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    props = {
        "archive": archive.name,
        "commit": commit,
        "cargo-target": args.target or ("none" if archive.suffix == ".tgz" else "all"),
        "coverage": "Resolved normal/build crates, not proof of linkage. No OS/toolchain, JS dev tools or opaque vendored/static libraries inferred.",
    }
    bom = dict(
        bomFormat="CycloneDX",
        specVersion="1.5",
        version=1,
        metadata=dict(
            component=subject,
            properties=[dict(name="taunicorn:" + k, value=v) for k, v in props.items()],
        ),
        components=components,
    )
    args.output.mkdir(parents=True, exist_ok=True)
    with (args.output / (archive.name + ".cdx.json")).open(
        "x", encoding="utf-8"
    ) as stream:
        json.dump(bom, stream, indent=2, ensure_ascii=False)
        stream.write("\n")


if __name__ == "__main__":
    main()
