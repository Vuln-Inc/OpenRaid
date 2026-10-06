"""Stage verified Windows executables and LICENSE as standalone release assets."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tomllib


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
    if platform != "windows-x86_64":
        raise SystemExit("Published release assets must be Windows .exe files and LICENSE")
    version = release_version(root, include_desktop=desktop_binary is not None)
    binary = binary.resolve(strict=True)
    desktop_binary = desktop_binary.resolve(strict=True) if desktop_binary is not None else None
    if binary.name != "openraid.exe":
        raise SystemExit(f"Expected terminal executable named openraid.exe, got {binary.name}")
    if desktop_binary is not None:
        expected_name = "openraid-desktop.exe"
        if desktop_binary.name != expected_name:
            raise SystemExit(f"Expected desktop executable named {expected_name}, got {desktop_binary.name}")
    actual = subprocess.check_output([str(binary), "--version"], text=True).strip()
    if actual != f"openraid {version}":
        raise SystemExit(f"Executable version mismatch: {actual!r}, expected openraid {version}")
    license_text = (root / "LICENSE").read_text(encoding="utf-8")
    expected = {binary.name, "LICENSE"}
    if desktop_binary is not None:
        expected.add(desktop_binary.name)
        # The frontend embeds these icons; preserve their notice in the one
        # permitted license asset instead of shipping an extra notice file.
        icon_license = (root / "desktop/public/licenses/untitled-ui.txt").read_text(encoding="utf-8")
        license_text = license_text.rstrip() + "\n\n---\n\nUntitled UI icons\n\n" + icon_license
    if output.exists():
        unexpected = sorted(path.name for path in output.iterdir() if path.name not in expected or not path.is_file())
        if unexpected:
            raise SystemExit(f"Release output contains unexpected entries: {', '.join(unexpected)}")
    output.mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, output / binary.name)
    if desktop_binary is not None:
        # Do not invoke the GUI binary: release runners have no desktop/display.
        shutil.copy2(desktop_binary, output / desktop_binary.name)
    (output / "LICENSE").write_text(license_text, encoding="utf-8")
    print(f"Staged {actual}{' + desktop' if desktop_binary is not None else ''}: {output}")
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--desktop-binary", type=Path)
    parser.add_argument("--verify-tag", help="Check matching TUI/desktop versions and the selected release tag only")
    parser.add_argument("--platform", choices=["windows-x86_64"])
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
