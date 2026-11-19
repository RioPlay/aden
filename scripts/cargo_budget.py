#!/usr/bin/env python3
"""Run one explicit Cargo command with a sampled, project-local storage guard."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import signal
import stat
import subprocess
import sys
import time
import tomllib
from typing import Callable, Sequence

GIB = 1024**3
DEFAULT_BUDGET = 5 * GIB
DEFAULT_RESERVE = 10 * GIB
DEFAULT_HEADROOM = 512 * 1024**2
SAMPLE_SECONDS = 1.0
SUPPORTED_COMMANDS = {"build", "check", "test", "clippy", "bench", "metadata", "fmt", "clean"}
OUTPUT_REDIRECT_ENV = (
    "CARGO_BUILD_TARGET_DIR",
    "CARGO_BUILD_BUILD_DIR",
)
UNSUPPORTED_OUTPUT_ENV = ("CARGO_BUILD_ARTIFACT_DIR",)
IDENTITY_FILE = ".cargo-budget-identity.json"
BUILD_COMMANDS = {"build", "check", "test", "clippy", "bench"}


class GuardMonitoringError(RuntimeError):
    """A storage monitor failed after the Cargo child was started."""


class GuardLaunchError(RuntimeError):
    """Cargo could not be started after preflight completed."""


def directory_size(path: Path) -> int:
    """Count regular-file bytes beneath the selected root, without following nested links."""
    try:
        if not stat.S_ISDIR(path.stat().st_mode):
            return 0
    except (FileNotFoundError, NotADirectoryError):
        return 0

    total = 0
    pending = [os.fspath(path)]
    while pending:
        directory = pending.pop()
        try:
            with os.scandir(directory) as entries:
                for entry in entries:
                    try:
                        metadata = entry.stat(follow_symlinks=False)
                    except FileNotFoundError:
                        continue
                    if stat.S_ISREG(metadata.st_mode):
                        total += metadata.st_size
                    elif stat.S_ISDIR(metadata.st_mode):
                        if os.name == "nt" and metadata.st_reparse_tag == stat.IO_REPARSE_TAG_MOUNT_POINT:
                            continue
                        pending.append(entry.path)
        except FileNotFoundError:
            continue
    return total


def cargo_cache_paths(cargo_home: Path) -> tuple[Path, ...]:
    return (
        cargo_home / "registry" / "cache",
        cargo_home / "registry" / "index",
        cargo_home / "registry" / "src",
        cargo_home / "git" / "checkouts",
        cargo_home / "git" / "db",
    )


def cargo_config_paths(root: Path, cargo_home: Path) -> tuple[Path, ...]:
    directories = (root.resolve(), *root.resolve().parents)
    paths: list[Path] = [cargo_home / "config", cargo_home / "config.toml"]
    for directory in reversed(directories):
        paths.extend((directory / ".cargo" / "config", directory / ".cargo" / "config.toml"))
    return tuple(dict.fromkeys(paths))


def cargo_output_config(root: Path, cargo_home: Path) -> dict[str, object]:
    """Read configured output paths so tests and reports cover actual TOML input."""
    output: dict[str, object] = {}
    for config_path in cargo_config_paths(root, cargo_home):
        if not config_path.is_file():
            continue
        with config_path.open("rb") as stream:
            config = tomllib.load(stream)
        build = config.get("build", {})
        if isinstance(build, dict):
            for key in ("target-dir", "build-dir", "artifact-dir"):
                if key in build:
                    output[key] = build[key]
    return output


def validate_cargo_output_config(config: dict[str, object]) -> None:
    if "artifact-dir" in config:
        raise ValueError("Cargo artifact-dir configuration is unsupported by the storage guard")


def resolve_cargo_home(root: Path, configured: str | None) -> Path:
    path = Path(configured).expanduser() if configured else Path.home() / ".cargo"
    if not path.is_absolute():
        path = root / path
    return path.resolve()


def cache_size(cargo_home: Path) -> int:
    return sum(directory_size(path) for path in cargo_cache_paths(cargo_home))


def volume_free(path: Path) -> int:
    existing = path
    while not existing.exists() and existing != existing.parent:
        existing = existing.parent
    return shutil.disk_usage(existing).free


def resolve_target(root: Path, env: dict[str, str]) -> Path:
    """Enforce the repository's single shared target directory."""
    workspace = root.resolve()
    configured_target = workspace / "target"
    if configured_target.is_symlink():
        raise ValueError("workspace target directory cannot be a symlink")
    target = configured_target.resolve()
    if not target.is_relative_to(workspace):
        raise ValueError("workspace target directory resolves outside the workspace")
    configured = env.get("CARGO_TARGET_DIR")
    if configured:
        candidate = Path(configured)
        if not candidate.is_absolute():
            candidate = root / candidate
        if candidate.resolve() != target:
            raise ValueError(
                f"CARGO_TARGET_DIR must be the workspace target directory: {target}"
            )
    return target


def resolve_report_path(root: Path, report: Path | None) -> Path | None:
    if report is None:
        return None
    workspace = root.resolve()
    candidate = report if report.is_absolute() else workspace / report
    candidate = candidate.resolve()
    if not candidate.is_relative_to(workspace):
        raise ValueError("resource report path must stay inside the workspace")
    if candidate.exists() or candidate.is_symlink():
        raise ValueError("resource report path already exists; choose a new path")
    return candidate


def preflight(
    target: Path,
    cargo_home: Path,
    budget_bytes: int,
    reserve_bytes: int,
    minimum_headroom_bytes: int,
    free_bytes: int | None = None,
) -> dict[str, int]:
    """Fail before launch when existing output or volume headroom is unsuitable."""
    target_bytes = directory_size(target)
    target_free = volume_free(target.parent) if free_bytes is None else free_bytes
    cache_free = volume_free(cargo_home) if free_bytes is None else free_bytes
    if target_bytes >= budget_bytes:
        raise ValueError(
            f"existing target is {target_bytes} bytes, at or above the {budget_bytes} byte cap"
        )
    required = reserve_bytes + minimum_headroom_bytes
    if min(target_free, cache_free) <= required:
        raise ValueError(
            "free space on the target or Cargo cache volume does not exceed "
            f"reserve plus headroom {required} bytes "
            f"(target {target_free}, Cargo cache {cache_free})"
        )
    return {
        "target_bytes_at_start": target_bytes,
        "cargo_cache_bytes_at_start": cache_size(cargo_home),
        "target_volume_free_bytes_at_start": target_free,
        "cargo_cache_volume_free_bytes_at_start": cache_free,
    }


def _sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def parse_build_arguments(command: Sequence[str], env: dict[str, str]) -> dict[str, object]:
    """Normalize every supported build option; reject flags not represented in identity."""
    args = list(command[1:])
    toolchain = None
    if args and args[0].startswith("+"):
        toolchain = args.pop(0)[1:]
    subcommand = args.pop(0)
    if any(arg == "--manifest-path" or arg.startswith("--manifest-path=") for arg in args):
        raise ValueError("--manifest-path is not supported by the build identity guard")
    selected: list[str] = []
    targets: list[str] = []
    features: list[str] = []
    all_features = False
    no_default_features = False
    target = env.get("CARGO_BUILD_TARGET")
    target_option_seen = False
    profile = "debug"
    forwarded_rustc: list[str] = []
    if "--" in args:
        separator = args.index("--")
        if subcommand in {"build", "check", "clippy"}:
            forwarded_rustc = args[separator + 1 :]
        args = args[:separator]
    index = 0
    value_options = {"-p", "--package", "--exclude", "--features", "-F", "--target", "--bin", "--example", "--test", "--bench", "--profile", "--jobs", "-j", "--color", "--message-format"}
    bool_options = {
        "--release", "-r", "--lib", "--all-targets", "--all", "--workspace", "-w",
        "--all-features", "--no-default-features", "--bins", "--examples", "--tests", "--benches",
        "--locked", "--offline", "--frozen", "--keep-going", "--no-run", "--no-fail-fast",
        "--doc", "--fix", "--allow-dirty", "--allow-staged", "--no-deps", "--timings",
        "--verbose", "-v", "-vv", "--quiet", "-q",
    }
    while index < len(args):
        arg = args[index]
        if arg in value_options:
            if index + 1 >= len(args):
                raise ValueError(f"missing value for Cargo build option: {arg}")
            value = args[index + 1]
            if arg in ("-p", "--package", "--exclude"):
                selected.append(("exclude:" if arg == "--exclude" else "package:") + value)
            elif arg in ("--features", "-F"):
                features.extend(part for part in value.replace(",", " ").split() if part)
            elif arg in ("--bin", "--example", "--test", "--bench"):
                targets.append(arg[2:] + ":" + value)
            elif arg == "--target":
                if target_option_seen:
                    raise ValueError("repeated --target options are not supported by the build identity guard")
                target_option_seen = True
                target = value
            elif arg == "--profile":
                profile = value
            index += 1
        elif arg.startswith("--") and "=" in arg:
            option, value = arg.split("=", 1)
            if option in value_options:
                if option in ("-p", "--package", "--exclude"):
                    selected.append(("exclude:" if option == "--exclude" else "package:") + value)
                elif option in ("--features", "-F"):
                    features.extend(part for part in value.replace(",", " ").split() if part)
                elif option in ("--bin", "--example", "--test", "--bench"):
                    targets.append(option[2:] + ":" + value)
                elif option == "--target":
                    if target_option_seen:
                        raise ValueError("repeated --target options are not supported by the build identity guard")
                    target_option_seen = True
                    target = value
                elif option == "--profile":
                    profile = value
            elif option == "--timings":
                pass
            else:
                raise ValueError(f"unsupported Cargo build option for identity guard: {option}")
        elif arg in bool_options:
            if arg in ("--release", "-r"):
                profile = "release"
            elif arg in ("--all",):
                selected.append("workspace")
            elif arg in ("--workspace", "-w"):
                selected.append("workspace")
            elif arg == "--all-features":
                all_features = True
            elif arg == "--no-default-features":
                no_default_features = True
            elif arg == "--lib":
                targets.append("lib")
            elif arg == "--all-targets":
                targets.append("all-targets")
            elif arg in ("--bins", "--examples", "--tests", "--benches"):
                targets.append(arg[2:])
            elif arg == "--doc":
                targets.append("doc")
        elif arg.startswith("-"):
            if arg.startswith("-p") and len(arg) > 2:
                selected.append("package:" + arg[2:])
            elif arg.startswith("-F") and len(arg) > 2:
                features.extend(part for part in arg[2:].lstrip("=").replace(",", " ").split() if part)
            elif arg.startswith("-j") and len(arg) > 2:
                pass
            else:
                raise ValueError(f"unsupported Cargo build option for identity guard: {arg}")
        elif subcommand == "test":
            # Cargo's positional test-name filter does not change compiled artifacts.
            pass
        else:
            raise ValueError(f"unsupported positional Cargo build argument for identity guard: {arg}")
        index += 1
    if not selected:
        selected = ["default"]
    return {
        "version": 2, "command_class": subcommand, "package_selection": sorted(selected),
        "features": sorted(features), "all_features": all_features,
        "no_default_features": no_default_features, "profile": profile, "target": target,
        "target_selection": sorted(targets), "forwarded_rustc_args": forwarded_rustc,
        "toolchain": toolchain or env.get("RUSTUP_TOOLCHAIN"),
    }


def build_identity(root: Path, command: Sequence[str], env: dict[str, str]) -> dict[str, object]:
    """Fingerprint build inputs that can make cached target artifacts incompatible."""
    parsed = parse_build_arguments(command, env)
    files = [root / "Cargo.toml", root / "Cargo.lock"]
    cargo_home = resolve_cargo_home(root, env.get("CARGO_HOME"))
    files.extend(cargo_config_paths(root, cargo_home))
    manifests = [
        path for path in root.rglob("Cargo.toml")
        if not any(part in {"target", ".git", ".cargo"} for part in path.relative_to(root).parts)
    ]
    files.extend(manifests)
    for directory in (root.resolve(), *root.resolve().parents):
        files.extend((directory / "rust-toolchain", directory / "rust-toolchain.toml"))
    rustup_settings = Path(env.get("RUSTUP_HOME", Path.home() / ".rustup")) / "settings.toml"
    files.append(rustup_settings)
    file_hashes = {str(path.relative_to(root)) if path.is_relative_to(root) else "cargo-config:" + str(path): _sha256_file(path)
                   for path in dict.fromkeys(files) if path.is_file()}
    env_prefixes = (
        "CARGO_PROFILE_", "CARGO_TARGET_", "CARGO_BUILD_", "RUSTC_", "RUSTDOC",
        "CC", "CXX", "AR", "CFLAGS", "CXXFLAGS", "PKG_CONFIG", "CMAKE", "BINDGEN", "TSLP_",
    )
    env_names = {"RUSTUP_TOOLCHAIN", "RUSTC", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET",
                 "CARGO_BUILD_RUSTC", "CARGO_BUILD_RUSTC_WRAPPER", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
                 "RUSTDOC", "RUSTDOCFLAGS", "CARGO_ENCODED_RUSTDOCFLAGS"}
    env_names.update(name for name in env if name.startswith(env_prefixes))
    relevant_env = {name: hashlib.sha256(env[name].encode()).hexdigest() for name in sorted(env_names) if env.get(name) is not None}
    return {**parsed, "environment": relevant_env, "manifest_hashes": file_hashes}


def check_build_identity(target: Path, identity: dict[str, object]) -> None:
    """Reject incompatible or unknown nonempty target trees without changing them."""
    marker = target / IDENTITY_FILE
    files = [p for p in target.rglob("*") if p.is_file() and p != marker]
    if marker.exists():
        try:
            stored = json.loads(marker.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise ValueError(f"build identity marker is corrupt; run cargo clean: {error}") from error
        if stored != identity:
            raise ValueError("target build identity differs from this command; run cargo clean before switching configuration")
        return
    if files:
        raise ValueError("target has existing files but no build identity marker; run cargo clean before building")
    target.mkdir(parents=True, exist_ok=True)
    marker.write_text(json.dumps(identity, sort_keys=True, indent=2) + "\n", encoding="utf-8")


def cargo_environment(target: Path, cargo_home: Path, env: dict[str, str]) -> dict[str, str]:
    result = env.copy()
    for name in tuple(result):
        if name.startswith("CARGO_UNSTABLE") or name in UNSUPPORTED_OUTPUT_ENV:
            result.pop(name, None)
    result.update({name: str(target) for name in OUTPUT_REDIRECT_ENV})
    result.update(
        CARGO_TARGET_DIR=str(target),
        CARGO_HOME=str(cargo_home),
        CARGO_BUILD_JOBS="2",
        CARGO_INCREMENTAL="0",
        CARGO_PROFILE_DEV_DEBUG="0",
        CARGO_PROFILE_DEV_INCREMENTAL="false",
        CARGO_PROFILE_TEST_DEBUG="0",
        CARGO_PROFILE_TEST_INCREMENTAL="false",
        CARGO_PROFILE_RELEASE_DEBUG="0",
        CARGO_PROFILE_RELEASE_STRIP="symbols",
        CARGO_PROFILE_RELEASE_INCREMENTAL="false",
    )
    # Cargo always runs from the workspace root, including when invoked elsewhere.
    return result


def validate_command(command: Sequence[str]) -> None:
    if not command or Path(command[0]).name.casefold() not in {"cargo", "cargo.exe"}:
        raise ValueError("provide an explicit Cargo command after --, for example: -- cargo check -p aden-asm")
    if any("\x00" in arg for arg in command):
        raise ValueError("Cargo arguments cannot contain NUL bytes")

    args = list(command[1:])
    if args and args[0].startswith("+"):
        if not args[0][1:] or any(c.isspace() for c in args[0]):
            raise ValueError("invalid Cargo toolchain selector")
        args.pop(0)
    if not args or args[0] not in SUPPORTED_COMMANDS:
        raise ValueError(
            "supported Cargo subcommands are: " + ", ".join(sorted(SUPPORTED_COMMANDS))
        )

    # Cargo accepts a `--` separator for tool-specific arguments. Only inspect
    # Cargo's own arguments before it, especially for `cargo test -- ...`.
    subcommand = args[0]
    cargo_args = args[1:]
    forwarded_args: list[str] = []
    if "--" in cargo_args:
        separator = cargo_args.index("--")
        forwarded_args = cargo_args[separator + 1 :]
        cargo_args = cargo_args[:separator]
    for arg in cargo_args:
        if (
            arg == "--target-dir"
            or arg.startswith("--target-dir=")
            or arg.startswith("--build-dir")
            or arg.startswith("--artifact-dir")
            or arg.startswith("--out-dir")
            or arg.startswith("--config")
            or arg == "-Z"
            or arg.startswith("-Z")
        ):
            raise ValueError(f"Cargo output or config redirection is not supported: {arg}")
    # Build-like commands forward arguments after `--` to rustc, where
    # `--out-dir` could place compiler artifacts outside Cargo's target tree.
    if subcommand in {"build", "check", "clippy"} and any(
        arg.startswith(("--out-dir", "--artifact-dir", "-Z")) for arg in forwarded_args
    ):
        raise ValueError("rustc --out-dir is not supported by the storage guard")
    if subcommand in BUILD_COMMANDS:
        parse_build_arguments(command, os.environ)


def validate_environment(root: Path, target: Path, env: dict[str, str]) -> None:
    for name in OUTPUT_REDIRECT_ENV:
        configured = env.get(name)
        if configured:
            path = Path(configured)
            if not path.is_absolute():
                path = root / path
            if path.resolve() != target.resolve():
                raise ValueError(f"{name} redirects Cargo output outside the guarded target")
    for name in UNSUPPORTED_OUTPUT_ENV:
        if env.get(name):
            raise ValueError(f"{name} is not a supported Cargo output setting")
    if any(name.startswith("CARGO_UNSTABLE") and value for name, value in env.items()):
        raise ValueError("CARGO_UNSTABLE environment settings are not supported")
    if resolve_target(root, env) != target.resolve():
        raise ValueError("Cargo target path does not match the guarded workspace target")


def stop_process_tree(process: subprocess.Popen[object]) -> None:
    """Stop the launched process and descendants without invoking a shell."""
    if os.name == "nt":
        subprocess.run(
            ["taskkill", "/PID", str(process.pid), "/T", "/F"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        pass
    # The Cargo parent can exit while a descendant is still unwinding. Reap any
    # remaining members of the isolated group after the grace period.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def cleanup_process_tree(
    process: subprocess.Popen[object], stopper: Callable[[subprocess.Popen[object]], None]
) -> None:
    """Best-effort stop and reap after any post-launch monitor failure."""

    def kill_parent() -> None:
        try:
            process.kill()
        except BaseException:
            pass

    try:
        stopper(process)
    except BaseException:
        kill_parent()
    try:
        process.wait(timeout=5)
        return
    except subprocess.TimeoutExpired:
        pass
    except BaseException:
        return
    try:
        stopper(process)
    except BaseException:
        kill_parent()
    kill_parent()
    try:
        process.wait(timeout=5)
    except BaseException:
        pass


def run_guarded(
    command: Sequence[str],
    root: Path,
    target: Path,
    cargo_home: Path,
    budget_bytes: int,
    reserve_bytes: int,
    minimum_headroom_bytes: int,
    *,
    popen: Callable[..., subprocess.Popen[object]] = subprocess.Popen,
    sampler: Callable[[Path], int] = directory_size,
    free_space: Callable[[Path], int] | None = None,
    stopper: Callable[[subprocess.Popen[object]], None] = stop_process_tree,
    sample_seconds: float = SAMPLE_SECONDS,
) -> dict[str, object]:
    """Run Cargo and sample target/cache growth and free space until it exits."""
    validate_command(command)
    validate_environment(root, target, os.environ)
    configured_outputs = cargo_output_config(root, cargo_home)
    validate_cargo_output_config(configured_outputs)
    start = preflight(
        target, cargo_home, budget_bytes, reserve_bytes, minimum_headroom_bytes
    )
    args = list(command[1:])
    if args and args[0].startswith("+"):
        args.pop(0)
    if args and args[0] in BUILD_COMMANDS:
        check_build_identity(target, build_identity(root, command, os.environ))
    baseline_target = start["target_bytes_at_start"]
    baseline_cache = start["cargo_cache_bytes_at_start"]
    env = cargo_environment(target, cargo_home, os.environ)
    kwargs: dict[str, object] = {
        "cwd": str(root),
        "env": env,
        "stdout": None,
        "stderr": None,
    }
    if os.name != "nt":
        kwargs["start_new_session"] = True
    began = time.monotonic()
    peak_counted_bytes = baseline_target
    stopped_reason = None
    termination_requested = False

    def sample() -> tuple[int, int, int, int, int]:
        target_now = sampler(target)
        cache_now = cache_size(cargo_home)
        cache_growth = max(0, cache_now - baseline_cache)
        counted_bytes = target_now + cache_growth
        target_free = (
            free_space(target.parent) if free_space is not None else volume_free(target.parent)
        )
        cache_free = (
            free_space(cargo_home) if free_space is not None else volume_free(cargo_home)
        )
        return target_now, cache_now, counted_bytes, target_free, cache_free

    process: subprocess.Popen[object] | None = None
    try:
        process = popen(list(command), **kwargs)
        while process.poll() is None:
            _target_now, _cache_now, counted_bytes, target_free, cache_free = sample()
            peak_counted_bytes = max(peak_counted_bytes, counted_bytes)
            if counted_bytes > budget_bytes:
                stopped_reason = "target_and_cargo_cache_growth_reached_budget"
            elif min(target_free, cache_free) <= reserve_bytes:
                stopped_reason = "free_space_reached_reserve"
            if stopped_reason:
                stopper(process)
                termination_requested = True
                break
            time.sleep(sample_seconds)

        return_code = process.wait()
        target_final, cache_final, final_counted_bytes, target_free_final, cache_free_final = sample()
    except BaseException as error:
        if process is not None:
            cleanup_process_tree(process, stopper)
        if process is None and isinstance(error, Exception):
            raise GuardLaunchError(f"could not start Cargo: {error}") from error
        if process is not None and isinstance(error, Exception):
            raise GuardMonitoringError(f"monitoring failed after Cargo started: {error}") from error
        raise
    peak_counted_bytes = max(peak_counted_bytes, final_counted_bytes)
    if stopped_reason is None:
        if final_counted_bytes > budget_bytes:
            stopped_reason = "target_and_cargo_cache_growth_reached_budget"
        elif min(target_free_final, cache_free_final) <= reserve_bytes:
            stopped_reason = "free_space_reached_reserve"
    report: dict[str, object] = {
        "exit_code": return_code,
        "guard_stopped": termination_requested,
        "limit_exceeded": stopped_reason is not None,
        "stop_reason": stopped_reason,
        "target_bytes_at_start": baseline_target,
        "target_bytes_final": target_final,
        "cargo_cache_bytes_at_start": baseline_cache,
        "cargo_cache_bytes_final": cache_final,
        "cargo_cache_growth_bytes_final": max(0, cache_final - baseline_cache),
        "peak_sampled_budget_counted_bytes": peak_counted_bytes,
        "budget_bytes": budget_bytes,
        "target_volume_free_bytes_at_start": start["target_volume_free_bytes_at_start"],
        "cargo_cache_volume_free_bytes_at_start": start[
            "cargo_cache_volume_free_bytes_at_start"
        ],
        "target_volume_free_bytes_final": target_free_final,
        "cargo_cache_volume_free_bytes_final": cache_free_final,
        "reserve_bytes": reserve_bytes,
        "elapsed_seconds": round(time.monotonic() - began, 2),
        "target_dir": str(target),
        "cargo_output_config_keys_overridden": sorted(configured_outputs),
        "note": "Sampled process guard, not an OS filesystem quota; brief spikes between samples may be missed.",
    }
    return report


def guarded_exit_code(report: dict[str, object]) -> int:
    """Make an over-budget final sample fail even if Cargo exited successfully."""
    return 1 if report["limit_exceeded"] else int(report["exit_code"])


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--budget-gib", type=float, default=DEFAULT_BUDGET / GIB)
    parser.add_argument("--reserve-gib", type=float, default=DEFAULT_RESERVE / GIB)
    parser.add_argument(
        "--minimum-headroom-mib", type=float, default=DEFAULT_HEADROOM / 1024**2
    )
    parser.add_argument("--report", type=Path, help="optional JSON report path inside the workspace")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    values = (args.budget_gib, args.reserve_gib, args.minimum_headroom_mib)
    if not all(math.isfinite(value) for value in values):
        parser.error("budget, reserve and headroom must be finite numbers")
    if args.budget_gib <= 0 or args.reserve_gib < 0 or args.minimum_headroom_mib < 0:
        parser.error("budget must be positive and reserve/headroom cannot be negative")
    root = Path(__file__).resolve().parents[1]
    env = os.environ.copy()
    try:
        target = resolve_target(root, env)
        report_path = resolve_report_path(root, args.report)
        report = run_guarded(
            command,
            root,
            target,
            resolve_cargo_home(root, env.get("CARGO_HOME")),
            int(args.budget_gib * GIB),
            int(args.reserve_gib * GIB),
            int(args.minimum_headroom_mib * 1024**2),
        )
    except GuardLaunchError as error:
        print(f"cargo budget launch failed: {error}", file=sys.stderr)
        return 2
    except GuardMonitoringError as error:
        print(f"cargo budget monitoring failed: {error}", file=sys.stderr)
        return 2
    except (OSError, ValueError) as error:
        print(f"cargo budget preflight failed: {error}", file=sys.stderr)
        return 2
    serialized = json.dumps(report, indent=2)
    print(serialized)
    if report_path:
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(serialized + "\n", encoding="utf-8")
    return guarded_exit_code(report)


if __name__ == "__main__":
    raise SystemExit(main())
