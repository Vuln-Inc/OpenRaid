"""Release archive contracts; uses fake binaries and never opens a desktop window."""
import hashlib
import importlib.util
from contextlib import redirect_stdout
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile


spec = importlib.util.spec_from_file_location("package_release", Path(__file__).with_name("package-release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleasePackagingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="openraid-release-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        files = {
            "Cargo.toml": '[package]\nname = "openraid"\nversion = "1.2.3"\n',
            "desktop/src-tauri/Cargo.toml": '[package]\nversion = "1.2.3"\n',
            "desktop/src-tauri/tauri.conf.json": '{"version":"1.2.3"}',
            "desktop/package.json": '{"version":"1.2.3"}',
            "desktop/public/licenses/untitled-ui.txt": "Untitled UI license",
            "README.md": "README",
            "LICENSE": "Project license",
            "assets/icon.svg": "<svg />",
            "docs/DESKTOP.md": "Desktop instructions",
            "scripts/sdk-bridge.mjs": "// SDK bridge",
            "scripts/http-transport.mjs": "// Transport",
            "scripts/package.json": "{}",
            "scripts/package-lock.json": "{}",
        }
        for name, content in files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")

    def binary(self, name, content):
        path = self.root / "binaries" / name
        path.parent.mkdir(exist_ok=True)
        path.write_bytes(content)
        path.chmod(0o755)
        return path

    def test_all_platform_archives_include_both_executables_and_companions(self):
        for platform in ["linux-x86_64", "windows-x86_64", "darwin-x86_64", "darwin-arm64"]:
            with self.subTest(platform=platform):
                suffix = ".exe" if platform.startswith("windows-") else ""
                tui = self.binary(f"openraid{suffix}", b"terminal executable")
                desktop = self.binary(f"openraid-desktop{suffix}", b"desktop executable")
                with redirect_stdout(io.StringIO()), patch.object(release.subprocess, "check_output", return_value="openraid 1.2.3\n") as execute:
                    archive = release.package_release(self.root, tui, platform, self.root / "dist", desktop)
                # Only the terminal's --version is executed; GUI binaries are copied.
                execute.assert_called_once_with([str(tui.resolve()), "--version"], text=True)
                prefix = f"openraid-v1.2.3-{platform}/"
                if suffix:
                    with zipfile.ZipFile(archive) as packaged:
                        contents = {name.removeprefix(prefix): packaged.read(name) for name in packaged.namelist()}
                else:
                    with tarfile.open(archive) as packaged:
                        contents = {member.name.removeprefix(prefix): packaged.extractfile(member).read()
                                    for member in packaged.getmembers() if member.isfile()}
                        for executable in [tui.name, desktop.name]:
                            self.assertTrue(packaged.getmember(prefix + executable).mode & 0o111)
                self.assertEqual(contents[tui.name], b"terminal executable")
                self.assertEqual(contents[desktop.name], b"desktop executable")
                for path in ["scripts/sdk-bridge.mjs", "scripts/http-transport.mjs", "docs/DESKTOP.md", "LICENSE", "licenses/untitled-ui.txt"]:
                    self.assertIn(path, contents)
                self.assertIn(desktop.name, contents["START-HERE.txt"].decode())
                expected = f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n"
                self.assertEqual(Path(str(archive) + ".sha256").read_text(encoding="ascii"), expected)

    def test_terminal_only_packaging_remains_supported(self):
        tui = self.binary("openraid", b"terminal executable")
        with redirect_stdout(io.StringIO()), patch.object(release.subprocess, "check_output", return_value="openraid 1.2.3"):
            archive = release.package_release(self.root, tui, "linux-x86_64", self.root / "dist")
        with tarfile.open(archive) as packaged:
            self.assertFalse(any("openraid-desktop" in member.name for member in packaged.getmembers()))

    def test_mismatched_desktop_versions_reject_the_release(self):
        for name in ["desktop/src-tauri/Cargo.toml", "desktop/src-tauri/tauri.conf.json", "desktop/package.json"]:
            with self.subTest(file=name):
                path = self.root / name
                original = path.read_text(encoding="utf-8")
                path.write_text(original.replace("1.2.3", "9.9.9"), encoding="utf-8")
                with self.assertRaisesRegex(SystemExit, "Version mismatch"):
                    release.verify_tag(self.root, "v1.2.3")
                path.write_text(original, encoding="utf-8")

    def test_tag_must_match_all_release_versions(self):
        self.assertEqual(release.verify_tag(self.root, "v1.2.3"), "1.2.3")
        with self.assertRaises(SystemExit):
            release.verify_tag(self.root, "main")

    def test_missing_desktop_executable_does_not_produce_partial_artifacts(self):
        tui = self.binary("openraid", b"terminal executable")
        with self.assertRaises(FileNotFoundError):
            release.package_release(self.root, tui, "linux-x86_64", self.root / "dist", self.root / "missing")
        self.assertFalse((self.root / "dist").exists())

    def test_wrong_terminal_binary_version_rejects_packaging(self):
        tui = self.binary("openraid", b"terminal executable")
        with patch.object(release.subprocess, "check_output", return_value="openraid 9.9.9"):
            with self.assertRaisesRegex(SystemExit, "Executable version mismatch"):
                release.package_release(self.root, tui, "linux-x86_64", self.root / "dist")
        self.assertFalse((self.root / "dist").exists())

    def test_desktop_binary_cannot_shadow_the_terminal_executable(self):
        tui = self.binary("openraid", b"terminal executable")
        with self.assertRaisesRegex(SystemExit, "Expected desktop executable"):
            release.package_release(self.root, tui, "linux-x86_64", self.root / "dist", tui)
        self.assertFalse((self.root / "dist").exists())


if __name__ == "__main__":
    unittest.main()
