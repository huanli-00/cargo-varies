use super::support::{run_varies_on_fixture, run_varies_on_manifest, run_varies_raw};
use std::fs;

#[test]
fn cargo_varies_help_lists_common_flags() {
    let output = run_varies_raw(&["varies", "-h"]);
    assert!(
        output.status.success(),
        "expected `cargo varies -h` to succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cargo varies"));
    let core_index = stdout
        .find("Core Options:")
        .expect("expected core options heading in help output");
    let cargo_index = stdout
        .find("Cargo Target Options:")
        .expect("expected cargo target options heading in help output");
    assert!(
        core_index < cargo_index,
        "expected core options to appear before cargo target options in help output:\n{stdout}"
    );
    assert!(stdout.contains("-C, --dir <DIR>"));
    assert!(stdout.contains("-o, --out <PATH>"));
    assert!(stdout.contains("-b, --backend <BACKEND>"));
    assert!(stdout.contains("fuzz"));
    assert!(stdout.contains("tests"));
    assert!(stdout.contains("-c, --check-harness"));
    assert!(stdout.contains("--max-depth <CALLS>"));
    assert!(stdout.contains("0 means unlimited"));
    assert!(stdout.contains("--log-level <LOG_LEVEL>"));
}

#[test]
fn cargo_varies_accepts_unlimited_max_depth() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-lib",
        "unlimited-depth-harness",
        true,
        &["-d", "0"],
    );
    run.assert_success();

    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_lib");
    assert!(
        !sequence_metadata.is_empty(),
        "expected `-d 0` to keep unlimited synthesis enabled"
    );
}

#[test]
fn cargo_varies_generates_harness_and_metadata() {
    let run = run_varies_on_fixture("basic-lib", "harness", true, &[]);
    run.assert_success();

    let sequence_meta = run
        .harness_dir
        .join("varies_meta/basic_lib_api_sequences.json");
    let api_meta = run
        .harness_dir
        .join("varies_meta/basic_lib_api_functions.json");
    let harness_lib = run.harness_dir.join("src/lib.rs");

    assert!(
        sequence_meta.exists(),
        "missing {}",
        sequence_meta.display()
    );
    assert!(api_meta.exists(), "missing {}", api_meta.display());
    assert!(harness_lib.exists(), "missing {}", harness_lib.display());

    let api_metadata = run.api_metadata("basic_lib");
    let sequence_metadata = run.sequence_metadata("basic_lib");

    assert!(
        api_metadata.iter().all(|api| api.api_safety == "safe"),
        "expected basic-lib to expose only safe APIs"
    );
    assert!(
        sequence_metadata.is_empty(),
        "expected no wrapper-focused suites for a safe-only fixture"
    );
    assert!(
        fs::read_to_string(harness_lib)
            .expect("generated harness lib should exist")
            .contains("#![cfg(kani)]"),
        "expected generated harness crate root to match original Kani-style gating"
    );

    let manifest = run.manifest();
    assert!(manifest.contains("publish = false"));
    assert!(manifest.contains("[lib]\npath = \"src/lib.rs\""));

    let harness_lock = run.lockfile();
    assert!(
        harness_lock.contains("[[package]]\nname = \"basic-lib\"\nversion = \"0.1.0\""),
        "expected generated harness lockfile to include the path dependency package:\n{harness_lock}"
    );
    assert!(
        harness_lock.contains("[[package]]\nname = \"varies_test\"\nversion = \"0.1.0\"\ndependencies = [\n \"basic-lib\",\n]"),
        "expected generated harness lockfile to include the harness root package entry:\n{harness_lock}"
    );
}

#[test]
fn cargo_varies_accepts_manifest_path() {
    let run = run_varies_on_manifest("basic-lib", "manifest-harness", true, &[]);
    run.assert_success();

    assert!(
        run.sequence_metadata("basic_lib").is_empty(),
        "expected manifest-path flow to preserve wrapper-focused empty synthesis for safe-only crates"
    );
}

#[test]
fn cargo_varies_requires_package_for_ambiguous_workspace() {
    let run = run_varies_on_fixture("workspace", "workspace-harness", false, &[]);
    run.assert_failure();

    assert!(
        run.stderr().contains("rerun with `--package <name>`"),
        "expected an explicit package guidance message, got:\n{}",
        run.stderr()
    );
}

#[test]
fn cargo_varies_supports_workspace_package_selection() {
    let run = run_varies_on_fixture(
        "workspace",
        "workspace-harness",
        true,
        &["--package", "target-lib"],
    );
    run.assert_success();

    let stderr = run.stderr();
    assert!(
        stderr.contains("generated")
            && stderr.contains("target_lib")
            && stderr.contains(&run.harness_dir.display().to_string()),
        "expected generation summary in stderr, got:\n{stderr}"
    );

    assert!(
        run.sequence_metadata("target_lib").is_empty(),
        "expected workspace package selection to succeed even when the selected crate has no unsafe wrapper targets"
    );

    let harness_lock = run.lockfile();
    assert!(
        harness_lock.contains("[[package]]\nname = \"target-lib\"\nversion = \"0.1.0\""),
        "expected generated harness lockfile to include the selected workspace member:\n{harness_lock}"
    );
    assert!(
        !harness_lock.contains("name = \"helper-lib\""),
        "expected generated harness lockfile to omit unrelated workspace members:\n{harness_lock}"
    );
    assert!(
        harness_lock.contains("[[package]]\nname = \"varies_test\"\nversion = \"0.1.0\"\ndependencies = [\n \"target-lib\",\n]"),
        "expected generated harness lockfile to include a root package entry for the selected workspace member:\n{harness_lock}"
    );
}

#[test]
fn cargo_varies_propagates_feature_flags_to_generated_harness() {
    let run = run_varies_on_fixture(
        "feature-lib",
        "feature-harness",
        true,
        &["--features", "extra"],
    );
    run.assert_success();

    let manifest = run.manifest();
    assert!(
        manifest.contains("features = [\"extra\"]"),
        "expected generated harness manifest to enable the selected feature:\n{manifest}"
    );

    let api_metadata = run.api_metadata("feature_lib");
    assert!(
        api_metadata
            .iter()
            .any(|api| api.full_name == "FeaturedCounter::new"),
        "expected feature-gated API visibility to flow into metadata"
    );
}

#[test]
fn cargo_varies_absorbs_passthrough_feature_flags() {
    let run = run_varies_on_fixture(
        "feature-lib",
        "passthrough-feature-harness",
        true,
        &["--", "--features", "extra"],
    );
    run.assert_success();

    let manifest = run.manifest();
    assert!(
        manifest.contains("features = [\"extra\"]"),
        "expected passthrough features to be reflected in the generated harness manifest:\n{manifest}"
    );
}

#[test]
fn cargo_varies_handles_fallible_value_producers() {
    let run = run_varies_on_fixture("fallible-lib", "fallible-harness", true, &[]);
    run.assert_success();

    let sequence_metadata = run.sequence_metadata("fallible_lib");
    assert!(
        sequence_metadata.is_empty(),
        "expected no suites yet for fallible producers without unsafe wrapper targets"
    );
}

#[test]
fn cargo_varies_accepts_explicit_backend_flag() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-lib",
        "explicit-backend-harness",
        true,
        &["--backend", "kani"],
    );
    run.assert_success();

    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_lib");
    assert!(
        !sequence_metadata.is_empty(),
        "expected explicit backend selection to preserve wrapper-target synthesis"
    );
}

#[test]
fn cargo_varies_renders_multiple_backends_in_separate_harness_dirs() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-lib",
        "multi-backend-harness",
        false,
        &["--backend", "kani,tests", "--backend", "fuzz"],
    );
    run.assert_success();

    for backend in ["kani", "tests", "fuzz"] {
        let harness_dir = run.harness_dir.join(backend);
        assert!(
            harness_dir.join("Cargo.toml").exists(),
            "expected {backend} harness manifest at {}",
            harness_dir.display()
        );
        assert!(
            harness_dir
                .join("varies_meta/unsafe_wrapper_lib_api_sequences.json")
                .exists(),
            "expected {backend} harness metadata"
        );
    }
    assert!(run.harness_dir.join("kani/src/lib.rs").exists());
    assert!(
        fs::read_dir(run.harness_dir.join("tests/src"))
            .expect("tests src dir should exist")
            .any(|entry| entry
                .expect("tests src entry should exist")
                .file_name()
                .to_string_lossy()
                .starts_with("test_"))
    );
    assert!(
        fs::read_dir(run.harness_dir.join("fuzz/fuzz_targets"))
            .expect("fuzz target dir should exist")
            .next()
            .is_some()
    );
}
