mod planner;

use super::{ArgPlan, CallStep, SynthesisConfig, SynthesizedSuite};
use crate::rapx_graph::ApiDependencyGraph;
use crate::unsafe_analysis::ApiFieldFactsIndex;
use rustc_middle::ty::TyCtxt;

use planner::Planner;

const DEFAULT_BASIC_SEQUENCE_LIMIT: usize = 8;
const NEAREST_TARGET_THRESHOLD: u32 = 2;
const NO_MUTATOR_AFTER_TARGET: bool = true;
const DEFAULT_MUTATION_ROUND_LIMIT: usize = 8;
const MIN_MUTATION_KIDS_PER_PARENT: usize = 2;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct MutatorPlan {
    prefix: Vec<CallStep>,
    args: Vec<Option<ArgPlan>>,
}

#[derive(Clone, Debug)]
struct PartialMutatorPlan {
    prefix: Vec<CallStep>,
    args: Vec<Option<ArgPlan>>,
    next_param_index: usize,
}

pub(super) fn synthesize_suite<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    field_facts_index: &ApiFieldFactsIndex,
    config: &SynthesisConfig,
) -> SynthesizedSuite<'tcx> {
    Planner::new(
        tcx,
        graph,
        field_facts_index,
        config.max_depth,
        config.max_mutators_per_target,
    )
    .synthesize()
}
