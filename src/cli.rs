use crate::backend::BackendKind;
use anyhow::{Context, Result, bail};
use clap::{ArgAction, Args, Parser, ValueEnum};
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const WRAPPER_MODE_ARG: &str = "__varies_wrapper";

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "cargo")]
#[command(bin_name = "cargo")]
enum CargoCli {
    #[command(
        about = "Synthesize Kani-oriented verification harnesses for a Rust library crate",
        after_help = "Examples:\n  cargo varies -C /path/to/crate\n  cargo varies -m /path/to/crate/Cargo.toml\n  cargo varies -C /path/to/workspace -p my-lib\n  cargo varies -C /path/to/crate -o /tmp/my-suite -c\n\nPass extra `cargo check` flags after `--`, for example:\n  cargo varies -C /path/to/crate -- --features extra"
    )]
    Varies(VariesArgs),
}

#[derive(Parser, Debug)]
struct FlagCli {
    #[command(flatten)]
    varies: VariesArgs,
}

#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct VariesArgs {
    /// Output directory for the generated harness crate.
    #[arg(
        short = 'o',
        long = "out",
        visible_alias = "harness-crate",
        value_name = "PATH",
        default_value = "varies_test",
        help_heading = "Core Options",
        display_order = 10
    )]
    pub harness_crate: PathBuf,
    /// Backend used to render and validate the generated suite. May be repeated
    /// or passed as a comma-separated list to render multiple backend crates.
    #[arg(
        short = 'b',
        long = "backend",
        value_enum,
        value_name = "BACKEND",
        value_delimiter = ',',
        action = ArgAction::Append,
        default_value = "kani",
        help_heading = "Core Options",
        display_order = 11
    )]
    pub backends: Vec<BackendKind>,
    /// Kill `cargo check` and follow-up validation after this many seconds.
    #[arg(
        short = 't',
        long,
        value_name = "SECONDS",
        help_heading = "Core Options",
        display_order = 12
    )]
    pub timeout: Option<u64>,
    /// Build-check the generated harness crate after synthesis.
    #[arg(
        short = 'c',
        long = "check-harness",
        visible_alias = "check",
        help_heading = "Core Options",
        display_order = 13
    )]
    pub check_harness: bool,
    /// Maximum call count in one synthesized sequence; 0 means unlimited.
    #[arg(
        short = 'd',
        long = "max-depth",
        value_name = "CALLS",
        default_value_t = 0,
        help_heading = "Core Options",
        display_order = 14
    )]
    pub max_depth: usize,
    /// Maximum extra mutator rounds to insert per target sequence.
    #[arg(
        short = 'M',
        long = "max-mutators",
        visible_alias = "max-mutators-per-target",
        default_value_t = 4,
        help_heading = "Core Options",
        display_order = 15
    )]
    pub max_mutators_per_target: usize,
    /// Application log verbosity for cargo-varies stages.
    #[arg(
        long = "log-level",
        value_enum,
        default_value_t = LogLevel::Info,
        help_heading = "Core Options",
        display_order = 16
    )]
    pub log_level: LogLevel,
    /// Crate dir. Defaults to the current directory.
    #[arg(
        short = 'C',
        long,
        value_name = "DIR",
        conflicts_with = "manifest_path",
        help_heading = "Cargo Target Options",
        display_order = 100
    )]
    pub dir: Option<PathBuf>,
    /// Manifest path.
    #[arg(
        short = 'm',
        long,
        value_name = "PATH",
        conflicts_with = "dir",
        help_heading = "Cargo Target Options",
        display_order = 101
    )]
    pub manifest_path: Option<PathBuf>,
    /// Workspace member package.
    #[arg(
        short = 'p',
        long,
        value_name = "SPEC",
        help_heading = "Cargo Target Options",
        display_order = 102
    )]
    pub package: Option<String>,
    /// Enable Cargo features.
    #[arg(
        short = 'F',
        long,
        value_name = "FEATURES",
        value_delimiter = ',',
        action = ArgAction::Append,
        help_heading = "Cargo Target Options",
        display_order = 103
    )]
    pub features: Vec<String>,
    /// Enable all Cargo features.
    #[arg(long, help_heading = "Cargo Target Options", display_order = 104)]
    pub all_features: bool,
    /// Disable default Cargo features.
    #[arg(long, help_heading = "Cargo Target Options", display_order = 105)]
    pub no_default_features: bool,
}

impl VariesArgs {
    fn parse_flag_args<I, T>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        FlagCli::try_parse_from(args)
            .map(|cli| {
                let mut varies = cli.varies;
                varies.normalize_backend_selection();
                varies
            })
            .map_err(Into::into)
    }

    pub fn cargo_feature_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if !self.features.is_empty() {
            args.push("--features".to_owned());
            args.push(self.features.join(","));
        }
        if self.all_features {
            args.push("--all-features".to_owned());
        }
        if self.no_default_features {
            args.push("--no-default-features".to_owned());
        }
        args
    }

    pub fn resolve_harness_dir(&self, base_dir: &Path) -> PathBuf {
        if self.harness_crate.is_absolute() {
            self.harness_crate.clone()
        } else {
            base_dir.join(&self.harness_crate)
        }
    }

    pub fn backend_harness_dir(&self, base_dir: &Path, backend: BackendKind) -> PathBuf {
        let harness_dir = self.resolve_harness_dir(base_dir);
        if self.backends.len() == 1 {
            harness_dir
        } else {
            harness_dir.join(backend.as_str())
        }
    }

    pub fn backend_list_label(&self) -> String {
        self.backends
            .iter()
            .map(|backend| backend.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn flag_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(dir) = &self.dir {
            args.push("--dir".to_owned());
            args.push(dir.display().to_string());
        }
        if let Some(manifest_path) = &self.manifest_path {
            args.push("--manifest-path".to_owned());
            args.push(manifest_path.display().to_string());
        }
        if let Some(package) = &self.package {
            args.push("--package".to_owned());
            args.push(package.to_owned());
        }
        args.extend(self.cargo_feature_args());
        args.push("--out".to_owned());
        args.push(self.harness_crate.display().to_string());
        for backend in &self.backends {
            args.push("--backend".to_owned());
            args.push(backend.as_str().to_owned());
        }
        if let Some(timeout) = self.timeout {
            args.push("--timeout".to_owned());
            args.push(timeout.to_string());
        }
        if self.check_harness {
            args.push("--check-harness".to_owned());
        }
        args.push("--max-depth".to_owned());
        args.push(self.max_depth.to_string());
        args.push("--max-mutators".to_owned());
        args.push(self.max_mutators_per_target.to_string());
        args.push("--log-level".to_owned());
        args.push(self.log_level.as_str().to_owned());
        args
    }
}

pub fn exit_with_varies_help() -> ! {
    let _ = CargoCli::parse_from(["cargo", "varies", "--help"]);
    unreachable!("clap exits after printing help")
}

pub fn exit_with_varies_version() -> ! {
    let _ = CargoCli::parse_from(["cargo", "varies", "--version"]);
    unreachable!("clap exits after printing version")
}

pub fn parse_cargo_cli_from_env() -> Result<(VariesArgs, Vec<String>)> {
    let args: Vec<String> = env::args().collect();
    if args.get(1).map(String::as_str) != Some("varies") {
        bail!("expected cargo subcommand mode");
    }

    let (varies_args, cargo_args) = split_args_by_double_dash(&args);
    let CargoCli::Varies(mut cli) = CargoCli::parse_from(varies_args);
    cli.normalize_backend_selection();

    Ok((cli, cargo_args))
}

pub fn parse_wrapper_cli_from_env() -> Result<(VariesArgs, Vec<String>)> {
    let args: Vec<String> = env::args().collect();
    if args.get(1).map(String::as_str) != Some(WRAPPER_MODE_ARG) {
        bail!("expected internal wrapper mode")
    }

    let (wrapper_args, rustc_args) = split_args_by_double_dash_from(&args, 2, WRAPPER_MODE_ARG);
    let cli = VariesArgs::parse_flag_args(wrapper_args)?;
    if rustc_args.is_empty() {
        bail!("expected rustc arguments after `--` in wrapper mode")
    }

    Ok((cli, rustc_args))
}

pub fn split_args_by_double_dash(args: &[String]) -> (Vec<String>, Vec<String>) {
    split_args_by_double_dash_from(args, 1, "cargo")
}

fn split_args_by_double_dash_from(
    args: &[String],
    skip: usize,
    head: &str,
) -> (Vec<String>, Vec<String>) {
    let mut left = vec!["cargo".to_owned()];
    let mut right = Vec::new();
    let mut after_dash = false;

    left[0] = head.to_owned();

    for arg in args.iter().skip(skip) {
        if !after_dash && arg == "--" {
            after_dash = true;
            continue;
        }
        if after_dash {
            right.push(arg.clone());
        } else {
            left.push(arg.clone());
        }
    }

    (left, right)
}

pub fn current_exe() -> Result<PathBuf> {
    env::current_exe().context("failed to resolve current executable path")
}

pub fn first_mode_arg() -> Option<String> {
    env::args().nth(1)
}

pub fn resolved_target_dir(cli: &VariesArgs) -> Result<PathBuf> {
    let dir = if let Some(manifest_path) = &cli.manifest_path {
        manifest_path
            .parent()
            .map(Path::to_path_buf)
            .context("manifest path should have a parent directory")?
    } else {
        cli.dir
            .clone()
            .unwrap_or(env::current_dir().context("failed to read current directory")?)
    };
    dir.canonicalize()
        .with_context(|| format!("failed to resolve target directory {}", dir.display()))
}

pub fn resolved_manifest_path(cli: &VariesArgs) -> Result<Option<PathBuf>> {
    let Some(manifest_path) = &cli.manifest_path else {
        return Ok(None);
    };

    manifest_path
        .canonicalize()
        .with_context(|| {
            format!(
                "failed to resolve manifest path {}",
                manifest_path.display()
            )
        })
        .map(Some)
}

pub fn absorb_feature_args(
    mut cli: VariesArgs,
    cargo_args: Vec<String>,
) -> Result<(VariesArgs, Vec<String>)> {
    let mut passthrough = Vec::new();
    let mut index = 0usize;

    while index < cargo_args.len() {
        let arg = &cargo_args[index];
        match arg.as_str() {
            "--all-features" => {
                cli.all_features = true;
                index += 1;
            }
            "--no-default-features" => {
                cli.no_default_features = true;
                index += 1;
            }
            "--features" | "-F" => {
                let spec = cargo_args
                    .get(index + 1)
                    .with_context(|| format!("expected a value after `{arg}`"))?;
                cli.extend_feature_spec(spec);
                index += 2;
            }
            _ => {
                if let Some(spec) = arg.strip_prefix("--features=") {
                    cli.extend_feature_spec(spec);
                } else if let Some(spec) = arg.strip_prefix("-F") {
                    if spec.is_empty() {
                        passthrough.push(arg.clone());
                    } else {
                        cli.extend_feature_spec(spec);
                    }
                } else {
                    passthrough.push(arg.clone());
                }
                index += 1;
            }
        }
    }

    cli.normalize_features();
    Ok((cli, passthrough))
}

impl VariesArgs {
    fn extend_feature_spec(&mut self, spec: &str) {
        self.features.extend(
            spec.split(',')
                .map(str::trim)
                .filter(|feature| !feature.is_empty())
                .map(ToOwned::to_owned),
        );
    }

    fn normalize_features(&mut self) {
        self.features.sort();
        self.features.dedup();
    }

    fn normalize_backend_selection(&mut self) {
        let mut unique = Vec::new();
        self.backends.retain(|backend| {
            if unique.contains(backend) {
                false
            } else {
                unique.push(*backend);
                true
            }
        });
        if self.backends.is_empty() {
            self.backends.push(BackendKind::Kani);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BackendKind, LogLevel, VariesArgs, absorb_feature_args, resolved_target_dir,
        split_args_by_double_dash,
    };
    use std::path::PathBuf;

    #[test]
    fn split_respects_separator() {
        let args = vec![
            "cargo-varies".to_owned(),
            "varies".to_owned(),
            "--dir".to_owned(),
            "fixture".to_owned(),
            "--".to_owned(),
            "--all-features".to_owned(),
        ];
        let (left, right) = split_args_by_double_dash(&args);
        assert_eq!(left, vec!["cargo", "varies", "--dir", "fixture"]);
        assert_eq!(right, vec!["--all-features"]);
    }

    #[test]
    fn resolve_harness_dir_joins_relative_output_path() {
        let cli = VariesArgs {
            dir: None,
            manifest_path: None,
            package: None,
            features: Vec::new(),
            all_features: false,
            no_default_features: false,
            harness_crate: PathBuf::from("ignored"),
            backends: vec![BackendKind::Kani],
            timeout: None,
            check_harness: false,
            max_depth: 4,
            max_mutators_per_target: 2,
            log_level: LogLevel::Info,
        };
        let base = PathBuf::from("/tmp/base");
        let resolved = cli.resolve_harness_dir(&base);

        assert_eq!(resolved, base.join("ignored"));
    }

    #[test]
    fn absorb_feature_args_merges_feature_flags_from_passthrough() {
        let cli = VariesArgs {
            dir: None,
            manifest_path: None,
            package: Some("target-lib".to_owned()),
            features: vec!["cli-only".to_owned()],
            all_features: false,
            no_default_features: false,
            harness_crate: PathBuf::from("varies_test"),
            backends: vec![BackendKind::Kani],
            timeout: None,
            check_harness: false,
            max_depth: 4,
            max_mutators_per_target: 2,
            log_level: LogLevel::Info,
        };
        let cargo_args = vec![
            "--features".to_owned(),
            "target-lib/extra,serde".to_owned(),
            "--all-features".to_owned(),
            "--no-default-features".to_owned(),
            "--jobs".to_owned(),
            "4".to_owned(),
        ];

        let (cli, passthrough) = absorb_feature_args(cli, cargo_args).unwrap();

        assert_eq!(
            cli.features,
            vec![
                "cli-only".to_owned(),
                "serde".to_owned(),
                "target-lib/extra".to_owned()
            ]
        );
        assert!(cli.all_features);
        assert!(cli.no_default_features);
        assert_eq!(passthrough, vec!["--jobs", "4"]);
    }

    #[test]
    fn resolved_target_dir_uses_manifest_parent() {
        let cli = VariesArgs {
            dir: None,
            manifest_path: Some(PathBuf::from("fixtures/basic-lib/Cargo.toml")),
            package: None,
            features: Vec::new(),
            all_features: false,
            no_default_features: false,
            harness_crate: PathBuf::from("varies_test"),
            backends: vec![BackendKind::Kani],
            timeout: None,
            check_harness: false,
            max_depth: 4,
            max_mutators_per_target: 2,
            log_level: LogLevel::Info,
        };

        let resolved = resolved_target_dir(&cli).expect("manifest parent should resolve");
        assert!(resolved.ends_with("fixtures/basic-lib"));
    }

    #[test]
    fn flag_args_round_trip_preserves_cli_configuration() {
        let cli = VariesArgs {
            dir: Some(PathBuf::from("fixtures/basic-lib")),
            manifest_path: None,
            package: Some("basic-lib".to_owned()),
            features: vec!["serde".to_owned(), "feature-a".to_owned()],
            all_features: true,
            no_default_features: true,
            harness_crate: PathBuf::from("custom-harness"),
            backends: vec![BackendKind::Kani],
            timeout: Some(30),
            check_harness: true,
            max_depth: 7,
            max_mutators_per_target: 3,
            log_level: LogLevel::Info,
        };

        let mut args = vec!["varies".to_owned()];
        args.extend(cli.flag_args());
        let decoded = VariesArgs::parse_flag_args(args).expect("flags should parse");

        assert_eq!(decoded, cli);
    }

    #[test]
    fn parse_flag_args_accepts_short_common_options() {
        let cli = VariesArgs::parse_flag_args([
            "varies",
            "-C",
            "fixtures/basic-lib",
            "-o",
            "custom-harness",
            "-c",
            "-d",
            "7",
            "-M",
            "3",
            "-t",
            "30",
        ])
        .expect("short options should parse");

        assert_eq!(cli.dir, Some(PathBuf::from("fixtures/basic-lib")));
        assert_eq!(cli.harness_crate, PathBuf::from("custom-harness"));
        assert!(cli.check_harness);
        assert_eq!(cli.max_depth, 7);
        assert_eq!(cli.max_mutators_per_target, 3);
        assert_eq!(cli.timeout, Some(30));
    }

    #[test]
    fn parse_flag_args_defaults_max_depth_to_unlimited() {
        let cli = VariesArgs::parse_flag_args(["varies"]).expect("defaults should parse");

        assert_eq!(cli.max_depth, 0);
        assert_eq!(cli.max_mutators_per_target, 4);
        assert!(!cli.check_harness);
        assert_eq!(cli.log_level, LogLevel::Info);
    }

    #[test]
    fn parse_flag_args_accepts_explicit_log_level() {
        let cli = VariesArgs::parse_flag_args(["varies", "--log-level", "debug"])
            .expect("log level should parse");

        assert_eq!(cli.log_level, LogLevel::Debug);
    }

    #[test]
    fn parse_flag_args_accepts_multiple_backends() {
        let cli =
            VariesArgs::parse_flag_args(["varies", "--backend", "kani,tests", "--backend", "fuzz"])
                .expect("multiple backends should parse");

        assert_eq!(
            cli.backends,
            vec![BackendKind::Kani, BackendKind::Tests, BackendKind::Fuzz]
        );
    }

    #[test]
    fn backend_harness_dir_uses_backend_subdirs_only_for_multi_backend_runs() {
        let mut cli = VariesArgs::parse_flag_args(["varies", "--out", "suite"])
            .expect("defaults should parse");
        let base = PathBuf::from("/tmp/base");

        assert_eq!(
            cli.backend_harness_dir(&base, BackendKind::Kani),
            base.join("suite")
        );

        cli.backends = vec![BackendKind::Kani, BackendKind::Tests];
        assert_eq!(
            cli.backend_harness_dir(&base, BackendKind::Tests),
            base.join("suite").join("tests")
        );
    }

    #[test]
    fn cargo_feature_args_render_consistently() {
        let cli = VariesArgs {
            dir: None,
            manifest_path: None,
            package: None,
            features: vec!["serde".to_owned(), "alloc".to_owned()],
            all_features: true,
            no_default_features: true,
            harness_crate: PathBuf::from("varies_test"),
            backends: vec![BackendKind::Kani],
            timeout: None,
            check_harness: false,
            max_depth: 4,
            max_mutators_per_target: 2,
            log_level: LogLevel::Info,
        };

        assert_eq!(
            cli.cargo_feature_args(),
            vec![
                "--features".to_owned(),
                "serde,alloc".to_owned(),
                "--all-features".to_owned(),
                "--no-default-features".to_owned(),
            ]
        );
    }
}
