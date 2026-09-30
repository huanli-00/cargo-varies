use crate::cli::VariesArgs;
use crate::rapx_graph::ApiDependencyGraph;
use crate::synth::SynthesizedSuite;
use crate::workspace::WorkspaceTarget;
use anyhow::Result;
use clap::ValueEnum;
use rustc_middle::ty::TyCtxt;
use std::ffi::OsString;
use std::path::Path;

mod common;
mod fuzz;
mod kani;
mod sequence_fn;
mod test_cases;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
/// Selects the backend used to render and validate synthesized suites.
pub enum BackendKind {
    Fuzz,
    Kani,
    Tests,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fuzz => "fuzz",
            Self::Kani => "kani",
            Self::Tests => "tests",
        }
    }
}

/// Bundles the rustc context plus workspace metadata needed by a backend
/// renderer.
pub struct BackendRenderContext<'a, 'tcx> {
    pub tcx: TyCtxt<'tcx>,
    pub graph: &'a ApiDependencyGraph<'tcx>,
    pub target: &'a WorkspaceTarget,
    pub harness_dir: &'a Path,
    pub suite: &'a SynthesizedSuite<'tcx>,
    pub cli: &'a VariesArgs,
}

/// Describes an optional post-generation command that validates the emitted
/// harness crate for a backend.
pub struct BackendValidationCommand {
    pub program: &'static str,
    pub args: Vec<OsString>,
    pub description: &'static str,
}

/// Renders a synthesized verification suite for one backend and optionally
/// exposes a command that validates the generated harness crate.
pub trait SuiteBackend {
    fn kind(&self) -> BackendKind;

    fn write_suite<'tcx>(&self, context: BackendRenderContext<'_, 'tcx>) -> Result<()>;

    fn validation_command(&self, manifest: &Path) -> Option<BackendValidationCommand>;
}

/// Render a synthesized suite with the selected backend implementation.
pub fn write_suite<'tcx>(
    backend: BackendKind,
    context: BackendRenderContext<'_, 'tcx>,
) -> Result<()> {
    let implementation = backend_impl(backend);
    debug_assert_eq!(implementation.kind(), backend);
    implementation.write_suite(context)
}

/// Return the backend-specific validation command for a generated harness
/// manifest, if the backend supports one.
pub fn validation_command(
    backend: BackendKind,
    manifest: &Path,
) -> Option<BackendValidationCommand> {
    let implementation = backend_impl(backend);
    debug_assert_eq!(implementation.kind(), backend);
    implementation.validation_command(manifest)
}

fn backend_impl(backend: BackendKind) -> &'static dyn SuiteBackend {
    match backend {
        BackendKind::Fuzz => &fuzz::FUZZ_BACKEND,
        BackendKind::Kani => &kani::KANI_BACKEND,
        BackendKind::Tests => &test_cases::TEST_CASES_BACKEND,
    }
}
