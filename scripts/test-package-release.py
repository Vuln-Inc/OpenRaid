"""Standalone release asset contracts; never opens a desktop window."""
import importlib.util
from contextlib import redirect_stdout
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("package_release", Path(__file__).with_name("package-release.py"))
assert spec is not None and spec.loader is not None
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

    def test_release_contains_only_executables_and_combined_license(self):
        tui = self.binary("openraid.exe", b"terminal executable")
        desktop = self.binary("openraid-desktop.exe", b"desktop executable")
        with redirect_stdout(io.StringIO()), patch.object(release.subprocess, "check_output", return_value="openraid 1.2.3\n") as execute:
            output = release.package_release(self.root, tui, "windows-x86_64", self.root / "dist", desktop)
        execute.assert_called_once_with([str(tui.resolve()), "--version"], text=True)
        contents = {path.name: path.read_bytes() for path in output.iterdir()}
        self.assertEqual(set(contents), {"openraid.exe", "openraid-desktop.exe", "LICENSE"})
        self.assertEqual(contents[tui.name], b"terminal executable")
        self.assertEqual(contents[desktop.name], b"desktop executable")
        self.assertIn(b"Project license", contents["LICENSE"])
        self.assertIn(b"Untitled UI license", contents["LICENSE"])

    def test_terminal_only_packaging_remains_supported(self):
        tui = self.binary("openraid.exe", b"terminal executable")
        with redirect_stdout(io.StringIO()), patch.object(release.subprocess, "check_output", return_value="openraid 1.2.3"):
            output = release.package_release(self.root, tui, "windows-x86_64", self.root / "dist")
        self.assertEqual({path.name for path in output.iterdir()}, {"openraid.exe", "LICENSE"})
        self.assertEqual((output / "LICENSE").read_text(encoding="utf-8"), "Project license")

    def test_non_windows_assets_are_rejected(self):
        for platform in ["linux-x86_64", "darwin-x86_64", "darwin-arm64"]:
            with self.subTest(platform=platform), self.assertRaisesRegex(SystemExit, "Windows .exe"):
                release.package_release(self.root, self.root / "missing", platform, self.root / "dist")
        self.assertFalse((self.root / "dist").exists())

    def test_old_archives_or_directories_in_output_are_rejected(self):
        tui = self.binary("openraid.exe", b"terminal executable")
        output = self.root / "dist"
        output.mkdir()
        (output / "old.zip").write_bytes(b"old archive")
        with patch.object(release.subprocess, "check_output", return_value="openraid 1.2.3"):
            with self.assertRaisesRegex(SystemExit, "unexpected entries"):
                release.package_release(self.root, tui, "windows-x86_64", output)
        self.assertEqual({path.name for path in output.iterdir()}, {"old.zip"})
        (output / "old.zip").unlink()
        (output / "LICENSE").mkdir()
        with patch.object(release.subprocess, "check_output", return_value="openraid 1.2.3"):
            with self.assertRaisesRegex(SystemExit, "unexpected entries"):
                release.package_release(self.root, tui, "windows-x86_64", output)

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
        tui = self.binary("openraid.exe", b"terminal executable")
        with self.assertRaises(FileNotFoundError):
            release.package_release(self.root, tui, "windows-x86_64", self.root / "dist", self.root / "missing")
        self.assertFalse((self.root / "dist").exists())

    def test_wrong_terminal_binary_version_rejects_packaging(self):
        tui = self.binary("openraid.exe", b"terminal executable")
        with patch.object(release.subprocess, "check_output", return_value="openraid 9.9.9"):
            with self.assertRaisesRegex(SystemExit, "Executable version mismatch"):
                release.package_release(self.root, tui, "windows-x86_64", self.root / "dist")
        self.assertFalse((self.root / "dist").exists())

    def test_desktop_binary_cannot_shadow_the_terminal_executable(self):
        tui = self.binary("openraid.exe", b"terminal executable")
        with self.assertRaisesRegex(SystemExit, "Expected desktop executable"):
            release.package_release(self.root, tui, "windows-x86_64", self.root / "dist", tui)
        self.assertFalse((self.root / "dist").exists())


if __name__ == "__main__":
    unittest.main()
