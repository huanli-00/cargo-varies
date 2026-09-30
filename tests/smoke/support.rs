use serde::Deserialize;
use std::ffi::OsString;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

#[derive(Debug, Deserialize)]
pub(super) struct ApiMetadataView {
    pub(super) full_name: String,
    #[serde(default)]
    pub(super) instantiated_path: String,
    pub(super) index: usize,
    pub(super) api_safety: String,
    #[serde(default)]
    pub(super) concrete_args: Vec<String>,
    #[serde(default)]
    pub(super) is_mono: bool,
    #[serde(default)]
    pub(super) unsafe_wrappers: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct SequenceMetadataView {
    pub(super) target: usize,
    pub(super) is_basic: bool,
    pub(super) functions: Vec<usize>,
    #[serde(default)]
    pub(super) unsafe_wrappers: Vec<String>,
}

pub(super) struct HarnessRun {
    _temp: TempDir,
    pub(super) harness_dir: PathBuf,
    pub(super) target_dir: PathBuf,
    output: Output,
}

impl HarnessRun {
    pub(super) fn assert_success(&self) {
        assert!(
            self.output.status.success(),
            "cargo-varies exited with {}\nstdout:\n{}\nstderr:\n{}",
            self.output.status,
            String::from_utf8_lossy(&self.output.stdout),
            String::from_utf8_lossy(&self.output.stderr)
        );
    }

    pub(super) fn assert_failure(&self) {
        assert!(
            !self.output.status.success(),
            "expected cargo-varies to fail\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&self.output.stdout),
            String::from_utf8_lossy(&self.output.stderr)
        );
    }

    pub(super) fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).into_owned()
    }

    pub(super) fn manifest(&self) -> String {
        fs::read_to_string(self.harness_dir.join("Cargo.toml"))
            .expect("generated manifest should exist")
    }

    pub(super) fn lockfile(&self) -> String {
        fs::read_to_string(self.harness_dir.join("Cargo.lock"))
            .expect("generated harness lockfile should exist")
    }

    pub(super) fn cargo_config(&self) -> String {
        fs::read_to_string(self.harness_dir.join(".cargo").join("config.toml"))
            .expect("generated cargo config should exist")
    }

    pub(super) fn generated_modules(&self) -> Vec<String> {
        fs::read_dir(self.harness_dir.join("src"))
            .expect("generated src dir should exist")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
            .filter(|path| path.file_name().is_some_and(|name| name != "lib.rs"))
            .map(|path| fs::read_to_string(path).expect("generated sequence module should exist"))
            .collect()
    }

    pub(super) fn generated_fuzz_targets(&self) -> Vec<String> {
        fs::read_dir(self.harness_dir.join("fuzz_targets"))
            .expect("generated fuzz_targets dir should exist")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
            .map(|path| fs::read_to_string(path).expect("generated fuzz target should exist"))
            .collect()
    }

    pub(super) fn generated_fuzz_seed_lengths(&self) -> Vec<usize> {
        fs::read_dir(self.harness_dir.join("corpus"))
            .expect("generated corpus dir should exist")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path().join("seed"))
            .filter(|path| path.exists())
            .map(|path| {
                fs::read(path)
                    .expect("generated corpus seed should exist")
                    .len()
            })
            .collect()
    }

    pub(super) fn api_metadata(&self, crate_name: &str) -> Vec<ApiMetadataView> {
        read_json(
            self.harness_dir
                .join(format!("varies_meta/{}_api_functions.json", crate_name)),
        )
    }

    pub(super) fn sequence_metadata(&self, crate_name: &str) -> Vec<SequenceMetadataView> {
        read_json(
            self.harness_dir
                .join(format!("varies_meta/{}_api_sequences.json", crate_name)),
        )
    }
}

pub(super) fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

pub(super) fn fixture_manifest_path(name: &str) -> PathBuf {
    fixture_dir(name).join("Cargo.toml")
}

pub(super) fn run_varies_on_fixture(
    fixture_name: &str,
    harness_name: &str,
    check_harness: bool,
    extra_args: &[&str],
) -> HarnessRun {
    run_varies_on_fixture_with_env(fixture_name, harness_name, check_harness, extra_args, &[])
}

pub(super) fn run_varies_on_fixture_with_env(
    fixture_name: &str,
    harness_name: &str,
    check_harness: bool,
    extra_args: &[&str],
    extra_env: &[(&str, &str)],
) -> HarnessRun {
    let mut args = vec![
        OsString::from("--dir"),
        fixture_dir(fixture_name).into_os_string(),
    ];
    args.extend(extra_args.iter().map(OsString::from));
    run_cargo_varies(harness_name, check_harness, args, extra_env)
}

pub(super) fn run_varies_on_manifest(
    fixture_name: &str,
    harness_name: &str,
    check_harness: bool,
    extra_args: &[&str],
) -> HarnessRun {
    let mut args = vec![
        OsString::from("--manifest-path"),
        fixture_manifest_path(fixture_name).into_os_string(),
    ];
    args.extend(extra_args.iter().map(OsString::from));
    run_cargo_varies(harness_name, check_harness, args, &[])
}

pub(super) fn api_index(apis: &[ApiMetadataView], name: &str) -> usize {
    apis.iter()
        .find(|api| api.full_name == name)
        .unwrap_or_else(|| panic!("expected `{name}` in metadata"))
        .index
}

pub(super) fn read_json<T>(path: PathBuf) -> T
where
    T: for<'de> Deserialize<'de>,
    T: Debug,
{
    let raw = fs::read_to_string(&path).expect("json file should exist");
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()))
}

pub(super) fn run_varies_raw(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cargo-varies"))
        .args(args)
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("DYLD_LIBRARY_PATH")
        .output()
        .expect("cargo-varies should execute")
}

pub(super) fn kani_is_available() -> bool {
    Command::new("cargo")
        .arg("varies-kani")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub(super) fn assert_harness_compiles_with_kani(harness_dir: &Path, target_dir: &Path) {
    let output = Command::new("cargo")
        .arg("varies-kani")
        .arg("--manifest-path")
        .arg(harness_dir.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(target_dir.join("kani"))
        .arg("--only-codegen")
        .output()
        .expect("cargo varies-kani should execute");

    assert!(
        output.status.success(),
        "cargo varies-kani exited with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_cargo_varies(
    harness_name: &str,
    check_harness: bool,
    mut args: Vec<OsString>,
    extra_env: &[(&str, &str)],
) -> HarnessRun {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let harness_dir = temp.path().join(harness_name);
    let target_dir = temp.path().join("target");
    let passthrough_index = args.iter().position(|arg| arg == "--");
    let passthrough = passthrough_index.map(|index| args.split_off(index));

    let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-varies"));
    command.arg("varies");
    command.args(args);
    if check_harness {
        command.arg("--check-harness");
    }
    command
        .arg("--harness-crate")
        .arg(&harness_dir)
        .env("CARGO_TARGET_DIR", &target_dir)
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("DYLD_LIBRARY_PATH");
    for (key, value) in extra_env {
        command.env(key, value);
    }
    if let Some(passthrough) = passthrough {
        command.args(passthrough);
    }

    let output = command.output().expect("cargo-varies should execute");
    HarnessRun {
        _temp: temp,
        harness_dir,
        target_dir,
        output,
    }
}
