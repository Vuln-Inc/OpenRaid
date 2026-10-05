"""Package verified TUI/desktop executables and optional SDK runtime companions."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile


def release_version(root, include_desktop=False):
    version = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    if include_desktop:
        versions = {
            "desktop/src-tauri/Cargo.toml": tomllib.loads(
                (root / "desktop/src-tauri/Cargo.toml").read_text(encoding="utf-8")
            )["package"]["version"],
            "desktop/src-tauri/tauri.conf.json": json.loads(
                (root / "desktop/src-tauri/tauri.conf.json").read_text(encoding="utf-8")
            )["version"],
            "desktop/package.json": json.loads(
                (root / "desktop/package.json").read_text(encoding="utf-8")
            )["version"],
        }
        for filename, desktop_version in versions.items():
            if desktop_version != version:
                raise SystemExit(f"Version mismatch in {filename}: {desktop_version}, expected {version}")
    return version


def verify_tag(root, tag):
    version = release_version(root, include_desktop=True)
    if tag != f"v{version}":
        raise SystemExit(f"Run this workflow from the matching v{version} tag, not {tag!r}")
    return version


def package_release(root, binary, platform, output, desktop_binary=None):
    version = release_version(root, include_desktop=desktop_binary is not None)
    binary = binary.resolve(strict=True)
    desktop_binary = desktop_binary.resolve(strict=True) if desktop_binary is not None else None
    if desktop_binary is not None:
        expected_name = "openraid-desktop.exe" if platform.startswith("windows-") else "openraid-desktop"
        if desktop_binary.name != expected_name:
            raise SystemExit(f"Expected desktop executable named {expected_name}, got {desktop_binary.name}")
    actual = subprocess.check_output([str(binary), "--version"], text=True).strip()
    if actual != f"openraid {version}":
        raise SystemExit(f"Executable version mismatch: {actual!r}, expected openraid {version}")
    output.mkdir(parents=True, exist_ok=True)
    name = f"openraid-v{version}-{platform}"
    with tempfile.TemporaryDirectory(prefix="openraid-package-") as temporary:
        package = Path(temporary) / name
        package.mkdir()
        shutil.copy2(binary, package / binary.name)
        if desktop_binary is not None:
            # Do not invoke the GUI binary: release runners have no desktop/display.
            shutil.copy2(desktop_binary, package / desktop_binary.name)
            licenses = package / "licenses"
            licenses.mkdir()
            shutil.copy2(root / "desktop/public/licenses/untitled-ui.txt", licenses / "untitled-ui.txt")
        for filename in ("README.md", "LICENSE"):
            shutil.copy2(root / filename, package / filename)
        for folder in ("assets", "docs"):
            shutil.copytree(root / folder, package / folder)
        scripts = package / "scripts"
        scripts.mkdir()
        for filename in ("sdk-bridge.mjs", "http-transport.mjs", "package.json", "package-lock.json"):
            shutil.copy2(root / "scripts" / filename, scripts / filename)
        desktop_instructions = (
            f"Launch {desktop_binary.name} for the desktop interface (no CLI arguments).\n"
            "Desktop uses the same workspace configuration and saved sessions as the terminal.\n"
            "Windows requires WebView2; Linux requires WebKitGTK 4.1. See docs/DESKTOP.md.\n"
            if desktop_binary is not None else ""
        )
        (package / "START-HERE.txt").write_text(
            f"openraid {version} by vuln.industries\n\n"
            "Run the included openraid terminal executable with setup --workspace PATH --agents 4.\n"
            + desktop_instructions
            + "Read README.md for exact commands and case-sensitive identifiers.\n"
            "Native model transports need no Node.js installation.\n"
            "Specialized providers: install Node.js 22.12+, then run npm ci --prefix scripts.\n"
            "These scripts are runtime companions; development tests/audits need the source checkout.\n",
            encoding="utf-8",
        )
        if platform.startswith("windows-"):
            archive = output / f"{name}.zip"
            with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as target:
                for path in sorted(package.rglob("*")):
                    if path.is_file():
                        target.write(path, path.relative_to(temporary))
        else:
            archive = output / f"{name}.tar.gz"
            executable_members = {f"{name}/{binary.name}"}
            if desktop_binary is not None:
                executable_members.add(f"{name}/{desktop_binary.name}")

            def executable_permissions(member):
                if member.name in executable_members:
                    member.mode = 0o755
                return member

            with tarfile.open(archive, "w:gz") as target:
                target.add(package, arcname=name, filter=executable_permissions)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (output / f"{archive.name}.sha256").write_text(
        f"{digest}  {archive.name}\n", encoding="ascii"
    )
    print(f"Packaged {actual}{' + desktop' if desktop_binary is not None else ''}: {archive}")
    print(f"SHA-256: {digest}")
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--desktop-binary", type=Path)
    parser.add_argument("--verify-tag", help="Check matching TUI/desktop versions and the selected release tag only")
    parser.add_argument("--platform", choices=[
        "linux-x86_64", "windows-x86_64", "darwin-x86_64", "darwin-arm64"
    ])
    parser.add_argument("--output", type=Path, default=Path("dist"))
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    if args.verify_tag is not None:
        print(f"Verified release versions: {verify_tag(root, args.verify_tag)}")
        return
    if args.binary is None or args.platform is None:
        parser.error("--binary and --platform are required when packaging")
    package_release(root, args.binary, args.platform, args.output, args.desktop_binary)


if __name__ == "__main__":
    main()
