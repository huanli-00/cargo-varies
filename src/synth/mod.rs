mod api;
mod unsafe_wrapper;

use crate::rapx_graph::TyWrapper;
use crate::unsafe_analysis::ApiFieldFacts;
use rustc_middle::ty::Ty;

#[derive(Clone, Debug)]
/// Captures the synthesized view of one callable API, including its resolved
/// signature, safety class, and output adaptation requirements.
pub struct ApiDescriptor<'tcx> {
    pub index: usize,
    pub path: String,
    pub target_family: String,
    pub instantiated_path: String,
    pub concrete_args: Vec<String>,
    pub is_mono: bool,
    pub inputs: Vec<Ty<'tcx>>,
    pub value_output: Ty<'tcx>,
    pub value_output_key: TyWrapper<'tcx>,
    pub output_adapter: OutputAdapter,
    pub api_safety: ApiSafety,
    pub field_facts: ApiFieldFacts,
    pub supported: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Classifies how an API exposes unsafe behavior to the public surface.
pub enum ApiSafety {
    Safe,
    UnsafeFn,
    BuiltinUnsafe,
    UnsafeBlock,
    IndirectUnsafe,
}

impl ApiSafety {
    pub fn as_str(self) -> &'static str {
        match self {
            ApiSafety::Safe => "safe",
            ApiSafety::UnsafeFn => "unsafe_fn",
            ApiSafety::BuiltinUnsafe => "builtin_unsafe",
            ApiSafety::UnsafeBlock => "unsafe_block",
            ApiSafety::IndirectUnsafe => "indirect_unsafe",
        }
    }

    fn is_unsafe_fn(self) -> bool {
        matches!(self, ApiSafety::UnsafeFn)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Indicates whether a call result must be unwrapped before later steps can
/// treat it as a produced value.
pub enum OutputAdapter {
    Plain,
    Option,
    Result,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
/// Records how one sequence argument borrows or moves its source value.
pub enum BorrowMode {
    Move,
    Shared,
    Mutable,
    RawConst,
    RawMutable,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
/// Identifies whether an argument comes from a symbolic parameter or a prior
/// call step in the same sequence.
pub enum ValueSource {
    Param(usize),
    Step(usize),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
/// Plans how one argument is sourced and borrowed for a call step.
pub struct ArgPlan {
    pub source: ValueSource,
    pub borrow: BorrowMode,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
/// Represents one API invocation inside a synthesized sequence.
pub struct CallStep {
    pub api_index: usize,
    pub args: Vec<ArgPlan>,
}

#[derive(Clone, Debug)]
/// Describes one synthesized target-focused verification sequence and its
/// predecessor/successor metadata.
pub struct SequencePlan {
    pub steps: Vec<CallStep>,
    pub target_api: usize,
    pub unsafe_wrapper: String,
    pub target_step: usize,
    pub predecessor: Option<usize>,
    pub successor: Vec<usize>,
    pub latest_call: usize,
    pub mutated_param: Option<usize>,
    pub ranking_value: f32,
}

#[derive(Clone)]
/// Groups all supported APIs and the synthesized unsafe-wrapper sequences.
pub struct SynthesizedSuite<'tcx> {
    pub apis: Vec<ApiDescriptor<'tcx>>,
    pub sequences: Vec<SequencePlan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Configures synthesis depth and mutator expansion limits.
pub struct SynthesisConfig {
    pub max_depth: usize,
    pub max_mutators_per_target: usize,
}

impl<'tcx> ApiDescriptor<'tcx> {
    pub fn primary_unsafe_wrapper(&self) -> String {
        self.field_facts
            .unsafe_functions
            .iter()
            .find(|function| !is_std_internal_unsafe_function(function))
            .or_else(|| self.field_facts.unsafe_functions.first())
            .cloned()
            .unwrap_or_else(|| self.instantiated_path.clone())
    }
}

fn is_std_internal_unsafe_function(function: &str) -> bool {
    matches!(function.split("::").next(), Some("std" | "core" | "alloc"))
}

impl<'tcx> ApiDescriptor<'tcx> {
    pub fn unsafe_wrappers_without_std_internal(&self) -> Vec<String> {
        let filtered = self
            .field_facts
            .unsafe_functions
            .iter()
            .filter(|function| !is_std_internal_unsafe_function(function))
            .cloned()
            .collect::<Vec<_>>();
        if filtered.is_empty() {
            self.field_facts.unsafe_functions.clone()
        } else {
            filtered
        }
    }
}

pub(crate) use api::{classify_api_safety, is_supported_callable, render_api_path};

/// Synthesize target-focused verification sequences for direct unsafe wrappers.
pub fn synthesize_suite<'tcx>(
    tcx: rustc_middle::ty::TyCtxt<'tcx>,
    graph: &crate::rapx_graph::ApiDependencyGraph<'tcx>,
    field_facts_index: &crate::unsafe_analysis::ApiFieldFactsIndex,
    config: &SynthesisConfig,
) -> SynthesizedSuite<'tcx> {
    unsafe_wrapper::synthesize_suite(tcx, graph, field_facts_index, config)
}
