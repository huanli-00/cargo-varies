use crate::backend::{BackendKind, validation_command};
use crate::cli::{
    VariesArgs, WRAPPER_MODE_ARG, absorb_feature_args, current_exe, exit_with_varies_help,
    exit_with_varies_version, first_mode_arg, parse_cargo_cli_from_env, parse_wrapper_cli_from_env,
    resolved_manifest_path, resolved_target_dir,
};
use crate::driver;
use crate::logging;
use crate::toolchain::pin_current_toolchain;
use crate::workspace::discover_target_with_package;
use anyhow::{Context, Result, bail};
use log::{debug, info};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use wait_timeout::ChildExt;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[cfg(unix)]
const SIGKILL: i32 = 9;

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

pub fn run_from_env() -> Result<()> {
    match first_mode_arg().as_deref() {
        Some("varies") => run_cargo_subcommand(),
        Some(WRAPPER_MODE_ARG) => run_wrapper_mode(),
        Some("-h") | Some("--help") | None => exit_with_varies_help(),
        Some("-V") | Some("--version") => exit_with_varies_version(),
        Some(mode) if mode.ends_with("rustc") => {
            bail!("direct rustc-wrapper invocation is unsupported; rerun via `cargo varies`")
        }
        Some(mode) => bail!("unexpected invocation mode `{mode}`"),
    }
}

fn run_cargo_subcommand() -> Result<()> {
    let (cli, cargo_args) = parse_cargo_cli_from_env()?;
    logging::init(cli.log_level);
    let (cli, cargo_args) = absorb_feature_args(cli, cargo_args)?;
    let target_dir = resolved_target_dir(&cli)?;
    debug!(target: "varies::cargo_phase", "resolved target dir {}", target_dir.display());
    let manifest_path = resolved_manifest_path(&cli)?;
    debug!(target: "varies::cargo_phase", "resolved manifest path {:?}", manifest_path);
    let analysis_target = discover_target_with_package(&target_dir, cli.package.as_deref())?;
    info!(
        target: "varies::cargo_phase",
        "planning synthesis for crate `{}` in {}",
        analysis_target.lib_crate_name,
        analysis_target.manifest_dir.display()
    );
    let current_exe = current_exe()?;
    let wrapper = WrapperLauncher::new(&current_exe, &cli, &target_dir)?;
    debug!(target: "varies::cargo_phase", "wrapper launcher: {}", wrapper.path().display());

    let mut command = Command::new("cargo");
    command.current_dir(&target_dir);
    command.arg("check");
    command.arg("--lib");
    if let Some(manifest_path) = &manifest_path {
        command.arg("--manifest-path");
        command.arg(manifest_path);
    }
    if let Some(package) = &cli.package {
        command.arg("--package");
        command.arg(package);
    }
    command.args(cli.cargo_feature_args());
    command.args(&cargo_args);
    command.env("RUSTC_WRAPPER", wrapper.path());
    pin_current_toolchain(&mut command)?;
    inject_rebuild_marker(&mut command)?;

    info!(target: "varies::cargo_phase", "running `cargo check --lib` with wrapper analysis");
    debug!(target: "varies::cargo_phase", "passthrough cargo args: {:?}", cargo_args);
    let status = wait_for_command(command, cli.timeout, "cargo check", &target_dir)?;
    exit_on_failure(status);

    info!(target: "varies::cargo_phase", "cargo check finished successfully");
    refresh_harness_lockfile(&target_dir, &cli)?;

    if cli.check_harness {
        run_harness_check(&target_dir, &cli)?;
    }

    Ok(())
}
fn run_wrapper_mode() -> Result<()> {
    let (cli, rustc_args) = parse_wrapper_cli_from_env()?;
    logging::init(cli.log_level);
    debug!(target: "varies::cargo_phase", "wrapper received {} rustc args", rustc_args.len());
    if should_analyze_primary_package(&rustc_args) {
        debug!(target: "varies::cargo_phase", "wrapper entered primary-package analysis mode");
        driver::run_wrapper(cli, rustc_args)
    } else {
        debug!(target: "varies::cargo_phase", "wrapper forwarding non-primary rustc invocation");
        run_plain_rustc(&rustc_args)
    }
}

fn should_analyze_primary_package(rustc_args: &[String]) -> bool {
    if env::var_os("CARGO_PRIMARY_PACKAGE").is_none() {
        return false;
    }

    if env::var("CARGO_CRATE_NAME")
        .ok()
        .as_deref()
        .is_some_and(|name| name == "build_script_build")
    {
        return false;
    }

    rustc_args
        .windows(2)
        .any(|window| window == ["--crate-type", "lib"] || window == ["--crate-type", "rlib"])
}

fn run_plain_rustc(rustc_args: &[String]) -> Result<()> {
    let (rustc, args) = rustc_args
        .split_first()
        .context("missing rustc path in wrapper invocation")?;

    let status = Command::new(rustc)
        .args(args)
        .status()
        .context("failed to invoke plain rustc from wrapper mode")?;

    exit_on_failure(status);

    Ok(())
}

struct WrapperLauncher {
    path: PathBuf,
}

impl WrapperLauncher {
    fn new(current_exe: &Path, cli: &VariesArgs, command_dir: &Path) -> Result<Self> {
        let directory = wrapper_launcher_dir(command_dir);
        fs::create_dir_all(&directory).with_context(|| {
            format!(
                "failed to create wrapper launcher dir {}",
                directory.display()
            )
        })?;

        let path = directory.join(wrapper_launcher_name()?);
        let script = render_wrapper_launcher(current_exe, cli)?;
        fs::write(&path, script)
            .with_context(|| format!("failed to write wrapper launcher {}", path.display()))?;
        make_wrapper_launcher_executable(&path)?;

        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WrapperLauncher {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn wrapper_launcher_dir(command_dir: &Path) -> PathBuf {
    match env::var_os("CARGO_TARGET_DIR").map(PathBuf::from) {
        Some(path) if path.is_absolute() => path.join("cargo-varies"),
        Some(path) => command_dir.join(path).join("cargo-varies"),
        None => command_dir.join("target").join("cargo-varies"),
    }
}

fn wrapper_launcher_name() -> Result<String> {
    #[cfg(windows)]
    let extension = "cmd";
    #[cfg(not(windows))]
    let extension = "sh";

    Ok(format!(
        "wrapper-{}-{}.{}",
        process::id(),
        invocation_id()?,
        extension
    ))
}

fn render_wrapper_launcher(current_exe: &Path, cli: &VariesArgs) -> Result<String> {
    let mut words = vec![
        current_exe.to_string_lossy().into_owned(),
        WRAPPER_MODE_ARG.to_owned(),
    ];
    words.extend(cli.flag_args());
    let joined = shlex::try_join(words.iter().map(String::as_str))
        .context("failed to render wrapper launcher command")?;

    #[cfg(windows)]
    {
        Ok(format!("@echo off\r\n{joined} -- %*\r\n"))
    }

    #[cfg(not(windows))]
    {
        Ok(format!("#!/bin/sh\nexec {joined} -- \"$@\"\n"))
    }
}

#[cfg(unix)]
fn make_wrapper_launcher_executable(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)
        .with_context(|| format!("failed to stat wrapper launcher {}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("failed to chmod wrapper launcher {}", path.display()))
}

#[cfg(not(unix))]
fn make_wrapper_launcher_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn run_harness_check(target_dir: &std::path::Path, cli: &VariesArgs) -> Result<()> {
    for backend in &cli.backends {
        let harness_dir = cli.backend_harness_dir(target_dir, *backend);
        let manifest = harness_dir.join("Cargo.toml");
        let Some(validation) = validation_command(*backend, &manifest) else {
            continue;
        };

        if *backend == BackendKind::Kani {
            prepare_kani_package_tmp_dirs(&manifest)?;
        }

        info!(target: "varies::cargo_phase", "validating generated harness with {}", validation.description);
        let mut command = Command::new(validation.program);
        command.current_dir(&harness_dir);
        command.args(&validation.args);
        pin_current_toolchain(&mut command)?;

        let status = wait_for_command(command, cli.timeout, validation.description, &harness_dir)
            .with_context(|| {
            format!(
                "failed to validate generated harness at {}",
                manifest.display()
            )
        })?;
        exit_on_failure(status);
    }
    Ok(())
}

#[derive(Deserialize)]
struct CargoMetadataOutput {
    packages: Vec<CargoMetadataPackage>,
}

#[derive(Deserialize)]
struct CargoMetadataPackage {
    manifest_path: PathBuf,
}

fn prepare_kani_package_tmp_dirs(manifest: &Path) -> Result<()> {
    let harness_dir = manifest.parent().with_context(|| {
        format!(
            "generated harness manifest has no parent: {}",
            manifest.display()
        )
    })?;
    let mut command = Command::new("cargo");
    command.current_dir(harness_dir);
    command.arg("metadata");
    command.arg("--manifest-path");
    command.arg(manifest);
    command.arg("--format-version");
    command.arg("1");
    command.arg("--locked");
    pin_current_toolchain(&mut command)?;

    let output = command
        .output()
        .with_context(|| format!("failed to invoke cargo metadata for {}", manifest.display()))?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed for generated Kani harness {}: {}",
            manifest.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let metadata: CargoMetadataOutput = serde_json::from_slice(&output.stdout)
        .context("failed to parse generated Kani harness metadata")?;
    for package in metadata.packages {
        let Some(package_dir) = package.manifest_path.parent() else {
            continue;
        };
        let tmp_dir = package_dir.join("target").join("tmp");
        fs::create_dir_all(&tmp_dir).with_context(|| {
            format!("failed to create Kani rustc tmp dir {}", tmp_dir.display())
        })?;
    }

    Ok(())
}

fn refresh_harness_lockfile(target_dir: &std::path::Path, cli: &VariesArgs) -> Result<()> {
    run_generated_harness_command(target_dir, cli, HarnessCommand::GenerateLockfile)
}

#[derive(Clone, Copy)]
enum HarnessCommand {
    GenerateLockfile,
}

impl HarnessCommand {
    fn description(self) -> &'static str {
        match self {
            HarnessCommand::GenerateLockfile => "generated harness cargo generate-lockfile",
        }
    }

    fn error_context(self, manifest: &std::path::Path) -> String {
        match self {
            HarnessCommand::GenerateLockfile => format!(
                "failed to refresh generated harness lockfile at {}",
                manifest.display()
            ),
        }
    }

    fn configure(self, command: &mut Command, manifest: &std::path::Path) {
        match self {
            HarnessCommand::GenerateLockfile => {
                command.arg("generate-lockfile");
                command.arg("--manifest-path");
                command.arg(manifest);
            }
        }
    }
}

fn run_generated_harness_command(
    target_dir: &std::path::Path,
    cli: &VariesArgs,
    action: HarnessCommand,
) -> Result<()> {
    for backend in &cli.backends {
        let harness_dir = cli.backend_harness_dir(target_dir, *backend);
        let manifest = harness_dir.join("Cargo.toml");
        let mut command = Command::new("cargo");
        command.current_dir(&harness_dir);
        action.configure(&mut command, &manifest);
        pin_current_toolchain(&mut command)?;

        let status = wait_for_command(command, cli.timeout, action.description(), &harness_dir)
            .with_context(|| action.error_context(&manifest))?;
        exit_on_failure(status);
    }

    Ok(())
}

fn exit_on_failure(status: std::process::ExitStatus) {
    if !status.success() {
        process::exit(status.code().unwrap_or(1));
    }
}

fn wait_for_command(
    mut command: Command,
    timeout: Option<u64>,
    description: &str,
    dir: &std::path::Path,
) -> Result<std::process::ExitStatus> {
    #[cfg(unix)]
    {
        command.process_group(0);
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch {description} in {}", dir.display()))?;

    if let Some(timeout) = timeout {
        let status = child
            .wait_timeout(Duration::from_secs(timeout))
            .with_context(|| format!("failed waiting on {description}"))?;
        if let Some(status) = status {
            return Ok(status);
        }

        terminate_timed_out_child(&mut child);
        let _ = child.wait();
        Err(anyhow::anyhow!("{description} timed out after {timeout}s"))
    } else {
        child
            .wait()
            .with_context(|| format!("failed waiting on {description}"))
    }
}

fn terminate_timed_out_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        let process_group = -(child.id() as i32);
        let _ = kill(process_group, SIGKILL);
    }

    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

fn inject_rebuild_marker(command: &mut Command) -> Result<()> {
    let marker = format!("varies_run_{}", invocation_id()?);

    if let Some(existing) = env::var_os("CARGO_ENCODED_RUSTFLAGS") {
        let mut encoded = existing
            .into_string()
            .map_err(|_| anyhow::anyhow!("CARGO_ENCODED_RUSTFLAGS contained non-utf8 data"))?;
        if !encoded.is_empty() {
            encoded.push('\u{1f}');
        }
        encoded.push_str("--cfg");
        encoded.push('\u{1f}');
        encoded.push_str(&marker);
        command.env("CARGO_ENCODED_RUSTFLAGS", encoded);
        return Ok(());
    }

    let mut rustflags = env::var("RUSTFLAGS").unwrap_or_default();
    if !rustflags.is_empty() {
        rustflags.push(' ');
    }
    rustflags.push_str("--cfg ");
    rustflags.push_str(&marker);
    command.env("RUSTFLAGS", rustflags);
    Ok(())
}

fn invocation_id() -> Result<u128> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_nanos())
}
