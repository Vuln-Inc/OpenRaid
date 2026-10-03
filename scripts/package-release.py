"""Package a verified native executable and its optional SDK runtime companions."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import zipfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--platform", required=True, choices=[
        "linux-x86_64", "windows-x86_64", "darwin-x86_64", "darwin-arm64"
    ])
    parser.add_argument("--output", type=Path, default=Path("dist"))
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=root, text=True,
    ))
    version = next(item["version"] for item in metadata["packages"] if item["name"] == "openraid")
    binary = args.binary.resolve(strict=True)
    actual = subprocess.check_output([str(binary), "--version"], text=True).strip()
    if actual != f"openraid {version}":
        raise SystemExit(f"Executable version mismatch: {actual!r}, expected openraid {version}")
    args.output.mkdir(parents=True, exist_ok=True)
    name = f"openraid-v{version}-{args.platform}"
    with tempfile.TemporaryDirectory(prefix="openraid-package-") as temporary:
        package = Path(temporary) / name
        package.mkdir()
        shutil.copy2(binary, package / binary.name)
        for filename in ("README.md", "LICENSE"):
            shutil.copy2(root / filename, package / filename)
        for folder in ("assets", "docs"):
            shutil.copytree(root / folder, package / folder)
        scripts = package / "scripts"
        scripts.mkdir()
        for filename in ("sdk-bridge.mjs", "http-transport.mjs", "package.json", "package-lock.json"):
            shutil.copy2(root / "scripts" / filename, scripts / filename)
        (package / "START-HERE.txt").write_text(
            f"openraid {version} by vuln.industries\n\n"
            "Run the included executable with setup --workspace PATH --agents 4.\n"
            "Read README.md for exact commands and case-sensitive identifiers.\n"
            "Native model transports need no Node.js installation.\n"
            "Specialized providers: install Node.js 22.12+, then run npm ci --prefix scripts.\n"
            "These scripts are runtime companions; development tests/audits need the source checkout.\n",
            encoding="utf-8",
        )
        if args.platform.startswith("windows-"):
            archive = args.output / f"{name}.zip"
            with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as target:
                for path in sorted(package.rglob("*")):
                    if path.is_file():
                        target.write(path, path.relative_to(temporary))
        else:
            archive = args.output / f"{name}.tar.gz"
            with tarfile.open(archive, "w:gz") as target:
                target.add(package, arcname=name)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (args.output / f"{archive.name}.sha256").write_text(
        f"{digest}  {archive.name}\n", encoding="ascii"
    )
    print(f"Packaged {actual}: {archive}")
    print(f"SHA-256: {digest}")


if __name__ == "__main__":
    main()
