use crate::cli::VariesArgs;
use crate::pipeline::run_pipeline;
use crate::workspace::discover_target_for_analysis;
use anyhow::{Context, Result};
use log::{debug, info};
use rustc_driver::{Callbacks, Compilation};
use rustc_interface::interface::Compiler;
use rustc_middle::ty::TyCtxt;
use std::env;
use std::sync::{Arc, Mutex};

const ANALYSIS_DEFAULT_ARGS: &[&str] = &[
    "-Zalways-encode-mir",
    "-Zmir-opt-level=0",
    "-Zinline-mir-threshold=0",
    "-Zinline-mir-hint-threshold=0",
    "-Zcross-crate-inline-threshold=0",
];

pub fn run_wrapper(cli: VariesArgs, rustc_args: Vec<String>) -> Result<()> {
    let mut callback = VariesCallback::new(cli);
    let mut args = rustc_args;
    args.extend(ANALYSIS_DEFAULT_ARGS.iter().map(ToString::to_string));

    rustc_driver::install_ice_hook("https://github.com/rust-lang/rust/issues/new", |_| ());
    rustc_driver::run_compiler(&args, &mut callback);

    if let Some(err) = callback.take_error() {
        return Err(err);
    }

    Ok(())
}
struct VariesCallback {
    cli: VariesArgs,
    error: Arc<Mutex<Option<anyhow::Error>>>,
}

impl VariesCallback {
    fn new(cli: VariesArgs) -> Self {
        Self {
            cli,
            error: Arc::new(Mutex::new(None)),
        }
    }

    fn take_error(&mut self) -> Option<anyhow::Error> {
        self.error.lock().ok()?.take()
    }

    fn store_error(&self, err: anyhow::Error) {
        if let Ok(mut slot) = self.error.lock()
            && slot.is_none()
        {
            *slot = Some(err);
        }
    }
}

impl Callbacks for VariesCallback {
    fn after_analysis<'tcx>(&mut self, _compiler: &Compiler, tcx: TyCtxt<'tcx>) -> Compilation {
        if let Err(err) = run_analysis(tcx, &self.cli) {
            self.store_error(err);
        }
        Compilation::Continue
    }
}

fn run_analysis<'tcx>(tcx: TyCtxt<'tcx>, cli: &VariesArgs) -> Result<()> {
    print_field_facts_start(cli)?;

    let summary = run_pipeline(tcx, cli)?;
    info!(
        target: "varies::summary",
        "varies summary:
  api instances:                           {}
  mono instances:                          {}
  supported apis:                          {}
  target api defs (pre-mono):              {}
  identified target api instances:         {}
  synthesized target api defs (pre-mono):  {}
  synthesized target api instances:        {}
  generated sequences:                     {}",
        summary.total_api_count,
        summary.mono_api_count,
        summary.supported_api_count,
        summary.pre_mono_target_api_count,
        summary.target_api_count,
        summary.pre_mono_synthesized_target_api_count,
        summary.synthesized_target_api_count,
        summary.sequence_count,
    );
    Ok(())
}

fn print_field_facts_start(cli: &VariesArgs) -> Result<()> {
    let cwd = env::current_dir().context("failed to resolve current directory in wrapper mode")?;
    let target = discover_target_for_analysis(&cwd, cli.package.as_deref())?;
    let harness_dir = cli.resolve_harness_dir(&cwd);

    info!(
        target: "varies::driver",
        "starting synthesis:
  crate: {}
  strategy: unsafe-wrapper
  backend(s): {}
  output dir: {}",
        target.lib_crate_name,
        cli.backend_list_label(),
        harness_dir.display(),
    );
    debug!(target: "varies::driver", "entering rustc analysis callback for primary library target");

    Ok(())
}
