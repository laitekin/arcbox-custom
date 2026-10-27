"""Build routing regression checks; no compiler or running app is required."""

import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest


DESKTOP = Path(__file__).resolve().parents[1]


class BuildModesTests(unittest.TestCase):
    def dry_run(self, target, *variables):
        env = {k: v for k, v in os.environ.items()
               if not k.startswith(("ARCBOX_", "MAKE"))}
        result = subprocess.run(
            ["make", "-n", "-C", str(DESKTOP), target,
             "ARCBOX_USE_LOCAL_RUNTIME=1", "SIGN_IDENTITY=-", *variables],
            env=env, text=True, capture_output=True, check=True,
        )
        return result.stdout

    def xcode_settings(self, target):
        lines = self.dry_run(target).splitlines()
        line = next(line for line in lines if "xcodebuild build " in line)
        return dict(token.split("=", 1) for token in shlex.split(line) if "=" in token)

    def test_fast_build_embeds_the_selected_profile(self):
        settings = self.xcode_settings("build-fast")
        self.assertEqual(settings["ARCBOX_HOST_CARGO_PROFILE"], "dev-fast")
        self.assertTrue(settings["ARCBOX_HOST_BIN_DIR"].endswith("/dev-fast"))
        self.assertEqual(settings["SKIP_RUST_BUILD"], "0")
        self.assertEqual(settings["ARCBOX_REUSE_RESOURCES"], "false")
        command = self.dry_run("build-runnable-rust", "ARCBOX_HOST_CARGO_PROFILE=dev-fast")
        self.assertIn("--profile dev-fast", command)
        self.assertIn("/dev-fast/$binary", command)

    def test_ui_reuses_payload_and_preserves_app_identity(self):
        ui = self.xcode_settings("build-ui")
        full = self.xcode_settings("build-runnable")
        self.assertEqual(ui["SKIP_RUST_BUILD"], "1")
        self.assertEqual(ui["ARCBOX_REUSE_RESOURCES"], "true")
        self.assertEqual(ui["ARCBOX_STAGE_RESOURCES"], "1")
        for key in ("ARCBOX_PRODUCT_BUNDLE_IDENTIFIER", "ARCBOX_PROFILE", "CODE_SIGN_IDENTITY"):
            self.assertEqual(ui[key], full[key])

    def test_original_build_still_uses_release(self):
        settings = self.xcode_settings("build-runnable")
        self.assertEqual(settings["ARCBOX_HOST_CARGO_PROFILE"], "release")
        self.assertTrue(settings["ARCBOX_HOST_BIN_DIR"].endswith("/release"))
        self.assertEqual(settings["SKIP_RUST_BUILD"], "0")
        self.assertIn("--profile release", self.dry_run("build-runnable-rust"))

    def test_conflicting_modes_fail_before_build_or_launch(self):
        result = subprocess.run(
            ["bash", str(DESKTOP / "script/build_and_run.sh"), "--fast", "--ui-only"],
            text=True, capture_output=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)

    def test_ui_without_a_previous_app_has_an_actionable_error(self):
        with tempfile.TemporaryDirectory() as directory:
            Path(directory, "arcbox.version").write_text("test")
            result = subprocess.run(
                ["make", "-C", directory, "-f", str(DESKTOP / "Makefile"),
                 "check-ui-prerequisites", "SIGN_IDENTITY=-"],
                text=True, capture_output=True,
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Run make run-fast", result.stderr)


if __name__ == "__main__":
    unittest.main()
