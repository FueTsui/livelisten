"""Assemble a local, unsigned Windows candidate from a fresh publish: the installer
(LiveListen-Setup-<ver>-x64.exe, built by the setup crate's `pack`), the portable ZIP,
provenance, release manifest and SHA256SUMS.txt.

Only explicit source/runtime inputs are read. User settings, credentials and .env
files are never inputs. Existing output folders are never overwritten or removed.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import struct
import subprocess
import sys
import tomllib
import urllib.request
import xml.etree.ElementTree as ET
import zipfile
from datetime import datetime, timezone

ROOT = Path(__file__).resolve().parents[1]
VERSION = "0.1.1"
LOCK = ROOT / "scripts/release-dependencies.json"
INSTALL_MANIFEST = ROOT / "setup/install-manifest.json"
VENDOR_FILES = ("__init__.py", "doubao.py", "protocol.py", "LICENSE", "UPSTREAM.md")
OMIT_SUFFIXES = {".pdb", ".lib", ".exp", ".ilk", ".iobj", ".ipdb"}
# Crates that publish without their license text: pinned copies under third_party/licenses
# (each folder's UPSTREAM.md names the source). A changed file stops the package.
APACHE = ("common/LICENSE-APACHE", "c16f8dcf1a368b83be78d826ea23de4079fe1b4469a0ab9ee20563f37ff3d44b")
SUPPLIED_LICENSES = {
    "dasp_sample-0.11.0": [("dasp_sample-0.11.0/LICENSE-MIT",
                            "b1d6df41ed3aa96806e74c729444d7c121d90e6660a6aed01d298e03fde475a0")],
    "clipboard-win-5.4.1": [("clipboard-win-5.4.1/LICENSE-BSL-1.0",
                             "c9bff75738922193e67fa726fa225535870d2aa1059f91452c411736284ad566")],
    "epaint_default_fonts-0.31.1": [APACHE] + [("epaint_default_fonts-0.31.1/" + name, sha) for name, sha in (
        ("OFL.txt", "6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2"),
        ("UFL.txt", "2f0015108d68627bd788d313f529c21ff4da2c2c42a5e1f3883acc83480f9002"),
        ("Hack-Regular.txt", "47c0cccbeec7e8614548cc485588b28149e7874188df5f41b36efebcee285c87"),
        ("emoji-icon-font-mit-license.txt", "b9d2c1d909aa149996fd4c91dcb92b2362a04431640c1d200959da94caf8cde1"))],
    **{name: [APACHE] for name in (
        "ecolor-0.31.1", "eframe-0.31.1", "egui-0.31.1", "egui-winit-0.31.1", "egui_glow-0.31.1",
        "emath-0.31.1", "epaint-0.31.1", "profiling-1.0.18", "gl_generator-0.14.0", "khronos_api-3.1.0")},
}
# WinUI ships its control strings in 93 language folders. The app's own UI is English and
# Chinese, and Windows falls back to English for every other language.
KEPT_LANGUAGES = ("en-", "zh-")


def omitted(relative: Path) -> bool:
    if relative.suffix.lower() in OMIT_SUFFIXES:
        return True
    return (relative.suffix.lower() == ".mui" and len(relative.parts) == 2
            and not relative.parts[0].lower().startswith(KEPT_LANGUAGES))


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def write_json(path: Path, value) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def safe_file(path: Path) -> None:
    for part in path.parts:
        name = part.lower()
        if name.startswith(".env") or name in {".git", ".venv", "credentials", "secrets", "sessions", "__pycache__"}:
            raise ValueError("Forbidden package input name")
    if path.suffix.lower() in {".pem", ".key", ".pfx", ".p12", ".wav", ".mp3", ".log"}:
        raise ValueError("Forbidden package input extension")
    if path.is_symlink():
        raise ValueError("Symlink package inputs are not accepted")


def new_output(path: Path) -> Path:
    path = path.resolve()
    if not path.is_relative_to(ROOT) or path == ROOT:
        raise ValueError("Output must be a new folder within the project")
    path.mkdir(parents=True, exist_ok=False)
    return path


def copy(source: Path, target: Path) -> None:
    safe_file(source)
    target.parent.mkdir(parents=True, exist_ok=True)
    if target.exists():
        raise ValueError(f"Duplicate package destination: {target.name}")
    shutil.copy2(source, target)


def fetch(item: dict) -> Path:
    cache = ROOT / "target/release-cache"
    cache.mkdir(parents=True, exist_ok=True)
    target = cache / item["file"]
    safe_file(target)
    if not target.exists():
        with urllib.request.urlopen(item["url"], timeout=60) as response, target.open("xb") as output:
            shutil.copyfileobj(response, output)
    if digest(target) != item["sha256"]:
        raise ValueError(f"Pinned dependency SHA256 mismatch: {target.name}; cache was not overwritten")
    return target


def extract(archive: Path, target: Path, include=lambda name: True) -> None:
    with zipfile.ZipFile(archive) as package:
        for entry in package.infolist():
            name = entry.filename.replace("\\", "/")
            relative = PurePosixPath(name)
            if relative.is_absolute() or ".." in relative.parts or ":" in name:
                raise ValueError("Unsafe archive member")
            if entry.is_dir() or not include(name):
                continue
            destination = target.joinpath(*relative.parts)
            safe_file(destination)
            destination.parent.mkdir(parents=True, exist_ok=True)
            with package.open(entry) as source, destination.open("xb") as output:
                shutil.copyfileobj(source, output)


def runtime(app: Path) -> dict:
    lock = json.loads(LOCK.read_text(encoding="utf-8-sig"))
    python = lock["python"]
    target = app / "runtime/python"
    extract(fetch(python), target)
    # Keep the embedded interpreter isolated from system/user site-packages and
    # PYTHONPATH. All entries are package-relative; there is no import-site line.
    (target / python["pathFile"]).write_text(
        python["stdlibZip"] + "\n.\nLib\\site-packages\n..\\..\\\n", encoding="ascii")
    wheel = lock["websocketClient"]
    def wheel_member(name: str) -> bool:
        return (name.startswith("websocket/") and "/tests/" not in name and name.endswith(".py")) or (
            ".dist-info/" in name and name.rsplit("/", 1)[-1] in {"LICENSE", "LICENSE.txt", "METADATA", "WHEEL"})
    extract(fetch(wheel), target / "Lib/site-packages", wheel_member)
    for name in VENDOR_FILES:
        copy(ROOT / "third_party/doubao_asr" / name, app / "doubao_asr" / name)
    for name in ("doubao_live.py", "official_live.py"):
        destination = app / "providers" / name
        if not destination.exists():
            copy(ROOT / "providers" / name, destination)
        elif digest(destination) != digest(ROOT / "providers" / name):
            raise ValueError("Published provider bridge does not match the source")
    probe = "import ssl, websocket, doubao_asr.doubao; import sys; assert sys.flags.isolated; print(sys.version.split()[0], websocket.__version__)"
    result = subprocess.run([str(target / "python.exe"), "-I", "-B", "-c", probe], cwd=app,
                            check=True, capture_output=True, text=True, timeout=20)
    if result.stdout.strip() != f"{python['version']} {wheel['version']}":
        raise ValueError("Bundled Python dependency version mismatch")
    return lock


def license_candidates(folder: Path, explicit: str | None = None):
    result = []
    if explicit:
        item = (folder / explicit).resolve()
        if not item.is_relative_to(folder.resolve()):
            raise ValueError("License file escapes its package")
        if item.is_file():
            result.append(item)
    for item in folder.iterdir():
        if item.is_file() and item.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE", "THIRD-PARTY", "THIRD_PARTY")):
            if item not in result:
                result.append(item)
    return result


def pe_imports(path: Path) -> list[str]:
    """Read native import names without loading or executing the binary."""
    data = path.read_bytes()
    pe = struct.unpack_from('<I', data, 0x3c)[0]
    if data[:2] != b'MZ' or data[pe:pe + 4] != b'PE\0\0':
        raise ValueError('Not a Windows PE image')
    sections, optional_size = struct.unpack_from('<H12xH', data, pe + 6)
    optional = pe + 24
    if struct.unpack_from('<H', data, optional)[0] != 0x20b:
        raise ValueError('Package core must be 64-bit PE32+')
    def offset(rva):
        for index in range(sections):
            section = optional + optional_size + index * 40
            virtual_size, virtual, raw_size, raw = struct.unpack_from('<IIII', data, section + 8)
            if virtual <= rva < virtual + max(virtual_size, raw_size):
                return raw + rva - virtual
        raise ValueError('PE address is outside its sections')
    def string(rva):
        start = offset(rva)
        return data[start:data.index(b'\0', start)].decode('ascii')
    result = []
    for directory_index, record_size, name_position in ((1, 20, 12), (13, 32, 4)):
        rva, size = struct.unpack_from('<II', data, optional + 112 + directory_index * 8)
        if not rva or not size:
            continue
        start = offset(rva)
        for position in range(start, start + size, record_size):
            record = data[position:position + record_size]
            if not any(record): break
            if directory_index == 13 and struct.unpack_from('<I', record)[0] != 1:
                raise ValueError('Unsupported delay import addressing')
            result.append(string(struct.unpack_from('<I', record, name_position)[0]))
    return sorted(set(result), key=str.lower)


def notices(app: Path, assets: Path, deps: Path) -> list[dict]:
    components = []
    metadata = json.loads(subprocess.run(["cargo", "metadata", "--locked", "--offline", "--format-version", "1",
        "--filter-platform", "x86_64-pc-windows-msvc"], cwd=ROOT, check=True, capture_output=True, text=True).stdout)
    selected = {node["id"] for node in metadata["resolve"]["nodes"]}
    for package in metadata["packages"]:
        if package["id"] not in selected or package["source"] is None:
            continue
        folder = Path(package["manifest_path"]).parent
        identifier = package["name"] + "-" + package["version"]
        files = license_candidates(folder, package.get("license_file"))
        if not files and identifier in SUPPLIED_LICENSES:
            for relative, expected in SUPPLIED_LICENSES[identifier]:
                supplied = ROOT / "third_party/licenses" / relative
                if digest(supplied) != expected:
                    raise ValueError("Pinned license mismatch: " + relative)
                files.append(supplied)
        if not files:
            raise ValueError('Resolved Rust component has no packaged license: ' + identifier)
        copied = []
        for item in files:
            destination = app / "licenses/rust" / identifier / item.name
            copy(item, destination)
            copied.append(destination.relative_to(app).as_posix())
        components.append({"ecosystem": "cargo", "name": package["name"], "version": package["version"],
            "license": package["license"], "licenseFiles": copied, "scope": "resolved Windows build graph"})
    nuget = json.loads(assets.read_text(encoding="utf-8-sig"))
    folders = [Path(p) for p in nuget["packageFolders"]]
    libraries = dict(nuget['libraries'])
    for identifier in json.loads(deps.read_text(encoding='utf-8-sig'))['libraries']:
        if identifier.startswith('runtimepack.'):
            identifier = identifier.removeprefix('runtimepack.')
            libraries.setdefault(identifier, {'type': 'package', 'path': identifier.lower()})
    for identifier, package in libraries.items():
        if package.get("type") != "package":
            continue
        folder = next((base / package["path"] for base in folders if (base / package["path"]).is_dir()), None)
        if folder is None:
            raise ValueError("Resolved NuGet package is absent")
        spec = next(folder.glob("*.nuspec"), None)
        expression = None
        explicit = None
        license_url = None
        if spec:
            for node in ET.parse(spec).getroot().iter():
                tag = node.tag.rsplit("}", 1)[-1]
                if tag == "license":
                    if node.attrib.get("type") == "file": explicit = node.text
                    else: expression = node.text
                elif tag == "licenseUrl": license_url = node.text
        copied = []
        files = license_candidates(folder, explicit)
        if not files and license_url == 'https://aka.ms/WinSDKLicenseURL':
            supplied = ROOT / 'third_party/licenses/windows-sdk/sdk_license.rtf'
            if digest(supplied) != 'dd07eb178e00c6bba4148457fc00ff77cd4887eb521d504186fe59c9ec8bbe62':
                raise ValueError('Pinned Windows SDK license mismatch')
            files = [supplied]
        for item in files:
            destination = app / "licenses/nuget" / identifier.replace("/", "-") / item.name
            copy(item, destination)
            copied.append(destination.relative_to(app).as_posix())
        name, version = identifier.rsplit("/", 1)
        if not copied:
            raise ValueError('Resolved NuGet component has no packaged license: ' + identifier)
        components.append({"ecosystem": "nuget", "name": name, "version": version, "license": expression,
            "licenseUrl": license_url, "licenseFiles": copied, "scope": "resolved build graph (includes build-only tooling)"})
    return components


def no_vc_runtime(path: Path) -> list[str]:
    imports = pe_imports(path)
    if any(name.lower().startswith(('vcruntime', 'msvcp', 'concrt')) for name in imports):
        raise ValueError(f'{path.name} requires external Visual C++ runtime')
    return imports


def installer(app: Path, output: Path, core: Path) -> Path:
    """Pack the app folder into the setup program; `pack` re-reads every file before it reports success."""
    product = json.loads(INSTALL_MANIFEST.read_text(encoding="utf-8"))
    target = output / f"{product['id']}-Setup-{VERSION}-x64.exe"
    result = subprocess.run([str(core / "pack.exe"), "--stub", str(core / "setup.exe"), "--source", str(app),
                             "--manifest", str(INSTALL_MANIFEST), "--version", VERSION, "--out", str(target)],
                            cwd=ROOT, capture_output=True, text=True, timeout=900)
    if result.returncode != 0:
        raise ValueError("Installer packing failed: " + result.stderr.strip())
    summary = json.loads(result.stdout)
    if not summary.get("verified") or summary.get("sha256") != digest(target) or summary.get("version") != VERSION:
        raise ValueError("Installer verification summary does not match the written file")
    return target


def bundle(args) -> Path:
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    if workspace["workspace"]["package"]["version"] != VERSION:
        raise ValueError("Cargo workspace version and package VERSION differ")
    output = new_output(args.output_dir)
    app = output / "LiveListen"
    if args.prepare_runtime:
        runtime(app)
        write_json(output / "runtime-check.json", {"passed": True, "networkAsrUsed": False})
        return output
    publish = args.publish_dir.resolve()
    required = ["LiveListen.exe", "coreclr.dll", "hostfxr.dll", "hostpolicy.dll",
                "System.Private.CoreLib.dll", "Microsoft.UI.Xaml.dll", "LiveListen.runtimeconfig.json"]
    if not all((publish / name).is_file() for name in required):
        raise ValueError("Publish directory is not a complete self-contained .NET/WinUI/Rust build")
    for source in sorted(publish.rglob("*")):
        if source.is_symlink():
            raise ValueError("Symlinks are not allowed in publish output")
        if source.is_file() and source.name != 'livelisten_native.dll' and not omitted(source.relative_to(publish)):
            copy(source, app / source.relative_to(publish))
    native = []
    for name in ('livelisten_native.dll', 'live-listen.exe'):
        source = args.core_dir / name
        imports = no_vc_runtime(source)
        copy(source, app / name)
        native.append({'name': name, 'sha256': digest(source), 'imports': imports})
    setup_stub = args.core_dir / 'setup.exe'
    native.append({'name': 'setup.exe (installer stub)', 'sha256': digest(setup_stub), 'imports': no_vc_runtime(setup_stub)})
    lock = runtime(app)
    copy(ROOT / "LICENSE", app / "LICENSE-LiveListen.txt")
    copy(ROOT / "docs/INSTALLATION.md", app / "README.md")
    for name in ("INSTALLATION.md", "USAGE.md", "THIRD_PARTY_NOTICES.md"):
        copy(ROOT / "docs" / name, app / "docs" / name)
    copy(LOCK, app / "dependencies.json")
    components = notices(app, args.assets, app / 'LiveListen.deps.json')
    components += [{"ecosystem": "embedded", "name": item["name"], "version": item["version"],
        "license": item["license"], "downloadSha256": item["sha256"], "source": item["metadataUrl"]}
        for item in (lock["python"], lock["websocketClient"])]
    components.append({"ecosystem": "vendored", "name": "doubao_asr realtime subset", "version": "2026-11-08",
        "license": "MIT", "copyright": "Copyright (c) 2026 kuxiaowo", "licenseFiles": ["doubao_asr/LICENSE"]})
    write_json(app / "dependency-inventory.json", {"schemaVersion": 1, "components": components})
    files = [{"path": p.relative_to(app).as_posix(), "size": p.stat().st_size, "sha256": digest(p)}
             for p in sorted(app.rglob("*")) if p.is_file()]
    write_json(app / "files.manifest.json", {"schemaVersion": 1, "files": files})
    archive = output / f"LiveListen-{VERSION}-portable-x64.zip"
    # Some deterministic NuGet artifacts carry 1970 timestamps; ZIP uses 1980 as
    # its lower limit. Clamp metadata only, preserving all original file bytes.
    with zipfile.ZipFile(archive, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=6, strict_timestamps=False) as package:
        for source in sorted(app.rglob("*")):
            if source.is_file(): package.write(source, "LiveListen/" + source.relative_to(app).as_posix())
    setup = installer(app, output, args.core_dir)
    revision = args.revision
    if not revision:
        probe = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True)
        revision = probe.stdout.strip() if probe.returncode == 0 else "uncommitted-working-tree"
    def tool_version(command):
        return subprocess.run(command, cwd=ROOT, check=True, capture_output=True, text=True).stdout.strip()
    write_json(output / "provenance.json", {"version": VERSION, "revision": revision, "status": "built",
        "builtAtUtc": datetime.now(timezone.utc).isoformat(), "tools": {"rustc": tool_version(["rustc", "--version"]),
        "dotnet": tool_version(["dotnet", "--version"]), "buildPython": sys.version.split()[0]},
        "rustLinkage": "-C target-feature=+crt-static", "nativeBinaries": native, "signed": False})
    write_json(output / "release-manifest.json", {"schemaVersion": 1, "version": VERSION, "status": "built",
        "platform": "win-x64", "supportedOs": "Windows 11 x64", "signing": "unsigned", "revision": revision,
        "files": [{"name": p.name, "size": p.stat().st_size, "sha256": digest(p)} for p in (setup, archive)]})
    checksum_targets = [setup, archive, output / "provenance.json", output / "release-manifest.json"]
    (output / "SHA256SUMS.txt").write_text("".join(digest(p) + "  " + p.name + "\n" for p in checksum_targets), encoding="ascii")
    with zipfile.ZipFile(archive) as package:
        if package.testzip() is not None: raise ValueError("Final ZIP CRC validation failed")
    return output


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--publish-dir", type=Path)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--revision", default="")
    parser.add_argument("--assets", type=Path, default=ROOT / "target/dotnet/obj/LiveListen/project.assets.json")
    parser.add_argument('--core-dir', type=Path)
    parser.add_argument("--prepare-runtime", action="store_true")
    args = parser.parse_args()
    if not args.prepare_runtime and args.publish_dir is None: parser.error("--publish-dir is required")
    if not args.prepare_runtime and args.core_dir is None: parser.error('--core-dir is required')
    output = bundle(args)
    print(json.dumps({"status": "built", "output": str(output), "signed": False}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
