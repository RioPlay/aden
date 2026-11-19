# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import cargo_budget as guard


class CargoBudgetTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.target = self.root / "target"
        self.cargo_home = self.root / ".cargo"
        self.target.mkdir()
        self.cargo_home.mkdir()

    def tearDown(self):
        self.temp.cleanup()

    def identity(self, command, env=None):
        return guard.build_identity(self.root, command, env or {})

    def test_identity_allows_same_build_with_test_filter_and_rust_source_change(self):
        (self.root / "Cargo.toml").write_text("[workspace]\nmembers = []\n", encoding="utf-8")
        (self.root / "Cargo.lock").write_text("version = 4\n", encoding="utf-8")
        source = self.root / "src.rs"
        source.write_text("fn main() {}\n", encoding="utf-8")
        first = self.identity(["cargo", "test", "-p", "aden-cli", "tests::one"])
        guard.check_build_identity(self.target, first)
        source.write_text("fn main() { println!(\"changed\"); }\n", encoding="utf-8")
        second = self.identity(["cargo", "test", "-p", "aden-cli", "tests::two"])
        guard.check_build_identity(self.target, second)

    def test_identity_rejects_package_workspace_toolchain_and_corrupt_marker(self):
        base = self.identity(["cargo", "check", "-p", "aden-cli"])
        guard.check_build_identity(self.target, base)
        for changed in (
            self.identity(["cargo", "check", "--workspace"]),
            self.identity(["cargo", "+nightly", "check", "-p", "aden-cli"]),
            self.identity(["cargo", "check", "-p", "aden-parse"]),
        ):
            with self.subTest(changed=changed), self.assertRaisesRegex(ValueError, "identity differs"):
                guard.check_build_identity(self.target, changed)
        (self.target / guard.IDENTITY_FILE).write_text("{broken", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "corrupt"):
            guard.check_build_identity(self.target, base)

    def test_identity_normalizes_release_and_short_feature_aliases(self):
        short = self.identity(["cargo", "build", "-r", "-F", "serde,derive"])
        long = self.identity(["cargo", "build", "--release", "--features", "derive,serde"])
        self.assertEqual(short, long)

    def test_identity_separates_profile_and_target_environment_changes(self):
        base = self.identity(["cargo", "build", "-r"], {"CARGO_PROFILE_RELEASE_OPT_LEVEL": "2"})
        guard.check_build_identity(self.target, base)
        for changed in (
            self.identity(["cargo", "build"], {"CARGO_PROFILE_RELEASE_OPT_LEVEL": "2"}),
            self.identity(["cargo", "build", "-r"], {"CARGO_PROFILE_RELEASE_OPT_LEVEL": "3"}),
            self.identity(["cargo", "build", "-r"], {"CARGO_BUILD_TARGET": "aarch64-unknown-linux-gnu"}),
        ):
            with self.subTest(changed=changed), self.assertRaisesRegex(ValueError, "identity differs"):
                guard.check_build_identity(self.target, changed)

    def test_identity_rejects_alternate_manifest_and_separates_selected_targets(self):
        base = self.identity(["cargo", "test", "--workspace", "--test", "integration"])
        guard.check_build_identity(self.target, base)
        for command in (
            ["cargo", "test", "--workspace", "--test", "other"],
            ["cargo", "test", "--workspace", "--lib"],
            ["cargo", "test", "--workspace", "--all-targets"],
        ):
            with self.subTest(command=command), self.assertRaisesRegex(ValueError, "identity differs"):
                guard.check_build_identity(self.target, self.identity(command))
        with self.assertRaisesRegex(ValueError, "--manifest-path"):
            self.identity(["cargo", "build", "--manifest-path", "other/Cargo.toml"])

    def test_identity_covers_plural_target_selectors_and_toolchain_files(self):
        (self.root / "rust-toolchain.toml").write_text("[toolchain]\nchannel = 'stable'\n", encoding="utf-8")
        base = self.identity(["cargo", "test", "--workspace", "--tests", "--bins", "--examples", "--benches"])
        guard.check_build_identity(self.target, base)
        for command in (
            ["cargo", "test", "--workspace", "--tests"],
            ["cargo", "test", "--workspace", "--bins"],
        ):
            with self.subTest(command=command), self.assertRaisesRegex(ValueError, "identity differs"):
                guard.check_build_identity(self.target, self.identity(command))
        toolchain = self.root / "rust-toolchain.toml"
        toolchain.write_text("[toolchain]\nchannel = 'nightly'\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "identity differs"):
            guard.check_build_identity(self.target, self.identity(["cargo", "test", "--workspace", "--tests", "--bins", "--examples", "--benches"]))

    def test_identity_covers_rustc_and_clippy_forwarded_args_and_rejects_unknown_flags(self):
        base = self.identity(["cargo", "clippy", "--workspace", "--", "--cfg", "guard_a"])
        guard.check_build_identity(self.target, base)
        changed = self.identity(["cargo", "clippy", "--workspace", "--", "--cfg", "guard_b"])
        with self.assertRaisesRegex(ValueError, "identity differs"):
            guard.check_build_identity(self.target, changed)
        with self.assertRaisesRegex(ValueError, "unsupported Cargo build option"):
            guard.validate_command(["cargo", "build", "--untracked-compile-switch"])
        with self.assertRaisesRegex(ValueError, "repeated --target"):
            guard.validate_command(["cargo", "build", "--target", "aarch64-unknown-linux-gnu", "--target=x86_64-unknown-linux-gnu"])

    def test_identity_tracks_bundled_grammar_configuration(self):
        command = ["cargo", "build"]
        for name, first, second in (("TSLP_LANGUAGES", "rust,c", "all"),
                                    ("TSLP_LINK_MODE", "static", "dynamic")):
            with self.subTest(name=name):
                baseline = guard.build_identity(self.root, command, {name: first})
                changed = guard.build_identity(self.root, command, {name: second})
                self.assertNotEqual(baseline, changed)
                self.assertEqual(baseline, guard.build_identity(self.root, command, {name: first}))

    def test_identity_fails_closed_on_unmarked_target_without_overwriting_user_files(self):
        user_file = self.target / "keep.bin"
        user_file.write_bytes(b"user data")
        with self.assertRaisesRegex(ValueError, "no build identity marker"):
            guard.check_build_identity(self.target, self.identity(["cargo", "build"]))
        self.assertEqual(user_file.read_bytes(), b"user data")
        self.assertFalse((self.target / guard.IDENTITY_FILE).exists())

    def test_nonbuild_commands_do_not_create_or_change_identity_marker(self):
        marker = self.target / guard.IDENTITY_FILE
        guard.check_build_identity(self.target, self.identity(["cargo", "build"]))
        before = marker.read_bytes()
        class Process:
            pid = 111

            def poll(self):
                return 0

            def wait(self, timeout=None):
                return 0

        for subcommand in ("metadata", "fmt", "clean"):
            guard.run_guarded(
                ["cargo", subcommand], self.root, self.target, self.cargo_home,
                1024, 0, 0, popen=lambda *_args, **_kwargs: Process(),
                free_space=lambda _: 10**12, sample_seconds=0,
            )
            self.assertEqual(marker.read_bytes(), before)

    def test_success_preflight_allows_workspace_target_and_rejects_redirect(self):
        env = {"CARGO_TARGET_DIR": str(self.target)}
        self.assertEqual(guard.resolve_target(self.root, env), self.target.resolve())
        with self.assertRaisesRegex(ValueError, "workspace target"):
            guard.resolve_target(self.root, {"CARGO_TARGET_DIR": str(self.root / "other")})

    def test_report_path_stays_inside_workspace(self):
        self.assertEqual(
            guard.resolve_report_path(self.root, Path(".tmp/resource.json")),
            (self.root / ".tmp/resource.json").resolve(),
        )
        with self.assertRaisesRegex(ValueError, "inside the workspace"):
            guard.resolve_report_path(self.root, self.root.parent / "outside.json")
        existing = self.root / "report.json"
        existing.write_text("keep", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "already exists"):
            guard.resolve_report_path(self.root, existing)

    def test_directory_size_counts_regular_files(self):
        nested = self.target / "nested"
        nested.mkdir()
        regular_file = nested / "data"
        regular_file.write_bytes(b"content")
        (self.target / "empty").touch()
        if hasattr(os, "mkfifo"):
            os.mkfifo(nested / "pipe")
        self.assertEqual(guard.directory_size(self.target), 7)
        self.assertEqual(guard.directory_size(regular_file), 0)
        self.assertEqual(guard.directory_size(self.target / "missing"), 0)

    def test_directory_size_uses_selected_root_and_excludes_nested_symlinks(self):
        external_file = self.cargo_home / "outside"
        external_file.write_bytes(b"external")
        (self.target / "local").write_bytes(b"local")
        root_alias = self.root / "selected"
        try:
            root_alias.symlink_to(self.target, target_is_directory=True)
        except (NotImplementedError, OSError) as error:
            self.skipTest(f"symlinks unavailable: {error}")
        (self.target / "outside_dir").symlink_to(self.cargo_home, target_is_directory=True)
        (self.target / "outside_file").symlink_to(external_file)
        self.assertEqual(guard.directory_size(root_alias), 5)

    def test_directory_size_ignores_disappearing_directories_and_surfaces_denials(self):
        nested = self.target / "nested"
        nested.mkdir()
        (self.target / "local").write_bytes(b"local")
        scandir = os.scandir

        def interrupted_scan(directory):
            if Path(directory) == nested:
                raise scan_error
            return scandir(directory)

        with patch.object(guard.os, "scandir", side_effect=interrupted_scan):
            scan_error = FileNotFoundError("directory disappeared")
            self.assertEqual(guard.directory_size(self.target), 5)
            scan_error = PermissionError("directory unreadable")
            with self.assertRaisesRegex(PermissionError, "directory unreadable"):
                guard.preflight(self.target, self.cargo_home, 100, 0, 0, free_bytes=100)

    def test_preflight_rejects_existing_target_at_budget(self):
        (self.target / "large.bin").write_bytes(b"x" * 32)
        with self.assertRaisesRegex(ValueError, "existing target"):
            guard.preflight(self.target, self.cargo_home, 32, 0, 0, free_bytes=100)

    def test_preflight_rejects_volume_reserve_breach(self):
        with self.assertRaisesRegex(ValueError, "free space"):
            guard.preflight(self.target, self.cargo_home, 100, 90, 10, free_bytes=100)

    def test_command_requires_explicit_cargo_and_uses_argument_vector(self):
        guard.validate_command(["cargo", "check", "-p", "aden-asm"])
        for command in ([], ["python", "-c", "pass"], ["cargo", "bad\x00arg"]):
            with self.subTest(command=command), self.assertRaises(ValueError):
                guard.validate_command(command)

    def test_supported_subcommands_and_toolchain_selector(self):
        for subcommand in guard.SUPPORTED_COMMANDS:
            with self.subTest(subcommand=subcommand):
                guard.validate_command(["cargo", "+stable", subcommand])

    def test_rejects_output_redirect_flags_and_unbounded_subcommands(self):
        bad_commands = (
            ["cargo", "check", "--target-dir", "D:/outside"],
            ["cargo", "build", "--target-dir=D:/outside"],
            ["cargo", "test", "--build-dir", "D:/outside"],
            ["cargo", "build", "--build-dir-extra=outside"],
            ["cargo", "build", "--artifact-dir", "D:/outside"],
            ["cargo", "bench", "--artifact-dir=D:/outside"],
            ["cargo", "build", "--out-dir", "D:/outside"],
            ["cargo", "check", "-Z", "unstable-options"],
            ["cargo", "+nightly", "build", "-Zunstable-options"],
            ["cargo", "check", "--config", "build.target-dir='D:/outside'"],
            ["cargo", "check", "--config=build.build-dir='D:/outside'"],
            ["cargo", "build", "--", "--out-dir", "D:/outside"],
            ["cargo", "clippy", "--", "--out-dir=D:/outside"],
            ["cargo", "install", "aden"],
            ["cargo", "publish"],
            ["cargo", "run"],
            ["cargo", "rustc"],
        )
        for command in bad_commands:
            with self.subTest(command=command), self.assertRaises(ValueError):
                guard.validate_command(command)

    def test_test_separator_arguments_are_not_treated_as_cargo_flags(self):
        guard.validate_command(
            [
                "cargo",
                "test",
                "--",
                "--target-dir",
                "test-value",
                "--build-dir=x",
                "--artifact-dir=x",
                "--out-dir=x",
                "--config=y",
                "-Z",
            ]
        )
        with self.assertRaisesRegex(ValueError, "redirection"):
            guard.validate_command(["cargo", "test", "--target-dir", "D:/outside", "--", "case"])

    def test_rejects_output_redirect_environment(self):
        for name in guard.OUTPUT_REDIRECT_ENV:
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, name):
                guard.validate_environment(self.root, self.target, {name: "D:/outside"})
        with self.assertRaisesRegex(ValueError, "workspace target"):
            guard.validate_environment(
                self.root,
                self.target,
                {"CARGO_TARGET_DIR": str(self.root / "outside")},
            )
        with self.assertRaisesRegex(ValueError, "CARGO_UNSTABLE"):
            guard.validate_environment(self.root, self.target, {"CARGO_UNSTABLE_BUILD_DIR": "1"})
        with self.assertRaisesRegex(ValueError, "CARGO_BUILD_ARTIFACT_DIR"):
            guard.validate_environment(
                self.root, self.target, {"CARGO_BUILD_ARTIFACT_DIR": "D:/outside"}
            )

    def test_configured_output_paths_are_parsed_and_forced_to_guarded_target(self):
        config_dir = self.root / ".cargo"
        config_dir.mkdir(exist_ok=True)
        (config_dir / "config.toml").write_text(
            "[build]\n"
            'target-dir = "D:/outside/target"\n'
            'build-dir = "D:/outside/build"\n'
            'artifact-dir = "D:/outside/artifacts"\n',
            encoding="utf-8",
        )
        configured = guard.cargo_output_config(self.root, self.cargo_home)
        self.assertEqual(
            configured,
            {
                "target-dir": "D:/outside/target",
                "build-dir": "D:/outside/build",
                "artifact-dir": "D:/outside/artifacts",
            },
        )
        with self.assertRaisesRegex(ValueError, "artifact-dir configuration"):
            guard.validate_cargo_output_config(configured)
        guard.validate_cargo_output_config(
            {"target-dir": "D:/outside/target", "build-dir": "D:/outside/build"}
        )
        env = guard.cargo_environment(self.target, self.cargo_home, {})
        for name in guard.OUTPUT_REDIRECT_ENV:
            self.assertEqual(env[name], str(self.target.resolve()))
        self.assertEqual(env["CARGO_TARGET_DIR"], str(self.target.resolve()))
        self.assertNotIn("CARGO_BUILD_ARTIFACT_DIR", env)

    def test_successful_fake_cargo_receives_low_disk_environment(self):
        captured = {}

        class Process:
            pid = 101

            def poll(self):
                return 0

            def wait(self, timeout=None):
                return 0

        def fake_popen(command, **kwargs):
            captured["command"] = command
            captured.update(kwargs)
            return Process()

        result = guard.run_guarded(
            ["cargo", "check", "-p", "aden-asm"],
            self.root,
            self.target,
            self.cargo_home,
            1024,
            0,
            0,
            popen=fake_popen,
            free_space=lambda _: 10**12,
            sample_seconds=0,
        )
        self.assertEqual(result["exit_code"], 0)
        self.assertEqual(captured["command"], ["cargo", "check", "-p", "aden-asm"])
        self.assertEqual(captured["cwd"], str(self.root))
        self.assertNotIn("shell", captured)
        env = captured["env"]
        self.assertEqual(env["CARGO_TARGET_DIR"], str(self.target.resolve()))
        for name in guard.OUTPUT_REDIRECT_ENV:
            self.assertEqual(env[name], str(self.target.resolve()))
        self.assertNotIn("CARGO_BUILD_ARTIFACT_DIR", env)
        self.assertNotIn("CARGO_UNSTABLE_BUILD_DIR", env)
        self.assertEqual(env["CARGO_HOME"], str(self.cargo_home.resolve()))
        self.assertEqual(env["CARGO_BUILD_JOBS"], "2")
        self.assertEqual(env["CARGO_INCREMENTAL"], "0")
        self.assertEqual(env["CARGO_PROFILE_DEV_DEBUG"], "0")

    def test_growth_limit_stops_fake_child_tree(self):
        class Process:
            pid = 102

            def __init__(self):
                self.stopped = False

            def poll(self):
                return -9 if self.stopped else None

            def wait(self, timeout=None):
                return -9

        process = Process()
        stopped = []
        result = guard.run_guarded(
            ["cargo", "check"],
            self.root,
            self.target,
            self.cargo_home,
            10,
            0,
            0,
            popen=lambda *_args, **_kwargs: process,
            sampler=lambda _path: 11,
            free_space=lambda _: 10**12,
            stopper=lambda child: (stopped.append(child.pid), setattr(child, "stopped", True)),
            sample_seconds=0,
        )
        self.assertEqual(stopped, [102])
        self.assertTrue(result["guard_stopped"])
        self.assertEqual(result["stop_reason"], "target_and_cargo_cache_growth_reached_budget")

    def test_cargo_cache_growth_is_counted(self):
        class Process:
            pid = 104

            def __init__(self):
                self.stopped = False

            def poll(self):
                return -9 if self.stopped else None

            def wait(self, timeout=None):
                return -9

        process = Process()
        cache_file = self.cargo_home / "registry" / "cache" / "new.crate"

        def sampler(_path):
            cache_file.parent.mkdir(parents=True, exist_ok=True)
            cache_file.write_bytes(b"x" * 11)
            return 0

        result = guard.run_guarded(
            ["cargo", "check"],
            self.root,
            self.target,
            self.cargo_home,
            10,
            0,
            0,
            popen=lambda *_args, **_kwargs: process,
            sampler=sampler,
            free_space=lambda _: 10**12,
            stopper=lambda child: setattr(child, "stopped", True),
            sample_seconds=0,
        )
        self.assertEqual(result["cargo_cache_growth_bytes_final"], 11)
        self.assertTrue(result["limit_exceeded"])

    def test_finished_command_over_budget_is_reported_without_claiming_kill(self):
        class Process:
            pid = 105

            def poll(self):
                return 0

            def wait(self, timeout=None):
                return 0

        result = guard.run_guarded(
            ["cargo", "check"],
            self.root,
            self.target,
            self.cargo_home,
            10,
            0,
            0,
            popen=lambda *_args, **_kwargs: Process(),
            sampler=lambda _path: 11,
            free_space=lambda _: 10**12,
            sample_seconds=0,
        )
        self.assertTrue(result["limit_exceeded"])
        self.assertFalse(result["guard_stopped"])
        self.assertEqual(result["exit_code"], 0)
        self.assertEqual(guard.guarded_exit_code(result), 1)

    def test_sampler_error_stops_and_reaps_running_fake_process(self):
        class Process:
            pid = 106

            def __init__(self):
                self.stopped = False
                self.reaped = False

            def poll(self):
                return -9 if self.stopped else None

            def wait(self, timeout=None):
                self.reaped = True
                return -9

        process = Process()
        stopped = []

        def fail_probe(_path):
            raise OSError("injected disk probe failure")

        with self.assertRaisesRegex(guard.GuardMonitoringError, "disk probe failure"):
            guard.run_guarded(
                ["cargo", "check"],
                self.root,
                self.target,
                self.cargo_home,
                1024,
                0,
                0,
                popen=lambda *_args, **_kwargs: process,
                sampler=fail_probe,
                free_space=lambda _: 10**12,
                stopper=lambda child: (stopped.append(child.pid), setattr(child, "stopped", True)),
                sample_seconds=0,
            )
        self.assertEqual(stopped, [106])
        self.assertTrue(process.reaped)

    def test_keyboard_interrupt_stops_and_reaps_running_fake_process(self):
        class Process:
            pid = 107

            def __init__(self):
                self.stopped = False
                self.reaped = False

            def poll(self):
                return -9 if self.stopped else None

            def wait(self, timeout=None):
                self.reaped = True
                return -9

        process = Process()
        stopped = []

        def interrupt_probe(_path):
            raise KeyboardInterrupt

        with self.assertRaises(KeyboardInterrupt):
            guard.run_guarded(
                ["cargo", "check"],
                self.root,
                self.target,
                self.cargo_home,
                1024,
                0,
                0,
                popen=lambda *_args, **_kwargs: process,
                sampler=interrupt_probe,
                free_space=lambda _: 10**12,
                stopper=lambda child: (stopped.append(child.pid), setattr(child, "stopped", True)),
                sample_seconds=0,
            )
        self.assertEqual(stopped, [107])
        self.assertTrue(process.reaped)

    def test_windows_tree_stop_uses_taskkill_without_shell(self):
        class Process:
            pid = 103

        with patch.object(guard.os, "name", "nt"), patch.object(guard.subprocess, "run") as run:
            guard.stop_process_tree(Process())
        self.assertEqual(run.call_args.args[0], ["taskkill", "/PID", "103", "/T", "/F"])
        self.assertFalse(run.call_args.kwargs.get("shell", False))


if __name__ == "__main__":
    unittest.main()
