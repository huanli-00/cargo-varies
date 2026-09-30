#!/usr/bin/env python3
"""Run generated backend targets from generation result outputs.

This script consumes generation result CSVs and executes the generated harness
crates per crate/backend. Execution results are kept separate from generation
metrics so failed backend runs do not overwrite the synthesis records.
"""

import argparse
import csv
import json
import os
import re
import signal
import shutil
import subprocess
import sys
import threading
import time
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path


REPO = Path(__file__).resolve().parents[1]
DEFAULT_GENERATION = REPO / "tmp/paper-experiments/runs/all_results.csv"
DEFAULT_OUT = REPO / "tmp/paper-experiments/backend-execution"
DEFAULT_KANI_CONFIG = REPO / "scripts/config/kani-ab-coverage.json"
DEFAULT_VARIES_KANI_REPO = "https://github.com/huanli-00/varies-kani.git"
DEFAULT_VARIES_KANI_DIR = Path(
    os.environ.get("VARIES_KANI_DIR", REPO / "tmp/paper-experiments/tools/varies-kani")
)
BACKENDS = ("tests", "fuzz", "kani")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--generation-results", type=Path, default=DEFAULT_GENERATION)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--experiment", default="unsafe-wrapper")
    parser.add_argument("--config", default="unsafe_wrapper_m4_d0")
    parser.add_argument("--backend", choices=BACKENDS)
    parser.add_argument("--crate", action="append", default=[], help="Crate name or crate-version key.")
    parser.add_argument("--timeout", type=int, default=1800)
    parser.add_argument("--test-timeout", type=int, default=300)
    parser.add_argument("--test-jobs", type=int, default=1, help="Number of generated tests to execute in parallel per crate.")
    parser.add_argument(
        "--max-tests-per-crate",
        type=int,
        default=0,
        help="Maximum generated tests to execute per crate; 0 means all generated tests.",
    )
    parser.add_argument(
        "--tests-runner",
        choices=("asan", "sanitizer", "native", "miri"),
        default="asan",
        help="Execution mode for generated tests. Defaults to ASan; native is plain execution.",
    )
    parser.add_argument(
        "--sanitizer",
        default="address",
        help="Sanitizer passed to -Zsanitizer when --tests-runner sanitizer is used; asan implies address.",
    )
    parser.add_argument("--sanitizer-target-triple", default="x86_64-unknown-linux-gnu")
    parser.add_argument("--jobs", type=int, default=1, help="Number of crates to execute in parallel.")
    parser.add_argument("--force", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--keep-target", action="store_true")
    parser.add_argument("--prepare-package-tmp", action="store_true", default=True)
    parser.add_argument("--no-prepare-package-tmp", dest="prepare_package_tmp", action="store_false")
    parser.add_argument("--env", action="append", default=[], help="Extra environment override, KEY=VALUE.")
    parser.add_argument("--fuzz-targets", choices=("first", "all"), default="first")
    parser.add_argument("--fuzz-jobs", type=int, default=1, help="Number of fuzz targets to execute in parallel per crate.")
    parser.add_argument("--fuzz-time-limit", type=int, default=24 * 60 * 60, help="Per-fuzz-target AFL -V time limit in seconds.")
    parser.add_argument("--fuzz-target-timeout", type=int, default=24 * 60 * 60 + 600, help="Outer timeout in seconds for each fuzz target command.")
    parser.add_argument("--fuzz-stop-on-crash", action="store_true", default=True, help="Stop scheduling new fuzz targets for a crate after the first crashing/failing target.")
    parser.add_argument("--no-fuzz-stop-on-crash", dest="fuzz_stop_on_crash", action="store_false")
    parser.add_argument(
        "--kani-mode",
        choices=("config", "codegen", "proof"),
        default="config",
        help="Kani execution mode. config uses --kani-config verify_cmd; codegen/proof use --kani-command.",
    )
    parser.add_argument(
        "--kani-config",
        type=Path,
        default=DEFAULT_KANI_CONFIG,
        help="JSON file containing verify_cmd for --kani-mode config.",
    )
    parser.add_argument(
        "--kani-command",
        default=None,
        help="Kani wrapper command for codegen/proof mode, or override for the first verify_cmd token.",
    )
    parser.add_argument("--varies-kani-repo", default=DEFAULT_VARIES_KANI_REPO)
    parser.add_argument("--varies-kani-dir", type=Path, default=DEFAULT_VARIES_KANI_DIR)
    parser.add_argument("--build-varies-kani", action="store_true")
    parser.add_argument("--prepare-varies-kani", action="store_true", default=True)
    parser.add_argument("--no-prepare-varies-kani", dest="prepare_varies_kani", action="store_false")
    parser.add_argument("--aggregate-existing", action="store_true")
    args = parser.parse_args()

    args.out = args.out.resolve()
    args.kani_config = args.kani_config.resolve()
    args.varies_kani_dir = args.varies_kani_dir.resolve()

    if args.aggregate_existing:
        rows = gather_existing_results(args.out)
        write_summaries(rows, args.out)
        return 0

    if args.backend is None:
        parser.error("--backend is required unless --aggregate-existing is used")
    if args.backend == "kani" and args.prepare_varies_kani and not args.dry_run:
        prepare_varies_kani(args)

    generation_rows = load_generation_rows(args)
    if not generation_rows:
        print("no matching generated harnesses", file=sys.stderr)
        return 2

    rows = []
    jobs = max(1, args.jobs)
    if jobs == 1:
        for completed, generation in enumerate(generation_rows, start=1):
            row = execute_one_generation(generation, args, completed, len(generation_rows))
            if row is not None:
                rows.append(row)
                if not args.dry_run:
                    write_summaries(gather_existing_results(args.out), args.out)
    else:
        with ThreadPoolExecutor(max_workers=jobs) as executor:
            futures = {
                executor.submit(run_backend, generation, args): (index, generation)
                for index, generation in enumerate(generation_rows, start=1)
            }
            for future in as_completed(futures):
                index, generation = futures[future]
                key = crate_key(generation)
                try:
                    row = future.result()
                except Exception as error:
                    print(f"[execute-error] {key}: {error}", file=sys.stderr, flush=True)
                    continue
                rows.append(row)
                print(
                    f"[{len(rows)}/{len(generation_rows)} done; slot {index}] executed {key} "
                    f"{generation['experiment']}/{generation['config']} backend={result_backend(args)}",
                    flush=True,
                )
                if not args.dry_run:
                    write_summaries(gather_existing_results(args.out), args.out)

    if rows and not args.dry_run:
        write_summaries(gather_existing_results(args.out), args.out)
    return 0


def execute_one_generation(generation, args, completed, total):
    key = crate_key(generation)
    print(
        f"[{completed}/{total}] execute {key} "
        f"{generation['experiment']}/{generation['config']} backend={result_backend(args)}",
        flush=True,
    )
    try:
        return run_backend(generation, args)
    except Exception as error:
        print(f"[execute-error] {key}: {error}", file=sys.stderr, flush=True)
        return None


def load_generation_rows(args):
    with args.generation_results.open(newline="", encoding="utf-8") as handle:
        rows = list(csv.DictReader(handle))
    requested = set(args.crate)
    selected = []
    for row in rows:
        if row.get("experiment") != args.experiment:
            continue
        if row.get("config") != args.config:
            continue
        if row.get("backend") != args.backend:
            continue
        if row.get("status") != "ok" or row.get("generation_status") != "generated":
            continue
        if requested and row.get("crate") not in requested and crate_key(row) not in requested:
            continue
        selected.append(row)
    selected.sort(key=lambda row: (int(row.get("csv_index") or 0), crate_key(row)))
    return selected


def run_backend(generation, args):
    key = crate_key(generation)
    run_dir = args.out / generation["experiment"] / generation["config"] / result_backend(args) / key
    result_path = run_dir / "result.json"
    if result_path.exists() and not args.force:
        return json.loads(result_path.read_text())

    if args.force and run_dir.exists():
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)

    harness = Path(generation["harness"])
    manifest = harness / "Cargo.toml"
    if not manifest.exists():
        raise RuntimeError(f"missing generated harness manifest: {manifest}")

    if args.prepare_package_tmp and not args.dry_run:
        prepare_package_tmp_dirs(manifest)

    env = os.environ.copy()
    apply_default_dynamic_library_paths(env)
    env.update(parse_env_overrides(args.env))
    if args.backend == "tests" and is_sanitizer_runner(args):
        sanitizer = selected_sanitizer(args)
        env["RUSTFLAGS"] = append_space_flag(env.get("RUSTFLAGS"), f"-Zsanitizer={sanitizer}")
        apply_sanitizer_env(env, sanitizer)
    if args.backend == "tests" and args.tests_runner == "miri":
        env["MIRIFLAGS"] = append_space_flag(env.get("MIRIFLAGS"), "-Zmiri-ignore-leaks")
    if args.backend == "fuzz":
        env["AFL_EXIT_WHEN_DONE"] = "1"
        env["AFL_NO_AFFINITY"] = "1"
    if args.backend == "kani":
        apply_varies_kani_env(args, env)
    target_dir = run_dir / "target"
    env["CARGO_TARGET_DIR"] = str(target_dir)

    if args.backend == "fuzz":
        if args.dry_run:
            for target in fuzz_targets(harness, args.fuzz_targets):
                print(" ".join(afl_build_command(manifest, target)))
                print(
                    " ".join(
                        fuzz_target_command(
                            target,
                            args,
                            harness,
                            run_dir / "targets" / target / "afl-out",
                            target_dir,
                        )
                    )
                )
            return {
                **base_result(generation, run_dir, [], result_backend(args)),
                "status": "dry-run",
                "exit_code": 0,
            }
        result = run_fuzz_backend(generation, args, run_dir, harness, manifest, env)
        if not args.keep_target:
            shutil.rmtree(target_dir, ignore_errors=True)
        return result

    command = backend_command(args, manifest)
    stdout_path = run_dir / "stdout.log"
    stderr_path = run_dir / "stderr.log"

    if args.dry_run:
        print(" ".join(command))
        return {
            **base_result(generation, run_dir, command, result_backend(args)),
            "status": "dry-run",
            "exit_code": 0,
        }

    if args.backend == "tests":
        result = run_tests_backend(generation, args, run_dir, harness, env, command)
        if not args.keep_target:
            shutil.rmtree(target_dir, ignore_errors=True)
        return result

    started = time.monotonic()
    status = "ok"
    exit_code = 0
    try:
        with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
            completed = run_command(
                command,
                cwd=harness,
                env=env,
                stdout=stdout,
                stderr=stderr,
                timeout=args.timeout,
            )
        exit_code = completed.returncode
        if exit_code != 0:
            status = "fail"
    except subprocess.TimeoutExpired:
        status = "timeout"
        exit_code = -999

    elapsed_sec = round(time.monotonic() - started, 3)
    result = {
        **base_result(generation, run_dir, command, result_backend(args)),
        "status": status,
        "exit_code": exit_code,
        "elapsed_sec": elapsed_sec,
        "timeout_sec": args.timeout,
        "stdout_tail": tail(stdout_path),
        "stderr_tail": tail(stderr_path),
        "first_error": first_error(stderr_path) or first_error(stdout_path),
    }
    result.update(
        execution_metrics(args.backend, stdout_path, stderr_path, generation, args)
    )
    result_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")

    if not args.keep_target:
        shutil.rmtree(target_dir, ignore_errors=True)
    return result


def base_result(generation, run_dir, command, backend_name):
    return {
        "crate": generation.get("crate"),
        "version": generation.get("version"),
        "experiment": generation.get("experiment"),
        "config": generation.get("config"),
        "generation_backend": generation.get("backend"),
        "backend": backend_name,
        "generation_harness": generation.get("harness"),
        "generation_sequence_count": int(float(generation.get("sequence_count") or 0)),
        "generation_target_count": int(float(generation.get("covered_target_count") or 0)),
        "generation_unsafe_wrapper_count": int(float(generation.get("covered_unsafe_wrapper_count") or 0)),
        "run_dir": str(run_dir),
        "command": command,
    }


def backend_command(args, manifest):
    if args.backend == "tests":
        if args.tests_runner == "miri":
            return [
                "cargo",
                "miri",
                "test",
                "--manifest-path",
                str(manifest),
                "--locked",
            ]
        return [
            "cargo",
            "test",
            *(
                ["-Zbuild-std"]
                if sanitizer_needs_build_std(selected_sanitizer(args))
                else []
            ),
            "--manifest-path",
            str(manifest),
            "--locked",
            *(
                ["--target", args.sanitizer_target_triple]
                if is_sanitizer_runner(args)
                else []
            ),
            "--no-run",
            "--message-format",
            "json",
        ]
    if args.backend == "fuzz":
        targets = fuzz_targets(manifest.parent, args.fuzz_targets)
        return fuzz_target_command(
            targets[0],
            args,
            manifest.parent,
            manifest.parent / "afl-out",
            manifest.parent / "target",
        )
    if args.backend == "kani":
        if args.kani_mode == "config":
            command = configured_kani_command(args)
        else:
            command = [args.kani_command or "cargo-varies-kani"]
            if args.kani_mode == "codegen":
                command.append("--only-codegen")
        if "--manifest-path" not in command and not any(
            arg.startswith("--manifest-path=") for arg in command
        ):
            command.extend(["--manifest-path", str(manifest)])
        return command
    raise AssertionError(args.backend)


def result_backend(args):
    if args.backend == "tests" and is_sanitizer_runner(args):
        sanitizer = selected_sanitizer(args)
        return "tests-asan" if sanitizer == "address" else f"tests-{sanitizer}"
    if args.backend == "tests" and args.tests_runner == "miri":
        return "tests-miri"
    return args.backend


def command_backend_name(command):
    if len(command) >= 3 and command[:3] == ["cargo", "miri", "test"]:
        return "tests-miri"
    if len(command) >= 2 and command[:2] == ["cargo", "test"]:
        return "tests"
    if len(command) >= 2 and command[:2] == ["cargo", "varies-kani"]:
        return "kani"
    if command and Path(command[0]).name == "cargo-varies-kani":
        return "kani"
    if len(command) >= 3 and command[:3] == ["cargo", "afl", "fuzz"]:
        return "fuzz"
    return "unknown"


def configured_kani_command(args):
    config = read_kani_config(args.kani_config)
    command = list(config.get("verify_cmd") or [])
    if not command:
        raise RuntimeError(f"{args.kani_config} does not define a non-empty verify_cmd")
    if args.kani_command:
        command[0] = args.kani_command
    return command


def read_kani_config(path):
    try:
        with path.open(encoding="utf-8") as handle:
            config = json.load(handle)
    except FileNotFoundError as error:
        raise RuntimeError(f"missing Kani config: {path}") from error
    if not isinstance(config, dict):
        raise RuntimeError(f"Kani config must be a JSON object: {path}")
    return config


def prepare_varies_kani(args):
    source = args.varies_kani_dir
    if not source.exists():
        repo = configured_varies_kani_repo(args)
        source.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            ["git", "clone", "--recurse-submodules", repo, str(source)],
            check=True,
        )
    wrapper = source / "scripts" / "cargo-varies-kani"
    if not wrapper.exists():
        raise RuntimeError(f"missing varies_kani wrapper: {wrapper}")
    driver = source / "target" / "kani" / "bin" / "kani-driver"
    if args.build_varies_kani:
        subprocess.run(["cargo", "build-dev", "--", "--release"], cwd=source, check=True)
    elif not driver.exists():
        raise RuntimeError(
            f"missing varies-kani driver: {driver}. "
            "Run with --build-varies-kani or build varies_kani before the experiment."
        )


def configured_varies_kani_repo(args):
    if args.kani_config.exists():
        config = read_kani_config(args.kani_config)
        varies_kani = config.get("varies_kani") or {}
        repo = varies_kani.get("repo")
        if repo:
            return repo
    return args.varies_kani_repo


def apply_varies_kani_env(args, env):
    if not args.prepare_varies_kani:
        return
    scripts_dir = args.varies_kani_dir / "scripts"
    env["PATH"] = prepend_path(env.get("PATH", ""), [scripts_dir])


def prepend_path(existing, paths):
    prefix = [str(path) for path in paths if path.exists()]
    if not prefix:
        return existing
    if not existing:
        return os.pathsep.join(prefix)
    return os.pathsep.join([*prefix, existing])


def fuzz_targets(harness, mode):
    targets = sorted(path.stem for path in (harness / "fuzz_targets").glob("*.rs"))
    if not targets:
        raise RuntimeError(f"no fuzz targets found under {harness / 'fuzz_targets'}")
    if mode == "first":
        return targets[:1]
    return targets


def fuzz_input_dir(harness, target):
    return harness / "corpus" / target


def afl_build_command(manifest, target):
    return [
        "cargo",
        "afl",
        "build",
        "--manifest-path",
        str(manifest),
        "--locked",
        "--bin",
        target,
    ]


def fuzz_target_command(target, args, harness, output_dir, target_dir):
    binary = target_dir / "debug" / target
    return [
        "cargo",
        "afl",
        "fuzz",
        "-i",
        str(fuzz_input_dir(harness, target)),
        "-o",
        str(output_dir),
        "-V",
        str(args.fuzz_time_limit),
        "--",
        str(binary),
    ]


def run_fuzz_backend(generation, args, run_dir, harness, manifest, env):
    started = time.monotonic()
    targets = fuzz_targets(harness, args.fuzz_targets)
    run_targets = []
    for target in targets:
        output_dir = run_dir / "targets" / target / "afl-out"
        command = fuzz_target_command(target, args, harness, output_dir, run_dir / "target")
        run_targets.append(
            {
                "name": target,
                "build_command": afl_build_command(manifest, target),
                "command": command,
                "afl_output_dir": str(output_dir),
                "stdout_log": str(run_dir / "targets" / target / "stdout.log"),
                "stderr_log": str(run_dir / "targets" / target / "stderr.log"),
            }
        )

    commands_path = run_dir / "commands.sh"
    commands_path.write_text(
        "\n".join(
            [
                "#!/usr/bin/env bash",
                "set -euo pipefail",
                "export AFL_EXIT_WHEN_DONE=1",
                "export AFL_NO_AFFINITY=1",
                *(
                    command
                    for target in run_targets
                    for command in (
                        " ".join(target["build_command"]),
                        " ".join(target["command"]),
                    )
                ),
                "",
            ]
        ),
        encoding="utf-8",
    )
    commands_path.chmod(0o755)

    if args.fuzz_jobs <= 1 or len(run_targets) <= 1:
        target_results = run_fuzz_targets_serial(run_targets, harness, env, args, run_dir)
    else:
        target_results = run_fuzz_targets_parallel(run_targets, harness, env, args, run_dir)

    status = fuzz_group_status(target_results)
    result = {
        **base_result(generation, run_dir, [str(commands_path)], result_backend(args)),
        "status": status,
        "exit_code": fuzz_group_exit_code(target_results),
        "elapsed_sec": round(time.monotonic() - started, 3),
        "timeout_sec": args.timeout,
        "fuzz_target_timeout_sec": args.fuzz_target_timeout,
        "fuzz_time_limit_sec": args.fuzz_time_limit,
        "fuzz_jobs": args.fuzz_jobs,
        "fuzz_stop_on_crash": args.fuzz_stop_on_crash,
        "fuzz_targets_mode": args.fuzz_targets,
        "afl_exit_when_done": "1",
        "afl_no_affinity": "1",
        "fuzz_targets_available": len(run_targets),
        "fuzz_targets_executed": sum(1 for row in target_results if row["status"] != "skipped"),
        "fuzz_targets_ok": sum(1 for row in target_results if row["status"] == "ok"),
        "fuzz_targets_fail": sum(1 for row in target_results if row["status"] == "fail"),
        "fuzz_targets_timeout": sum(1 for row in target_results if row["status"] == "timeout"),
        "fuzz_targets_skipped": sum(1 for row in target_results if row["status"] == "skipped"),
        "fuzz_crashes": sum(1 for row in target_results if row.get("crash_detected")),
        "fuzz_execs_done": sum(int(row.get("fuzz_execs_done") or 0) for row in target_results),
        "fuzz_target_results_path": str(run_dir / "fuzz_target_results.json"),
        "fuzz_target_results": target_results,
        "first_error": first_fuzz_error(target_results),
        "stdout_tail": "",
        "stderr_tail": "",
    }
    (run_dir / "fuzz_target_results.json").write_text(
        json.dumps(target_results, indent=2, sort_keys=True) + "\n"
    )
    (run_dir / "result.json").write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return result


def run_fuzz_targets_serial(run_targets, harness, env, args, run_dir):
    results = []
    stop = False
    for target in run_targets:
        if stop:
            results.append(skipped_fuzz_result(target["name"]))
            continue
        result = run_one_fuzz_target(target, harness, env, args, run_dir)
        results.append(result)
        if args.fuzz_stop_on_crash and result["status"] in ("fail", "timeout"):
            stop = True
    return results


def run_fuzz_targets_parallel(run_targets, harness, env, args, run_dir):
    stop_event = threading.Event()
    results = [None] * len(run_targets)
    with ThreadPoolExecutor(max_workers=max(1, args.fuzz_jobs)) as executor:
        futures = {
            executor.submit(
                run_one_fuzz_target_guarded,
                run_targets[index],
                harness,
                env,
                args,
                run_dir,
                stop_event,
            ): index
            for index in range(len(run_targets))
        }
        for future in as_completed(futures):
            index = futures[future]
            result = future.result()
            results[index] = result
            if args.fuzz_stop_on_crash and result["status"] in ("fail", "timeout"):
                stop_event.set()
    return [result if result is not None else skipped_fuzz_result(run_targets[index]["name"]) for index, result in enumerate(results)]


def run_one_fuzz_target_guarded(target, harness, env, args, run_dir, stop_event):
    if stop_event.is_set():
        return skipped_fuzz_result(target["name"])
    return run_one_fuzz_target(target, harness, env, args, run_dir)


def run_one_fuzz_target(target, harness, env, args, run_dir):
    target_name = target["name"]
    build_command = target["build_command"]
    command = target["command"]
    fuzz_input_dir(harness, target_name).mkdir(parents=True, exist_ok=True)
    afl_output_dir = Path(target["afl_output_dir"])
    target_dir = run_dir / "targets" / target_name
    target_dir.mkdir(parents=True, exist_ok=True)
    stdout_path = target_dir / "stdout.log"
    stderr_path = target_dir / "stderr.log"
    started = time.monotonic()
    status = "ok"
    build_exit_code = 0
    fuzz_exit_code = 0
    exit_code = 0
    fuzz_ran = False
    try:
        with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
            write_log(stdout, f"\n[paper-backend-execution] running {' '.join(build_command)}\n", None)
            completed = run_command(
                build_command,
                cwd=harness,
                env=env,
                stdout=stdout,
                stderr=stderr,
                timeout=args.timeout,
            )
            build_exit_code = completed.returncode
            if build_exit_code != 0:
                status = "fail"
                exit_code = build_exit_code
            else:
                write_log(stdout, f"\n[paper-backend-execution] running {' '.join(command)}\n", None)
                fuzz_ran = True
                completed = run_command(
                    command,
                    cwd=harness,
                    env=env,
                    stdout=stdout,
                    stderr=stderr,
                    timeout=args.fuzz_target_timeout,
                )
                fuzz_exit_code = completed.returncode
                exit_code = fuzz_exit_code
                if fuzz_exit_code != 0:
                    status = "fail"
    except subprocess.TimeoutExpired:
        status = "timeout"
        exit_code = -999

    stdout_tail = tail(stdout_path)
    stderr_tail = tail(stderr_path)
    combined = "\n".join([tail(stdout_path, 200000), tail(stderr_path, 200000)])
    crash_count = afl_crash_count(afl_output_dir)
    crash_detected = crash_count > 0 or (fuzz_ran and has_fuzz_crash(combined, exit_code))
    if crash_detected and status == "ok":
        status = "fail"
        exit_code = 1
    return {
        "name": target_name,
        "status": status,
        "exit_code": exit_code,
        "build_exit_code": build_exit_code,
        "fuzz_exit_code": fuzz_exit_code,
        "elapsed_sec": round(time.monotonic() - started, 3),
        "build_command": build_command,
        "command": command,
        "afl_output_dir": str(afl_output_dir),
        "stdout_log": str(stdout_path),
        "stderr_log": str(stderr_path),
        "stdout_tail": stdout_tail,
        "stderr_tail": stderr_tail,
        "first_error": first_error(stderr_path) or first_error(stdout_path),
        "fuzz_execs_done": read_afl_stat(afl_output_dir, "execs_done"),
        "fuzz_crash_count": crash_count,
        "crash_detected": crash_detected,
    }


def skipped_fuzz_result(name):
    return {
        "name": name,
        "status": "skipped",
        "exit_code": None,
        "elapsed_sec": 0.0,
        "build_command": [],
        "command": [],
        "afl_output_dir": "",
        "stdout_log": "",
        "stderr_log": "",
        "stdout_tail": "",
        "stderr_tail": "",
        "first_error": None,
        "fuzz_execs_done": 0,
        "fuzz_crash_count": 0,
        "crash_detected": False,
    }


def has_fuzz_crash(text, exit_code):
    if "Test unit written to" in text or "crashes saved as" in text or "Saved as" in text:
        return True
    if "ERROR: AddressSanitizer" in text or "runtime error:" in text:
        return True
    return exit_code not in (0, None, -999)


def afl_crash_count(output_dir):
    crash_dirs = [
        output_dir / "default" / "crashes",
        output_dir / "crashes",
    ]
    count = 0
    for crash_dir in crash_dirs:
        if not crash_dir.exists():
            continue
        count += sum(
            1
            for path in crash_dir.iterdir()
            if path.is_file() and path.name.startswith("id:")
        )
    return count


def read_afl_stat(output_dir, name):
    stat_paths = [
        output_dir / "default" / "fuzzer_stats",
        output_dir / "fuzzer_stats",
    ]
    for stat_path in stat_paths:
        if not stat_path.exists():
            continue
        for line in stat_path.read_text(encoding="utf-8", errors="replace").splitlines():
            key, sep, value = line.partition(":")
            if sep and key.strip() == name:
                try:
                    return int(value.strip())
                except ValueError:
                    return 0
    return 0


def fuzz_group_status(target_results):
    statuses = [row["status"] for row in target_results]
    if any(status == "timeout" for status in statuses):
        return "timeout"
    if any(status == "fail" for status in statuses):
        return "fail"
    if any(status == "ok" for status in statuses):
        return "ok"
    return "fail"


def fuzz_group_exit_code(target_results):
    if any(row["status"] == "timeout" for row in target_results):
        return -999
    for row in target_results:
        if row["status"] == "fail":
            return row["exit_code"] or 1
    return 0


def first_fuzz_error(target_results):
    for row in target_results:
        if row.get("first_error"):
            return row["first_error"]
        if row["status"] in ("fail", "timeout"):
            return f"{row['name']}: {row['status']}"
    return None


def run_tests_backend(generation, args, run_dir, harness, env, command):
    stdout_path = run_dir / "stdout.log"
    stderr_path = run_dir / "stderr.log"
    test_results_path = run_dir / "test_results.json"
    started = time.monotonic()
    compile_exit_code = 0
    status = "ok"
    test_results = []
    binary = None
    available_test_names = generated_test_names(harness)
    if args.max_tests_per_crate > 0:
        test_names = available_test_names[: args.max_tests_per_crate]
    else:
        test_names = available_test_names
    log_lock = threading.Lock()

    with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
        if args.tests_runner == "miri":
            compile_exit_code = 0
            test_log_dir = run_dir / "test_logs"
            test_results = run_generated_tests(
                test_names,
                args.test_jobs,
                lambda test_name: run_one_generated_test_with_miri(
                    command,
                    test_name,
                    harness,
                    env,
                    stdout,
                    stderr,
                    args.test_timeout,
                    test_log_dir,
                    log_lock,
                ),
            )
            status = first_non_ok_status(test_results) or status
        else:
            try:
                completed = run_command(
                    command,
                    cwd=harness,
                    env=env,
                    stdout=stdout,
                    stderr=stderr,
                    timeout=args.timeout,
                )
                compile_exit_code = completed.returncode
            except subprocess.TimeoutExpired:
                compile_exit_code = -999
                status = "timeout"

            if compile_exit_code == 0:
                binary = discover_test_binary(stdout_path, run_dir / "target")
                if binary is None:
                    status = "fail"
                    stderr.write(
                        b"\n[paper-backend-execution] failed to discover generated test binary\n"
                    )
                else:
                    test_log_dir = run_dir / "test_logs"
                    test_results = run_generated_tests(
                        test_names,
                        args.test_jobs,
                        lambda test_name: run_one_generated_test(
                            binary,
                            test_name,
                            harness,
                            env,
                            stdout,
                            stderr,
                            args.test_timeout,
                            test_log_dir,
                            log_lock,
                        ),
                    )
                    status = first_non_ok_status(test_results) or status
            elif status != "timeout":
                status = "fail"

    elapsed_sec = round(time.monotonic() - started, 3)
    test_results_path.write_text(json.dumps(test_results, indent=2, sort_keys=True) + "\n")
    result = {
        **base_result(generation, run_dir, command, result_backend(args)),
        "status": status,
        "exit_code": compile_exit_code if compile_exit_code != 0 else test_exit_code(test_results),
        "compile_exit_code": compile_exit_code,
        "elapsed_sec": elapsed_sec,
        "timeout_sec": args.timeout,
        "test_timeout_sec": args.test_timeout,
        "test_jobs": args.test_jobs,
        "max_tests_per_crate": args.max_tests_per_crate,
        "tests_runner": args.tests_runner,
        "sanitizer": selected_sanitizer(args) if is_sanitizer_runner(args) else None,
        "sanitizer_build_std": sanitizer_needs_build_std(selected_sanitizer(args))
        if is_sanitizer_runner(args)
        else False,
        "rustflags": env.get("RUSTFLAGS"),
        "asan_options": env.get("ASAN_OPTIONS") if selected_sanitizer(args) == "address" else None,
        "tsan_options": env.get("TSAN_OPTIONS") if selected_sanitizer(args) == "thread" else None,
        "miri_flags": env.get("MIRIFLAGS") if args.tests_runner == "miri" else None,
        "test_binary": str(binary) if binary else None,
        "test_result_path": str(test_results_path),
        "stdout_tail": tail(stdout_path),
        "stderr_tail": tail(stderr_path),
        "first_error": first_error(stderr_path) or first_error(stdout_path),
        "backend_output_count": int(float(generation.get("generated_test_module_count") or 0)),
        "available_generated_tests": len(available_test_names),
        "selected_generated_tests": len(test_names),
        "cargo_test_result_sections": len(test_results),
        "cargo_test_ok_sections": sum(1 for result in test_results if result["status"] == "ok"),
        "tests_passed": sum(1 for result in test_results if result["status"] == "ok"),
        "tests_failed": sum(1 for result in test_results if result["status"] == "fail"),
        "tests_timeout": sum(1 for result in test_results if result["status"] == "timeout"),
        "tests_ignored": 0,
        "tests_measured": 0,
        "tests_filtered_out": 0,
    }
    (run_dir / "result.json").write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return result


def run_generated_tests(test_names, test_jobs, run_one):
    if test_jobs <= 1 or len(test_names) <= 1:
        return [run_one(test_name) for test_name in test_names]

    results = [None] * len(test_names)
    with ThreadPoolExecutor(max_workers=test_jobs) as executor:
        futures = {
            executor.submit(run_one, test_name): index
            for index, test_name in enumerate(test_names)
        }
        for future in as_completed(futures):
            results[futures[future]] = future.result()
    return results


def first_non_ok_status(test_results):
    for result in test_results:
        if result["status"] != "ok":
            return result["status"]
    return None


def run_one_generated_test_with_miri(
    base_command, test_name, harness, env, stdout, stderr, timeout, log_dir, log_lock=None
):
    command = [*base_command, test_name, "--", "--exact", "--nocapture"]
    write_log(stdout, f"\n[paper-backend-execution] running {' '.join(command)}\n", log_lock)
    started = time.monotonic()
    status = "ok"
    exit_code, captured_stdout, captured_stderr, timed_out = run_command_capture(
        command,
        cwd=harness,
        env=env,
        timeout=timeout,
    )
    if timed_out:
        status = "timeout"
        write_log(
            stderr,
            f"\n[paper-backend-execution] miri timeout after {timeout}s: {test_name}\n",
            log_lock,
        )
    elif exit_code != 0:
        status = "fail"
        write_log(
            stderr,
            f"\n[paper-backend-execution] miri exit {exit_code}: {test_name}\n",
            log_lock,
        )
    return test_result_row(
        test_name,
        status,
        exit_code,
        round(time.monotonic() - started, 3),
        captured_stdout,
        captured_stderr,
        log_dir,
    )


def run_one_generated_test(binary, test_name, harness, env, stdout, stderr, timeout, log_dir, log_lock=None):
    command = [str(binary), test_name, "--exact", "--nocapture"]
    write_log(stdout, f"\n[paper-backend-execution] running {' '.join(command)}\n", log_lock)
    started = time.monotonic()
    status = "ok"
    exit_code, captured_stdout, captured_stderr, timed_out = run_command_capture(
        command,
        cwd=harness,
        env=env,
        timeout=timeout,
    )
    if timed_out:
        status = "timeout"
        write_log(
            stderr,
            f"\n[paper-backend-execution] timeout after {timeout}s: {test_name}\n",
            log_lock,
        )
    elif exit_code != 0:
        status = "fail"
        write_log(
            stderr,
            f"\n[paper-backend-execution] exit {exit_code}: {test_name}\n",
            log_lock,
        )
    return test_result_row(
        test_name,
        status,
        exit_code,
        round(time.monotonic() - started, 3),
        captured_stdout,
        captured_stderr,
        log_dir,
    )


def test_result_row(
    test_name, status, exit_code, elapsed_sec, captured_stdout, captured_stderr, log_dir
):
    stdout_text = decode_tail(captured_stdout)
    stderr_text = decode_tail(captured_stderr)
    row = {
        "name": test_name,
        "status": status,
        "exit_code": exit_code,
        "elapsed_sec": elapsed_sec,
        "stdout_tail": stdout_text,
        "stderr_tail": stderr_text,
        "first_error": first_error_text(stderr_text) or first_error_text(stdout_text),
        "unsoundness_kind": classify_unsoundness(stdout_text, stderr_text, status, exit_code),
    }
    if status != "ok":
        row.update(write_test_failure_logs(log_dir, test_name, captured_stdout, captured_stderr))
    return row


def write_log(handle, text, log_lock):
    data = text.encode()
    if log_lock is None:
        handle.write(data)
        handle.flush()
        return
    with log_lock:
        handle.write(data)
        handle.flush()


def run_command(command, cwd, env, stdout, stderr, timeout):
    if timeout is not None and timeout <= 0:
        timeout = None
    process = subprocess.Popen(
        command,
        cwd=cwd,
        env=env,
        stdout=stdout,
        stderr=stderr,
        start_new_session=True,
    )
    try:
        exit_code = process.wait(timeout=timeout)
        return subprocess.CompletedProcess(command, exit_code)
    except subprocess.TimeoutExpired:
        kill_process_group(process)
        raise


def run_command_capture(command, cwd, env, timeout):
    if timeout is not None and timeout <= 0:
        timeout = None
    process = subprocess.Popen(
        command,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    try:
        captured_stdout, captured_stderr = process.communicate(timeout=timeout)
        return process.returncode, captured_stdout, captured_stderr, False
    except subprocess.TimeoutExpired:
        kill_process_group(process)
        captured_stdout, captured_stderr = process.communicate()
        return -999, captured_stdout, captured_stderr, True


def kill_process_group(process):
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


def discover_test_binary(stdout_path, target_dir):
    for line in stdout_path.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("reason") != "compiler-artifact":
            continue
        if not message.get("executable"):
            continue
        target = message.get("target") or {}
        executable = Path(message["executable"])
        profile = message.get("profile") or {}
        if "test" in target.get("kind", []) or profile.get("test") or executable.name.startswith("varies_test-"):
            return executable

    deps = target_dir / "debug" / "deps"
    candidates = [
        path
        for path in deps.glob("varies_test-*")
        if path.is_file() and os.access(path, os.X_OK) and "." not in path.name
    ]
    candidates.extend(
        path
        for path in target_dir.glob("*/debug/deps/varies_test-*")
        if path.is_file() and os.access(path, os.X_OK) and "." not in path.name
    )
    return sorted(candidates)[-1] if candidates else None


def generated_test_names(harness):
    tests = []
    for path in sorted((harness / "src").glob("test_*.rs")):
        text = path.read_text(encoding="utf-8", errors="replace")
        for name in re.findall(r"fn (generated_test_\d+)\s*\(", text):
            tests.append(f"{path.stem}::{name}")
    return tests


def test_exit_code(test_results):
    if any(result["status"] == "timeout" for result in test_results):
        return -999
    if any(result["status"] == "fail" for result in test_results):
        return 1
    return 0


def prepare_package_tmp_dirs(manifest):
    output = subprocess.check_output(
        [
            "cargo",
            "metadata",
            "--manifest-path",
            str(manifest),
            "--locked",
            "--format-version",
            "1",
        ],
        cwd=manifest.parent,
    )
    metadata = json.loads(output)
    for package in metadata.get("packages", []):
        package_manifest = Path(package["manifest_path"])
        (package_manifest.parent / "target" / "tmp").mkdir(parents=True, exist_ok=True)


def execution_metrics(backend, stdout_path, stderr_path, generation, args):
    text = "\n".join(
        [
            tail(stdout_path, 200000),
            tail(stderr_path, 200000),
        ]
    )
    metrics = {
        "backend_output_count": int(float(generation.get("generated_fuzz_target_count") or 0))
        if backend == "fuzz"
        else int(float(generation.get("generated_test_module_count") or 0)),
    }
    if backend == "tests":
        metrics.update(parse_cargo_test_metrics(text))
    elif backend == "fuzz":
        metrics.update(
            {
                "fuzz_targets_executed": 1 if args.fuzz_targets == "first" else 0,
                "fuzz_execs_done": parse_afl_execs_done(text),
            }
        )
    elif backend == "kani":
        metrics.update(
            {
                "kani_mode": args.kani_mode,
                "kani_unsupported_constructs": parse_kani_unsupported_constructs(text),
            }
        )
    return metrics


def parse_cargo_test_metrics(text):
    results = re.findall(
        r"test result: (?P<status>\w+)\. (?P<passed>\d+) passed; (?P<failed>\d+) failed; "
        r"(?P<ignored>\d+) ignored; (?P<measured>\d+) measured; (?P<filtered>\d+) filtered out",
        text,
    )
    passed = failed = ignored = measured = filtered = 0
    ok_results = 0
    for status, p, f, i, m, flt in results:
        passed += int(p)
        failed += int(f)
        ignored += int(i)
        measured += int(m)
        filtered += int(flt)
        ok_results += int(status == "ok")
    return {
        "cargo_test_result_sections": len(results),
        "cargo_test_ok_sections": ok_results,
        "tests_passed": passed,
        "tests_failed": failed,
        "tests_ignored": ignored,
        "tests_measured": measured,
        "tests_filtered_out": filtered,
    }


def parse_afl_execs_done(text):
    match = re.search(r"execs_done\s*:\s*(?P<execs>\d+)", text)
    if not match:
        return 0
    return int(match.group("execs"))


def parse_kani_unsupported_constructs(text):
    return sorted(set(re.findall(r"- ([a-zA-Z0-9_ ]+) \(\d+\)", text)))


def parse_env_overrides(values):
    overrides = {}
    for value in values:
        if "=" not in value:
            raise ValueError(f"--env expects KEY=VALUE, got {value!r}")
        key, env_value = value.split("=", 1)
        overrides[key] = env_value
    return overrides


def apply_default_dynamic_library_paths(env):
    default_paths = [Path("/opt/anaconda3/lib")]
    existing_paths = [path for path in env.get("LD_LIBRARY_PATH", "").split(":") if path]
    prepend = [str(path) for path in default_paths if path.is_dir() and str(path) not in existing_paths]
    if prepend:
        env["LD_LIBRARY_PATH"] = ":".join([*prepend, *existing_paths])


def is_sanitizer_runner(args):
    return args.tests_runner in ("asan", "sanitizer")


def selected_sanitizer(args):
    if args.tests_runner == "asan":
        return "address"
    return args.sanitizer


def sanitizer_needs_build_std(sanitizer):
    return sanitizer in {"thread", "memory"}


def apply_sanitizer_env(env, sanitizer):
    if sanitizer == "address":
        env["ASAN_OPTIONS"] = merge_colon_options(
            env.get("ASAN_OPTIONS"),
            {
                "abort_on_error": "1",
                "detect_leaks": "0",
            },
        )
    elif sanitizer == "thread":
        env["TSAN_OPTIONS"] = merge_colon_options(
            env.get("TSAN_OPTIONS"),
            {
                "halt_on_error": "1",
            },
        )


def append_space_flag(existing, flag):
    if not existing:
        return flag
    parts = existing.split()
    if flag in parts:
        return existing
    return f"{existing} {flag}"


def merge_colon_options(existing, overrides):
    options = {}
    if existing:
        for part in existing.split(":"):
            if not part:
                continue
            key, _, value = part.partition("=")
            options[key] = value
    options.update(overrides)
    return ":".join(f"{key}={value}" for key, value in options.items())


def crate_key(row):
    return f"{row['crate']}-{row['version']}"


def tail(path, limit=5000):
    if path is None or not path.exists():
        return ""
    with path.open("rb") as handle:
        handle.seek(0, os.SEEK_END)
        size = handle.tell()
        handle.seek(max(0, size - limit), os.SEEK_SET)
        return handle.read(limit).decode("utf-8", "replace")


def first_error(path, limit=5_000_000):
    if path is None or not path.exists():
        return None
    with path.open("rb") as handle:
        data = handle.read(limit)
    for line in data.decode("utf-8", "replace").splitlines():
        stripped = line.strip()
        if stripped.startswith("error:") or re.match(r"^error\[[A-Za-z0-9]+\]:", stripped):
            return stripped
    return None


def decode_tail(data, limit=5000):
    if not data:
        return ""
    return data[-limit:].decode("utf-8", "replace")


def first_error_text(text):
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("error:") or re.match(r"^error\[[A-Za-z0-9]+\]:", stripped):
            return stripped
    return None


def classify_unsoundness(stdout_text, stderr_text, status, exit_code):
    text = "\n".join([stdout_text or "", stderr_text or ""])
    lowered = text.lower()
    if status == "timeout":
        return "timeout"
    if "memorysanitizer" in text or "use-of-uninitialized-value" in lowered:
        return "memory_sanitizer"
    if "addresssanitizer" in text:
        return "address_sanitizer"
    if "threadsanitizer" in text or "data race" in lowered:
        return "thread_sanitizer"
    if "undefined behavior" in lowered or "miri has detected undefined behavior" in lowered:
        return "miri_undefined_behavior"
    if "is uninitialized" in lowered or "using uninitialized data" in lowered:
        return "uninitialized"
    if "out-of-bounds" in lowered or "out of bounds" in lowered:
        return "out_of_bounds"
    if "dangling" in lowered or "use-after-free" in lowered:
        return "use_after_free"
    if "misaligned" in lowered:
        return "misaligned_access"
    if exit_code and exit_code < 0:
        return "signal"
    if "panicked at" in lowered:
        return "panic"
    if status != "ok":
        return "runtime_failure"
    return None


def write_test_failure_logs(log_dir, test_name, captured_stdout, captured_stderr):
    log_dir.mkdir(parents=True, exist_ok=True)
    stem = safe_log_stem(test_name)
    stdout_path = log_dir / f"{stem}.stdout.log"
    stderr_path = log_dir / f"{stem}.stderr.log"
    stdout_path.write_bytes(captured_stdout or b"")
    stderr_path.write_bytes(captured_stderr or b"")
    return {
        "stdout_log": str(stdout_path),
        "stderr_log": str(stderr_path),
    }


def safe_log_stem(test_name):
    return re.sub(r"[^A-Za-z0-9_.-]+", "_", test_name)[:180]


def gather_existing_results(out_dir):
    rows = []
    for path in sorted(out_dir.glob("*/*/*/*/result.json")):
        try:
            rows.append(json.loads(path.read_text()))
        except (FileNotFoundError, json.JSONDecodeError):
            continue
    return rows


def write_summaries(rows, out_dir):
    out_dir.mkdir(parents=True, exist_ok=True)
    write_json(out_dir / "all_results.json", rows)
    write_csv(out_dir / "all_results.csv", rows)
    summary_rows = summarize(rows)
    write_csv(out_dir / "summary_by_backend.csv", summary_rows)
    write_markdown_summary(out_dir / "summary.md", summary_rows, rows)
    unsoundness_cases = gather_unsoundness_cases(rows)
    write_json(out_dir / "unsoundness_cases.json", unsoundness_cases)
    write_csv(out_dir / "unsoundness_cases.csv", unsoundness_cases)
    write_unsoundness_markdown(out_dir / "unsoundness_cases.md", unsoundness_cases)


def summarize(rows):
    groups = defaultdict(list)
    for row in rows:
        groups[(row.get("experiment"), row.get("config"), row.get("backend"))].append(row)
    summary = []
    for (experiment, config, backend), group in sorted(groups.items()):
        status_counts = defaultdict(int)
        for row in group:
            status_counts[row.get("status", "unknown")] += 1
        summary.append(
            {
                "experiment": experiment,
                "config": config,
                "backend": backend,
                "runs": len(group),
                "ok": status_counts["ok"],
                "fail": status_counts["fail"],
                "timeout": status_counts["timeout"],
                "median_elapsed_sec": median(row.get("elapsed_sec") for row in group),
                "total_generation_sequences": sum(int(row.get("generation_sequence_count") or 0) for row in group),
                "tests_passed": sum(int(row.get("tests_passed") or 0) for row in group),
                "tests_failed": sum(int(row.get("tests_failed") or 0) for row in group),
                "tests_timeout": sum(int(row.get("tests_timeout") or 0) for row in group),
                "fuzz_targets_available": sum(int(row.get("fuzz_targets_available") or 0) for row in group),
                "fuzz_targets_executed": sum(int(row.get("fuzz_targets_executed") or 0) for row in group),
                "fuzz_targets_ok": sum(int(row.get("fuzz_targets_ok") or 0) for row in group),
                "fuzz_targets_fail": sum(int(row.get("fuzz_targets_fail") or 0) for row in group),
                "fuzz_targets_timeout": sum(int(row.get("fuzz_targets_timeout") or 0) for row in group),
                "fuzz_targets_skipped": sum(int(row.get("fuzz_targets_skipped") or 0) for row in group),
                "fuzz_crashes": sum(int(row.get("fuzz_crashes") or 0) for row in group),
                "fuzz_execs_done": sum(int(row.get("fuzz_execs_done") or 0) for row in group),
            }
        )
    return summary


def median(values):
    values = sorted(float(value) for value in values if value is not None)
    if not values:
        return None
    midpoint = len(values) // 2
    if len(values) % 2:
        return round(values[midpoint], 3)
    return round((values[midpoint - 1] + values[midpoint]) / 2.0, 3)


def write_json(path, rows):
    path.write_text(json.dumps(rows, indent=2, sort_keys=True) + "\n")


def write_csv(path, rows):
    if not rows:
        path.write_text("")
        return
    fieldnames = sorted({key for row in rows for key in row})
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=fieldnames)
        writer.writeheader()
        for row in rows:
            writer.writerow(row)


def write_markdown_summary(path, rows, result_rows):
    lines = [
        "# Backend Execution Summary",
        "",
        "| experiment | config | backend | ok/fail/timeout | median sec | gen seq | tests passed/failed | fuzz ok/fail/timeout/skipped | fuzz crashes | AFL execs |",
        "|---|---|---|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for row in rows:
        lines.append(
            f"| {row['experiment']} | {row['config']} | {row['backend']} | "
            f"{row['ok']}/{row['fail']}/{row['timeout']} | {row['median_elapsed_sec']} | "
            f"{row['total_generation_sequences']} | {row['tests_passed']}/{row['tests_failed']} "
            f"(timeout {row['tests_timeout']}) | "
            f"{row['fuzz_targets_ok']}/{row['fuzz_targets_fail']}/{row['fuzz_targets_timeout']}/{row['fuzz_targets_skipped']} "
            f"(of {row['fuzz_targets_available']}) | "
            f"{row['fuzz_crashes']} | {row['fuzz_execs_done']} |"
        )
    issue_rows = [
        row
        for row in result_rows
        if row.get("status") != "ok"
        or int(row.get("tests_failed") or 0) > 0
        or int(row.get("tests_timeout") or 0) > 0
    ]
    if issue_rows:
        lines.extend(
            [
                "",
                "## Non-Ok Runs",
                "",
                "| crate | backend | status | tests passed/failed/timeout | elapsed sec | first error |",
                "|---|---|---:|---:|---:|---|",
            ]
        )
        for row in issue_rows:
            lines.append(
                f"| {row.get('crate')}-{row.get('version')} | {row.get('backend')} | "
                f"{row.get('status')} | {row.get('tests_passed', 0)}/"
                f"{row.get('tests_failed', 0)}/{row.get('tests_timeout', 0)} | "
                f"{row.get('elapsed_sec')} | {markdown_cell(row.get('first_error') or '')} |"
            )
    path.write_text("\n".join(lines) + "\n")


def gather_unsoundness_cases(rows):
    cases = []
    for row in rows:
        backend = row.get("backend") or ""
        if not backend.startswith("tests-"):
            continue
        test_result_path = row.get("test_result_path")
        if not test_result_path:
            continue
        path = Path(test_result_path)
        try:
            test_results = json.loads(path.read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            continue
        for test in test_results:
            if test.get("status") == "ok":
                continue
            stdout_tail = test.get("stdout_tail") or ""
            stderr_tail = test.get("stderr_tail") or ""
            kind = test.get("unsoundness_kind") or classify_unsoundness(
                stdout_tail,
                stderr_tail,
                test.get("status"),
                int(test.get("exit_code") or 0),
            )
            cases.append(
                {
                    "crate": row.get("crate"),
                    "version": row.get("version"),
                    "crate_key": f"{row.get('crate')}-{row.get('version')}",
                    "backend": backend,
                    "test": test.get("name"),
                    "status": test.get("status"),
                    "exit_code": test.get("exit_code"),
                    "elapsed_sec": test.get("elapsed_sec"),
                    "classification": kind,
                    "first_error": test.get("first_error")
                    or first_error_text(stderr_tail)
                    or first_error_text(stdout_tail),
                    "stderr_tail": stderr_tail[:1000],
                    "stdout_tail": stdout_tail[:1000],
                    "stdout_log": test.get("stdout_log"),
                    "stderr_log": test.get("stderr_log"),
                    "result_json": str(path.parent / "result.json"),
                    "test_results_json": str(path),
                }
            )
    cases.sort(key=lambda row: (row["crate_key"], row["backend"], row["test"] or ""))
    return cases


def write_unsoundness_markdown(path, cases):
    lines = ["# Unsoundness Cases", ""]
    if not cases:
        lines.append("No non-ok generated test executions were recorded.")
    else:
        lines.extend(
            [
                "| crate | backend | classification | status | test | first error |",
                "|---|---|---|---:|---|---|",
            ]
        )
        for case in cases:
            lines.append(
                f"| {case['crate_key']} | {case['backend']} | {case['classification']} | "
                f"{case['status']} | `{markdown_cell(case.get('test') or '')}` | "
                f"{markdown_cell(case.get('first_error') or case.get('stderr_tail') or case.get('stdout_tail') or '')} |"
            )
    path.write_text("\n".join(lines) + "\n")


def markdown_cell(value):
    return str(value).replace("|", "\\|").replace("\n", " ")[:240]


if __name__ == "__main__":
    raise SystemExit(main())
