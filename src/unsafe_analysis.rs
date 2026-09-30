use crate::synth::ApiSafety;
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_hir::intravisit::{FnKind, Visitor};
use rustc_middle::mir::visit::{PlaceContext, Visitor as MirVisitor};
use rustc_middle::mir::{
    self, Body, Local, Operand, Place, ProjectionElem, Rvalue, Statement, StatementKind,
    Terminator, TerminatorKind,
};
use rustc_middle::ty::{self, TyCtxt};
use rustc_span::Span;
use rustc_span::symbol::sym;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::OnceLock;
use std::time::Instant;

pub type FieldPath = Vec<u32>;
pub type FieldSet = BTreeSet<FieldPath>;
const MAX_FIELD_PATH_DEPTH: usize = 2;
const UNION_FIELD_BASE: u32 = 1 << 30;
const STD_ANNOTATED_PARAMS_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/std_annotated_params.json"
));

/// Endpoint root used in the field-transfer graph.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TransferRoot {
    Param(usize),
    Return,
}

/// Endpoint in the field-transfer graph, addressed by an API-surface root plus
/// a bounded field path.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TransferPoint {
    pub root: TransferRoot,
    pub path: FieldPath,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FieldTransfer {
    pub src: TransferPoint,
    pub dst: TransferPoint,
    pub distance: u32,
}

#[derive(Clone, Debug, Default)]
pub struct ApiFieldFacts {
    #[allow(dead_code)]
    pub touched_inputs: Vec<FieldSet>,
    pub touched_output: FieldSet,
    pub mutated_inputs: Vec<FieldSet>,
    pub contract_inputs: Vec<FieldSet>,
    pub contract_output: FieldSet,
    pub propagated_inputs: Vec<FieldSet>,
    #[allow(dead_code)]
    pub unsafe_inputs: Vec<FieldSet>,
    #[allow(dead_code)]
    pub unsafe_output: FieldSet,
    pub unsafe_functions: Vec<String>,
    pub direct_unsafe: bool,
    pub has_transitive_unsafe: bool,
    #[allow(dead_code)]
    pub transfers: Vec<FieldTransfer>,
}

impl ApiFieldFacts {
    pub fn is_unsafe_related(&self) -> bool {
        self.has_transitive_unsafe
    }

    #[allow(dead_code)]
    pub fn input_overlaps_touched(&self, index: usize, required: &FieldSet) -> bool {
        field_sets_overlap(self.touched_inputs.get(index), required)
    }

    pub fn input_overlaps_mutated(&self, index: usize, required: &FieldSet) -> bool {
        field_sets_overlap(self.mutated_inputs.get(index), required)
    }

    pub fn output_overlaps_relevant(&self, required: &FieldSet) -> bool {
        if field_sets_overlap(Some(&self.contract_output), required) {
            return true;
        }
        field_sets_overlap(Some(&self.touched_output), required)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ApiFieldFactsIndex {
    api_field_facts: HashMap<DefId, ApiFieldFacts>,
}

impl ApiFieldFactsIndex {
    pub fn field_facts_for(&self, def_id: DefId) -> ApiFieldFacts {
        self.api_field_facts
            .get(&def_id)
            .cloned()
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum InterfaceRoot {
    Param(usize),
    Return,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct InterfacePlace {
    root: InterfaceRoot,
    path: FieldPath,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
/// Internal fixed-point summary derived from MIR before exposing the public
/// `ApiFieldFacts`.
struct FunctionFieldFacts {
    touched_inputs: Vec<FieldSet>,
    touched_output: FieldSet,
    mutated_inputs: Vec<FieldSet>,
    contract_inputs: Vec<FieldSet>,
    contract_output: FieldSet,
    propagated_inputs: Vec<FieldSet>,
    unsafe_inputs: Vec<FieldSet>,
    unsafe_output: FieldSet,
    unsafe_functions: Vec<String>,
    direct_unsafe: bool,
    has_transitive_unsafe: bool,
    callsites: Vec<CallSiteFacts>,
    transfers: Vec<FieldTransfer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// One outgoing call together with the caller-side origin mapping of its
/// arguments.
struct CallSiteFacts {
    callee: DefId,
    arg_origins: Vec<BTreeSet<InterfacePlace>>,
}

struct UnsafeContractExtraction<'tcx> {
    relevant_places: Vec<Place<'tcx>>,
    relevant_operands: Vec<Operand<'tcx>>,
    mark_destination_output: bool,
    detected: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct StdAnnotatedParamEntry {
    #[serde(default)]
    params: Vec<String>,
}

/// Field-sensitive origin map from MIR locals back to API-surface inputs or
/// return positions.
type OriginMap = BTreeMap<Local, BTreeSet<InterfacePlace>>;

fn std_annotated_params() -> &'static HashMap<String, StdAnnotatedParamEntry> {
    static ENTRIES: OnceLock<HashMap<String, StdAnnotatedParamEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        serde_json::from_str(STD_ANNOTATED_PARAMS_JSON)
            .expect("embedded std annotated param json should parse")
    })
}

pub fn analyze_public_unsafe_surfaces<'tcx>(tcx: TyCtxt<'tcx>) -> ApiFieldFactsIndex {
    let analysis_start = Instant::now();
    let callables = collect_callable_def_ids(tcx);
    log::info!(
        target: "varies::unsafe_analysis",
        "unsafe analysis collected {} callable bodies",
        callables.len()
    );
    let mut function_facts = callables
        .iter()
        .map(|def_id| {
            (
                *def_id,
                derive_function_facts(*def_id, tcx, &HashMap::new()),
            )
        })
        .collect::<HashMap<_, _>>();
    log::info!(
        target: "varies::unsafe_analysis",
        "unsafe analysis initial MIR summaries completed in {:.2?}",
        analysis_start.elapsed()
    );

    let callers_by_callee = callers_by_callee(&function_facts);
    let mut worklist = VecDeque::from(callables.clone());
    let mut queued = callables.iter().copied().collect::<HashSet<_>>();
    let mut recomputed = 0usize;
    while let Some(def_id) = worklist.pop_front() {
        queued.remove(&def_id);
        let next = derive_function_facts(def_id, tcx, &function_facts);
        if function_facts.get(&def_id) == Some(&next) {
            continue;
        }
        recomputed += 1;
        function_facts.insert(def_id, next);
        if let Some(callers) = callers_by_callee.get(&def_id) {
            for caller in callers {
                if queued.insert(*caller) {
                    worklist.push_back(*caller);
                }
            }
        }
    }
    log::info!(
        target: "varies::unsafe_analysis",
        "unsafe analysis worklist propagation recomputed {} summaries in {:.2?}",
        recomputed,
        analysis_start.elapsed()
    );

    let api_field_facts = function_facts
        .into_iter()
        .map(|(def_id, field_facts)| {
            (
                def_id,
                ApiFieldFacts {
                    touched_inputs: field_facts.touched_inputs,
                    touched_output: field_facts.touched_output,
                    mutated_inputs: field_facts.mutated_inputs,
                    contract_inputs: field_facts.contract_inputs,
                    contract_output: field_facts.contract_output,
                    propagated_inputs: field_facts.propagated_inputs,
                    unsafe_inputs: field_facts.unsafe_inputs,
                    unsafe_output: field_facts.unsafe_output,
                    unsafe_functions: field_facts.unsafe_functions,
                    direct_unsafe: field_facts.direct_unsafe,
                    has_transitive_unsafe: field_facts.has_transitive_unsafe,
                    transfers: field_facts.transfers,
                },
            )
        })
        .collect();

    ApiFieldFactsIndex { api_field_facts }
}

fn callers_by_callee(
    function_facts: &HashMap<DefId, FunctionFieldFacts>,
) -> HashMap<DefId, Vec<DefId>> {
    let mut callers = HashMap::<DefId, Vec<DefId>>::new();
    for (caller, facts) in function_facts {
        for callsite in &facts.callsites {
            callers.entry(callsite.callee).or_default().push(*caller);
        }
    }
    callers
}

/// Derive the internal field summary for one callable item.
fn derive_function_facts<'tcx>(
    def_id: DefId,
    tcx: TyCtxt<'tcx>,
    callee_summaries: &HashMap<DefId, FunctionFieldFacts>,
) -> FunctionFieldFacts {
    if def_id.as_local().is_none() {
        return FunctionFieldFacts::default();
    }
    let body = tcx.optimized_mir(def_id);
    let input_count = body.args_iter().count();
    let direct_unsafe = direct_api_safety(def_id, tcx) == ApiSafety::UnsafeBlock;

    // Phase 1: field-sensitive dataflow analysis.
    let origins = solve_origin_map(body);
    let mut collector = DataflowFactsCollector {
        tcx,
        body,
        origins: &origins,
        callee_summaries,
        facts: FunctionFieldFacts::new(input_count, direct_unsafe),
    };
    collector.visit_body(body);
    collector.facts.transfers = build_field_transfers(body, &origins, callee_summaries, tcx);

    // Phase 2: unsafe propagation extraction.
    let unsafe_spans = explicit_unsafe_block_spans(def_id, tcx);
    let (contract_inputs, contract_output) =
        derive_contract_summary(body, &origins, callee_summaries, &unsafe_spans, tcx);
    collector.facts.contract_inputs = contract_inputs;
    collector.facts.contract_output = contract_output;
    let (unsafe_inputs, unsafe_output, has_transitive_unsafe) =
        derive_unsafe_summary(body, &origins, callee_summaries, &unsafe_spans, tcx);
    collector.facts.unsafe_inputs = unsafe_inputs;
    collector.facts.unsafe_output = unsafe_output;
    collector.facts.unsafe_functions = collect_reachable_unsafe_functions(
        def_id,
        &unsafe_spans,
        &collector.facts.callsites,
        callee_summaries,
        tcx,
    );
    collector.facts.propagated_inputs =
        derive_propagated_inputs(&collector.facts.transfers, &collector.facts, input_count);
    collector.facts.has_transitive_unsafe = has_transitive_unsafe || direct_unsafe;
    normalize_field_facts(&mut collector.facts);
    collector.facts
}

impl FunctionFieldFacts {
    fn new(input_count: usize, direct_unsafe: bool) -> Self {
        Self {
            touched_inputs: vec![FieldSet::new(); input_count],
            touched_output: FieldSet::new(),
            mutated_inputs: vec![FieldSet::new(); input_count],
            contract_inputs: vec![FieldSet::new(); input_count],
            contract_output: FieldSet::new(),
            propagated_inputs: vec![FieldSet::new(); input_count],
            unsafe_inputs: vec![FieldSet::new(); input_count],
            unsafe_output: FieldSet::new(),
            unsafe_functions: Vec::new(),
            direct_unsafe,
            has_transitive_unsafe: direct_unsafe,
            callsites: Vec::new(),
            transfers: Vec::new(),
        }
    }
}

fn collect_reachable_unsafe_functions<'tcx>(
    def_id: DefId,
    unsafe_spans: &[Span],
    callsites: &[CallSiteFacts],
    callee_summaries: &HashMap<DefId, FunctionFieldFacts>,
    tcx: TyCtxt<'tcx>,
) -> Vec<String> {
    let mut functions = Vec::new();
    let mut seen = BTreeSet::new();
    if !unsafe_spans.is_empty() {
        let current = tcx.def_path_str(def_id);
        if seen.insert(current.clone()) {
            functions.push(current);
        }
    }
    for callsite in callsites {
        if let Some(callee_facts) = callee_summaries.get(&callsite.callee) {
            for function in &callee_facts.unsafe_functions {
                if seen.insert(function.clone()) {
                    functions.push(function.clone());
                }
            }
            continue;
        }

        if tcx
            .fn_sig(callsite.callee)
            .instantiate_identity()
            .safety()
            .is_unsafe()
        {
            let external = tcx.def_path_str(callsite.callee);
            if seen.insert(external.clone()) {
                functions.push(external);
            }
        }
    }
    functions
}

fn solve_origin_map<'tcx>(body: &Body<'tcx>) -> OriginMap {
    let mut origins = OriginMap::new();
    for (index, local) in body.args_iter().enumerate() {
        origins.insert(
            local,
            BTreeSet::from([InterfacePlace {
                root: InterfaceRoot::Param(index),
                path: Vec::new(),
            }]),
        );
    }
    origins.insert(
        mir::RETURN_PLACE,
        BTreeSet::from([InterfacePlace {
            root: InterfaceRoot::Return,
            path: Vec::new(),
        }]),
    );

    let limit = body.local_decls.len().max(1) * 4;
    for _ in 0..limit {
        let mut changed = false;
        for block in body.basic_blocks.iter() {
            for statement in &block.statements {
                changed |= apply_statement_origins(statement, body, &mut origins);
            }
        }
        if !changed {
            break;
        }
    }

    origins
}

fn apply_statement_origins<'tcx>(
    statement: &Statement<'tcx>,
    body: &Body<'tcx>,
    origins: &mut OriginMap,
) -> bool {
    let StatementKind::Assign(assign) = &statement.kind else {
        return false;
    };
    let (destination, rvalue) = &**assign;
    let mut new_origins = rvalue_origins(rvalue, body, origins);
    if new_origins.is_empty() {
        new_origins = place_origins(*destination, body, origins);
    }
    union_local_origins(destination.local, new_origins, origins)
}

fn union_local_origins(
    local: Local,
    new_origins: BTreeSet<InterfacePlace>,
    origins: &mut OriginMap,
) -> bool {
    if new_origins.is_empty() {
        return false;
    }
    let entry = origins.entry(local).or_default();
    let before = entry.len();
    entry.extend(new_origins);
    entry.len() != before
}

fn rvalue_origins<'tcx>(
    rvalue: &Rvalue<'tcx>,
    body: &Body<'tcx>,
    origins: &OriginMap,
) -> BTreeSet<InterfacePlace> {
    match rvalue {
        Rvalue::Use(operand)
        | Rvalue::Cast(_, operand, _)
        | Rvalue::UnaryOp(_, operand)
        | Rvalue::WrapUnsafeBinder(operand, _) => operand_origins(operand, body, origins),
        Rvalue::Repeat(operand, _) => operand_origins(operand, body, origins),
        Rvalue::Ref(_, _, place)
        | Rvalue::RawPtr(_, place)
        | Rvalue::CopyForDeref(place)
        | Rvalue::Discriminant(place) => place_origins(*place, body, origins),
        Rvalue::ThreadLocalRef(_) => BTreeSet::new(),
        Rvalue::BinaryOp(_, operands) => {
            let (lhs, rhs) = &**operands;
            let mut merged = operand_origins(lhs, body, origins);
            merged.extend(operand_origins(rhs, body, origins));
            merged
        }
        Rvalue::Aggregate(_, operands) => operands
            .iter()
            .flat_map(|operand| operand_origins(operand, body, origins))
            .collect(),
    }
}

fn operand_origins<'tcx>(
    operand: &Operand<'tcx>,
    body: &Body<'tcx>,
    origins: &OriginMap,
) -> BTreeSet<InterfacePlace> {
    match operand {
        Operand::Copy(place) | Operand::Move(place) => place_origins(*place, body, origins),
        Operand::Constant(..) => BTreeSet::new(),
        Operand::RuntimeChecks(..) => BTreeSet::new(),
    }
}

fn place_origins<'tcx>(
    place: Place<'tcx>,
    body: &Body<'tcx>,
    origins: &OriginMap,
) -> BTreeSet<InterfacePlace> {
    let suffix = place_suffix(place);
    if let Some(index) = body.args_iter().position(|local| local == place.local) {
        return BTreeSet::from([InterfacePlace {
            root: InterfaceRoot::Param(index),
            path: suffix,
        }]);
    }
    if place.local == mir::RETURN_PLACE {
        return BTreeSet::from([InterfacePlace {
            root: InterfaceRoot::Return,
            path: suffix,
        }]);
    }

    origins
        .get(&place.local)
        .into_iter()
        .flat_map(|stored| {
            stored.iter().map(|origin| InterfacePlace {
                root: origin.root.clone(),
                path: combine_paths(&origin.path, &suffix),
            })
        })
        .collect()
}

fn place_suffix(place: Place<'_>) -> FieldPath {
    let mut path = Vec::new();
    let mut current_ty = None;
    for elem in place.projection.iter() {
        match elem {
            ProjectionElem::Field(field, field_ty) => {
                let field_index = if current_ty.as_ref().is_some_and(|ty: &ty::Ty<'_>| {
                    matches!(ty.kind(), ty::TyKind::Adt(def, _) if def.is_union())
                }) {
                    UNION_FIELD_BASE + field.as_u32()
                } else {
                    field.as_u32()
                };
                path.push(field_index);
                current_ty = Some(field_ty);
                if path.len() >= MAX_FIELD_PATH_DEPTH {
                    break;
                }
            }
            ProjectionElem::Deref => {
                current_ty = match current_ty {
                    Some(ty) => match ty.kind() {
                        ty::TyKind::Ref(_, inner, _) | ty::TyKind::RawPtr(inner, _) => Some(*inner),
                        _ => None,
                    },
                    None => None,
                };
            }
            _ => break,
        }
    }
    path
}

fn combine_paths(prefix: &FieldPath, suffix: &FieldPath) -> FieldPath {
    let mut combined = prefix.clone();
    combined.extend(suffix.iter().copied());
    combined.truncate(MAX_FIELD_PATH_DEPTH);
    combined
}

fn field_sets_overlap(left: Option<&FieldSet>, right: &FieldSet) -> bool {
    let Some(left) = left else {
        return false;
    };
    if left.is_empty() || right.is_empty() {
        return false;
    }
    left.iter()
        .any(|lhs| right.iter().any(|rhs| paths_overlap(lhs, rhs)))
}

fn paths_overlap(left: &FieldPath, right: &FieldPath) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn normalize_field_facts(facts: &mut FunctionFieldFacts) {
    for fields in &mut facts.touched_inputs {
        normalize_field_set(fields);
    }
    normalize_field_set(&mut facts.touched_output);
    for fields in &mut facts.mutated_inputs {
        normalize_field_set(fields);
    }
    for fields in &mut facts.contract_inputs {
        normalize_field_set(fields);
    }
    normalize_field_set(&mut facts.contract_output);
    for fields in &mut facts.propagated_inputs {
        normalize_field_set(fields);
    }
    for fields in &mut facts.unsafe_inputs {
        normalize_field_set(fields);
    }
    normalize_field_set(&mut facts.unsafe_output);
}

fn build_field_transfers<'tcx>(
    body: &Body<'tcx>,
    origins: &OriginMap,
    callee_summaries: &HashMap<DefId, FunctionFieldFacts>,
    tcx: TyCtxt<'tcx>,
) -> Vec<FieldTransfer> {
    let mut transfers = BTreeMap::new();

    for block in body.basic_blocks.iter() {
        for statement in &block.statements {
            let StatementKind::Assign(assign) = &statement.kind else {
                continue;
            };
            let (destination, rvalue) = &**assign;
            let src_origins = rvalue_origins(rvalue, body, origins);
            let dst_origins = place_origins(*destination, body, origins);
            record_transfer_edges(&mut transfers, src_origins, dst_origins, 1);
        }

        let Some(terminator) = &block.terminator else {
            continue;
        };
        let TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } = &terminator.kind
        else {
            continue;
        };
        let Some(callee) = callee_def_id_from_operand(func, body, tcx) else {
            continue;
        };
        let Some(callee_facts) = callee_summaries.get(&callee) else {
            continue;
        };
        if callee_facts.transfers.is_empty() {
            continue;
        }

        let arg_origins = args
            .iter()
            .map(|arg| operand_origins(&arg.node, body, origins))
            .collect::<Vec<_>>();
        let return_origins = place_origins(*destination, body, origins);

        for transfer in &callee_facts.transfers {
            let src_origins =
                instantiate_transfer_point(&transfer.src, &arg_origins, &return_origins);
            let dst_origins =
                instantiate_transfer_point(&transfer.dst, &arg_origins, &return_origins);
            record_transfer_edges(
                &mut transfers,
                src_origins,
                dst_origins,
                transfer.distance.saturating_add(1),
            );
        }
    }

    transfers
        .into_iter()
        .map(|((src, dst), distance)| FieldTransfer { src, dst, distance })
        .collect()
}

fn record_transfer_edges(
    transfers: &mut BTreeMap<(TransferPoint, TransferPoint), u32>,
    src_origins: BTreeSet<InterfacePlace>,
    dst_origins: BTreeSet<InterfacePlace>,
    distance: u32,
) {
    for src in src_origins {
        for dst in &dst_origins {
            let key = (to_transfer_point(&src), to_transfer_point(dst));
            match transfers.get_mut(&key) {
                Some(existing) => {
                    if distance < *existing {
                        *existing = distance;
                    }
                }
                None => {
                    transfers.insert(key, distance);
                }
            }
        }
    }
}

fn instantiate_transfer_point(
    place: &TransferPoint,
    arg_origins: &[BTreeSet<InterfacePlace>],
    return_origins: &BTreeSet<InterfacePlace>,
) -> BTreeSet<InterfacePlace> {
    let origins = match place.root {
        TransferRoot::Param(index) => arg_origins.get(index).cloned().unwrap_or_default(),
        TransferRoot::Return => return_origins.clone(),
    };
    origins
        .into_iter()
        .map(|origin| InterfacePlace {
            root: origin.root,
            path: combine_paths(&origin.path, &place.path),
        })
        .collect()
}

fn to_transfer_point(origin: &InterfacePlace) -> TransferPoint {
    TransferPoint {
        root: match origin.root {
            InterfaceRoot::Param(index) => TransferRoot::Param(index),
            InterfaceRoot::Return => TransferRoot::Return,
        },
        path: origin.path.clone(),
    }
}

fn derive_propagated_inputs(
    transfers: &[FieldTransfer],
    field_facts: &FunctionFieldFacts,
    input_count: usize,
) -> Vec<FieldSet> {
    let mut propagated = vec![FieldSet::new(); input_count];

    for transfer in transfers {
        let TransferRoot::Param(src_index) = transfer.src.root else {
            continue;
        };
        let relevant_dst = match transfer.dst.root {
            TransferRoot::Param(dst_index) => field_facts
                .mutated_inputs
                .get(dst_index)
                .is_some_and(|fields| {
                    fields
                        .iter()
                        .any(|field| paths_overlap(field, &transfer.dst.path))
                }),
            TransferRoot::Return => field_facts
                .contract_output
                .iter()
                .chain(field_facts.touched_output.iter())
                .any(|field| paths_overlap(field, &transfer.dst.path)),
        };
        if relevant_dst {
            propagated[src_index].insert(transfer.src.path.clone());
        }
    }

    propagated
}

fn normalize_field_set(fields: &mut FieldSet) {
    let normalized = fields
        .iter()
        .filter(|path| {
            !fields
                .iter()
                .any(|other| path.len() < other.len() && other.starts_with(path))
        })
        .cloned()
        .collect();
    *fields = normalized;
}

fn explicit_unsafe_block_spans<'tcx>(def_id: DefId, tcx: TyCtxt<'tcx>) -> Vec<Span> {
    let Some(local_def_id) = def_id.as_local() else {
        return Vec::new();
    };
    let Some(body) = tcx.hir_maybe_body_owned_by(local_def_id) else {
        return Vec::new();
    };
    let mut collector = ExplicitUnsafeSpanCollector { spans: Vec::new() };
    collector.visit_body(body);
    collector.spans
}

fn derive_unsafe_summary<'tcx>(
    body: &Body<'tcx>,
    origins: &OriginMap,
    callee_summaries: &HashMap<DefId, FunctionFieldFacts>,
    unsafe_spans: &[Span],
    tcx: TyCtxt<'tcx>,
) -> (Vec<FieldSet>, FieldSet, bool) {
    let arg_locals = body.args_iter().collect::<Vec<_>>();
    let input_count = arg_locals.len();
    let mut local_unsafe: BTreeMap<Local, FieldSet> = BTreeMap::new();
    let mut has_transitive_unsafe = false;
    let limit = body.local_decls.len().max(1) * 8;

    for _ in 0..limit {
        let mut changed = false;
        for block in body.basic_blocks.iter() {
            for statement in &block.statements {
                changed |= transfer_unsafe_statement(
                    statement,
                    body,
                    origins,
                    unsafe_spans,
                    tcx,
                    &mut local_unsafe,
                    &mut has_transitive_unsafe,
                );
            }
            if let Some(terminator) = &block.terminator {
                changed |= transfer_unsafe_terminator(
                    terminator,
                    body,
                    origins,
                    unsafe_spans,
                    UnsafeTransferContext {
                        callee_summaries,
                        tcx,
                        local_unsafe: &mut local_unsafe,
                        has_transitive_unsafe: &mut has_transitive_unsafe,
                    },
                );
            }
        }
        if !changed {
            break;
        }
    }

    let mut unsafe_inputs = vec![FieldSet::new(); input_count];
    for (index, local) in arg_locals.iter().enumerate() {
        if let Some(fields) = local_unsafe.get(local) {
            unsafe_inputs[index].extend(fields.clone());
        }
    }
    let unsafe_output = local_unsafe
        .get(&mir::RETURN_PLACE)
        .cloned()
        .unwrap_or_default();
    (unsafe_inputs, unsafe_output, has_transitive_unsafe)
}

fn derive_contract_summary<'tcx>(
    body: &Body<'tcx>,
    origins: &OriginMap,
    callee_summaries: &HashMap<DefId, FunctionFieldFacts>,
    unsafe_spans: &[Span],
    tcx: TyCtxt<'tcx>,
) -> (Vec<FieldSet>, FieldSet) {
    let arg_locals = body.args_iter().collect::<Vec<_>>();
    let input_count = arg_locals.len();
    let mut local_contract: BTreeMap<Local, FieldSet> = BTreeMap::new();
    let limit = body.local_decls.len().max(1) * 8;

    for _ in 0..limit {
        let mut changed = false;
        for block in body.basic_blocks.iter() {
            for statement in &block.statements {
                changed |= transfer_contract_statement(
                    statement,
                    body,
                    origins,
                    unsafe_spans,
                    tcx,
                    &mut local_contract,
                );
            }
            if let Some(terminator) = &block.terminator {
                changed |= transfer_contract_terminator(
                    terminator,
                    body,
                    origins,
                    unsafe_spans,
                    callee_summaries,
                    tcx,
                    &mut local_contract,
                );
            }
        }
        if !changed {
            break;
        }
    }

    let mut contract_inputs = vec![FieldSet::new(); input_count];
    for (index, local) in arg_locals.iter().enumerate() {
        if let Some(fields) = local_contract.get(local) {
            contract_inputs[index].extend(fields.clone());
        }
    }
    let contract_output = local_contract
        .get(&mir::RETURN_PLACE)
        .cloned()
        .unwrap_or_default();
    (contract_inputs, contract_output)
}

fn transfer_unsafe_statement<'tcx>(
    statement: &Statement<'tcx>,
    body: &Body<'tcx>,
    origins: &OriginMap,
    unsafe_spans: &[Span],
    tcx: TyCtxt<'tcx>,
    local_unsafe: &mut BTreeMap<Local, FieldSet>,
    has_transitive_unsafe: &mut bool,
) -> bool {
    let StatementKind::Assign(assign) = &statement.kind else {
        return false;
    };
    let (destination, rvalue) = &**assign;
    let in_unsafe = is_in_unsafe_span(statement.source_info.span, unsafe_spans);
    let mut changed = false;

    let destination_required = required_rhs_fields_for_destination(*destination, local_unsafe);
    if !destination_required.is_empty() {
        changed |= seed_unsafe_from_rvalue_with_fields(
            rvalue,
            &destination_required,
            body,
            origins,
            local_unsafe,
        );
    }

    if in_unsafe {
        let contract = extract_statement_contract(statement, body, tcx);
        if contract.detected {
            *has_transitive_unsafe = true;
            for place in contract.relevant_places {
                changed |=
                    seed_unsafe_from_place(place, body, origins, local_unsafe, &root_field_set());
            }
            for operand in contract.relevant_operands {
                changed |= seed_unsafe_from_operand(&operand, body, origins, local_unsafe);
            }
            if contract.mark_destination_output {
                changed |= assign_unsafe_to_place(
                    *destination,
                    &root_field_set(),
                    body,
                    origins,
                    local_unsafe,
                );
            }
        }
    }

    let mut rhs_fields = rvalue_unsafe_fields(rvalue, body, local_unsafe);
    if in_unsafe
        && rhs_fields.is_empty()
        && extract_statement_contract(statement, body, tcx).detected
    {
        rhs_fields = root_field_set();
    }
    changed |= assign_unsafe_to_place(*destination, &rhs_fields, body, origins, local_unsafe);
    changed
}

fn transfer_contract_statement<'tcx>(
    statement: &Statement<'tcx>,
    body: &Body<'tcx>,
    origins: &OriginMap,
    unsafe_spans: &[Span],
    tcx: TyCtxt<'tcx>,
    local_contract: &mut BTreeMap<Local, FieldSet>,
) -> bool {
    let StatementKind::Assign(assign) = &statement.kind else {
        return false;
    };
    let (destination, rvalue) = &**assign;
    let in_unsafe = is_in_unsafe_span(statement.source_info.span, unsafe_spans);
    let mut changed = false;

    let destination_required = required_rhs_fields_for_destination(*destination, local_contract);
    if !destination_required.is_empty() {
        changed |= seed_contract_from_rvalue_with_fields(
            rvalue,
            &destination_required,
            body,
            origins,
            local_contract,
        );
    }

    if in_unsafe {
        let contract = extract_statement_contract(statement, body, tcx);
        if contract.detected {
            for place in contract.relevant_places {
                changed |= seed_contract_from_place(
                    place,
                    body,
                    origins,
                    local_contract,
                    &root_field_set(),
                );
            }
            for operand in contract.relevant_operands {
                changed |= seed_contract_from_operand(
                    &operand,
                    body,
                    origins,
                    local_contract,
                    &root_field_set(),
                );
            }
            if contract.mark_destination_output {
                changed |= assign_contract_to_place(
                    *destination,
                    &root_field_set(),
                    body,
                    origins,
                    local_contract,
                );
            }
        }
    }

    let rhs_fields = rvalue_contract_fields(rvalue, body, local_contract);
    changed |= assign_contract_to_place(*destination, &rhs_fields, body, origins, local_contract);
    changed
}

fn transfer_unsafe_terminator<'tcx>(
    terminator: &Terminator<'tcx>,
    body: &Body<'tcx>,
    origins: &OriginMap,
    unsafe_spans: &[Span],
    context: UnsafeTransferContext<'_, 'tcx>,
) -> bool {
    let TerminatorKind::Call {
        func,
        args,
        destination,
        ..
    } = &terminator.kind
    else {
        return false;
    };
    let in_unsafe = is_in_unsafe_span(terminator.source_info.span, unsafe_spans);
    let mut changed = false;

    if let Some(callee) = callee_def_id_from_operand(func, body, context.tcx) {
        if in_unsafe {
            let contract = extract_call_contract(terminator, body, context.tcx, callee);
            if contract.detected {
                *context.has_transitive_unsafe = true;
                for operand in contract.relevant_operands {
                    changed |=
                        seed_unsafe_from_operand(&operand, body, origins, context.local_unsafe);
                }
                for place in contract.relevant_places {
                    changed |= seed_unsafe_from_place(
                        place,
                        body,
                        origins,
                        context.local_unsafe,
                        &root_field_set(),
                    );
                }
                if contract.mark_destination_output {
                    changed |= assign_unsafe_to_place(
                        *destination,
                        &root_field_set(),
                        body,
                        origins,
                        context.local_unsafe,
                    );
                }
            }
        }
        if let Some(callee_facts) = context.callee_summaries.get(&callee) {
            if callee_facts.has_transitive_unsafe {
                *context.has_transitive_unsafe = true;
            }
            for (index, arg) in args.iter().enumerate() {
                let Some(fields) = callee_facts.unsafe_inputs.get(index) else {
                    continue;
                };
                changed |= assign_unsafe_from_operand_to_place(
                    &arg.node,
                    fields,
                    body,
                    origins,
                    context.local_unsafe,
                );
            }
            changed |= assign_unsafe_to_place(
                *destination,
                &callee_facts.unsafe_output,
                body,
                origins,
                context.local_unsafe,
            );
        }
    }

    changed
}

struct UnsafeTransferContext<'a, 'tcx> {
    callee_summaries: &'a HashMap<DefId, FunctionFieldFacts>,
    tcx: TyCtxt<'tcx>,
    local_unsafe: &'a mut BTreeMap<Local, FieldSet>,
    has_transitive_unsafe: &'a mut bool,
}

fn transfer_contract_terminator<'tcx>(
    terminator: &Terminator<'tcx>,
    body: &Body<'tcx>,
    origins: &OriginMap,
    unsafe_spans: &[Span],
    callee_summaries: &HashMap<DefId, FunctionFieldFacts>,
    tcx: TyCtxt<'tcx>,
    local_contract: &mut BTreeMap<Local, FieldSet>,
) -> bool {
    let TerminatorKind::Call {
        func, destination, ..
    } = &terminator.kind
    else {
        return false;
    };
    let in_unsafe = is_in_unsafe_span(terminator.source_info.span, unsafe_spans);
    let mut changed = false;

    if let Some(callee) = callee_def_id_from_operand(func, body, tcx) {
        if in_unsafe {
            let contract = extract_call_contract(terminator, body, tcx, callee);
            if contract.detected {
                for operand in contract.relevant_operands {
                    changed |= seed_contract_from_operand(
                        &operand,
                        body,
                        origins,
                        local_contract,
                        &root_field_set(),
                    );
                }
                for place in contract.relevant_places {
                    changed |= seed_contract_from_place(
                        place,
                        body,
                        origins,
                        local_contract,
                        &root_field_set(),
                    );
                }
                if contract.mark_destination_output {
                    changed |= assign_contract_to_place(
                        *destination,
                        &root_field_set(),
                        body,
                        origins,
                        local_contract,
                    );
                }
            }
        }

        if let Some(callee_facts) = callee_summaries.get(&callee) {
            for (index, arg) in terminator_call_args(terminator).iter().enumerate() {
                let Some(fields) = callee_facts.contract_inputs.get(index) else {
                    continue;
                };
                changed |=
                    seed_contract_from_operand(&arg.node, body, origins, local_contract, fields);
            }
            changed |= assign_contract_to_place(
                *destination,
                &callee_facts.contract_output,
                body,
                origins,
                local_contract,
            );
        }
    }

    changed
}

fn seed_unsafe_from_rvalue_with_fields<'tcx>(
    rvalue: &Rvalue<'tcx>,
    fields: &FieldSet,
    body: &Body<'tcx>,
    origins: &OriginMap,
    local_unsafe: &mut BTreeMap<Local, FieldSet>,
) -> bool {
    if fields.is_empty() {
        return false;
    }

    match rvalue {
        Rvalue::Use(operand)
        | Rvalue::Cast(_, operand, _)
        | Rvalue::UnaryOp(_, operand)
        | Rvalue::WrapUnsafeBinder(operand, _)
        | Rvalue::Repeat(operand, _) => {
            seed_unsafe_from_operand_with_fields(operand, fields, body, origins, local_unsafe)
        }
        Rvalue::Ref(_, _, place)
        | Rvalue::RawPtr(_, place)
        | Rvalue::CopyForDeref(place)
        | Rvalue::Discriminant(place) => {
            seed_unsafe_from_place(*place, body, origins, local_unsafe, fields)
        }
        Rvalue::BinaryOp(_, operands) => {
            let (lhs, rhs) = &**operands;
            seed_unsafe_from_operand_with_fields(lhs, fields, body, origins, local_unsafe)
                | seed_unsafe_from_operand_with_fields(rhs, fields, body, origins, local_unsafe)
        }
        Rvalue::Aggregate(_, operands) => {
            let mut changed = false;
            if fields.contains(&Vec::new()) {
                for operand in operands {
                    changed |= seed_unsafe_from_operand_with_fields(
                        operand,
                        &root_field_set(),
                        body,
                        origins,
                        local_unsafe,
                    );
                }
            }
            for (index, operand) in operands.iter().enumerate() {
                let projected = project_aggregate_fields(fields, index as u32);
                if projected.is_empty() {
                    continue;
                }
                changed |= seed_unsafe_from_operand_with_fields(
                    operand,
                    &projected,
                    body,
                    origins,
                    local_unsafe,
                );
            }
            changed
        }
        Rvalue::ThreadLocalRef(_) => false,
    }
}

fn seed_unsafe_from_operand<'tcx>(
    operand: &Operand<'tcx>,
    body: &Body<'tcx>,
    origins: &OriginMap,
    local_unsafe: &mut BTreeMap<Local, FieldSet>,
) -> bool {
    seed_unsafe_from_operand_with_fields(operand, &root_field_set(), body, origins, local_unsafe)
}

fn seed_unsafe_from_operand_with_fields<'tcx>(
    operand: &Operand<'tcx>,
    fields: &FieldSet,
    body: &Body<'tcx>,
    origins: &OriginMap,
    local_unsafe: &mut BTreeMap<Local, FieldSet>,
) -> bool {
    if fields.is_empty() {
        return false;
    }

    match operand {
        Operand::Copy(place) | Operand::Move(place) => {
            seed_unsafe_from_place(*place, body, origins, local_unsafe, fields)
        }
        Operand::Constant(..) | Operand::RuntimeChecks(..) => false,
    }
}

fn seed_unsafe_from_place<'tcx>(
    place: Place<'tcx>,
    body: &Body<'tcx>,
    origins: &OriginMap,
    local_unsafe: &mut BTreeMap<Local, FieldSet>,
    fields: &FieldSet,
) -> bool {
    let mut changed = false;
    for origin in place_origins(place, body, origins) {
        changed |= assign_fields_to_interface_root(origin, fields, local_unsafe);
    }
    changed
}

fn seed_contract_from_rvalue_with_fields<'tcx>(
    rvalue: &Rvalue<'tcx>,
    fields: &FieldSet,
    body: &Body<'tcx>,
    origins: &OriginMap,
    local_contract: &mut BTreeMap<Local, FieldSet>,
) -> bool {
    if fields.is_empty() {
        return false;
    }

    match rvalue {
        Rvalue::Use(operand)
        | Rvalue::Cast(_, operand, _)
        | Rvalue::UnaryOp(_, operand)
        | Rvalue::WrapUnsafeBinder(operand, _)
        | Rvalue::Repeat(operand, _) => {
            seed_contract_from_operand(operand, body, origins, local_contract, fields)
        }
        Rvalue::Ref(_, _, place)
        | Rvalue::RawPtr(_, place)
        | Rvalue::CopyForDeref(place)
        | Rvalue::Discriminant(place) => {
            seed_contract_from_place(*place, body, origins, local_contract, fields)
        }
        Rvalue::BinaryOp(_, operands) => {
            let (lhs, rhs) = &**operands;
            seed_contract_from_operand(lhs, body, origins, local_contract, fields)
                | seed_contract_from_operand(rhs, body, origins, local_contract, fields)
        }
        Rvalue::Aggregate(_, operands) => {
            let mut changed = false;
            if fields.contains(&Vec::new()) {
                for operand in operands {
                    changed |= seed_contract_from_operand(
                        operand,
                        body,
                        origins,
                        local_contract,
                        &root_field_set(),
                    );
                }
            }
            for (index, operand) in operands.iter().enumerate() {
                let projected = project_aggregate_fields(fields, index as u32);
                if projected.is_empty() {
                    continue;
                }
                changed |=
                    seed_contract_from_operand(operand, body, origins, local_contract, &projected);
            }
            changed
        }
        Rvalue::ThreadLocalRef(_) => false,
    }
}

fn seed_contract_from_operand<'tcx>(
    operand: &Operand<'tcx>,
    body: &Body<'tcx>,
    origins: &OriginMap,
    local_contract: &mut BTreeMap<Local, FieldSet>,
    fields: &FieldSet,
) -> bool {
    if fields.is_empty() {
        return false;
    }
    match operand {
        Operand::Copy(place) | Operand::Move(place) => {
            seed_contract_from_place(*place, body, origins, local_contract, fields)
        }
        Operand::Constant(..) | Operand::RuntimeChecks(..) => false,
    }
}

fn seed_contract_from_place<'tcx>(
    place: Place<'tcx>,
    body: &Body<'tcx>,
    origins: &OriginMap,
    local_contract: &mut BTreeMap<Local, FieldSet>,
    fields: &FieldSet,
) -> bool {
    let mut changed = false;
    for origin in place_origins(place, body, origins) {
        changed |= assign_fields_to_interface_root(origin, fields, local_contract);
    }
    changed
}

fn assign_unsafe_from_operand_to_place<'tcx>(
    operand: &Operand<'tcx>,
    fields: &FieldSet,
    body: &Body<'tcx>,
    origins: &OriginMap,
    local_unsafe: &mut BTreeMap<Local, FieldSet>,
) -> bool {
    match operand {
        Operand::Copy(place) | Operand::Move(place) => {
            assign_unsafe_to_place(*place, fields, body, origins, local_unsafe)
        }
        Operand::Constant(..) | Operand::RuntimeChecks(..) => false,
    }
}

fn assign_unsafe_to_place<'tcx>(
    place: Place<'tcx>,
    fields: &FieldSet,
    body: &Body<'tcx>,
    origins: &OriginMap,
    local_unsafe: &mut BTreeMap<Local, FieldSet>,
) -> bool {
    if fields.is_empty() {
        return false;
    }

    let suffix = place_suffix(place);
    let mut changed = extend_field_set(
        local_unsafe.entry(place.local).or_default(),
        fields.iter().map(|field| combine_paths(&suffix, field)),
    );
    if !is_interface_root_local(place.local, body) {
        for origin in place_origins(place, body, origins) {
            changed |= assign_fields_to_interface_root(origin, fields, local_unsafe);
        }
    }
    changed
}

fn assign_contract_to_place<'tcx>(
    place: Place<'tcx>,
    fields: &FieldSet,
    body: &Body<'tcx>,
    origins: &OriginMap,
    local_contract: &mut BTreeMap<Local, FieldSet>,
) -> bool {
    if fields.is_empty() {
        return false;
    }

    let suffix = place_suffix(place);
    let mut changed = extend_field_set(
        local_contract.entry(place.local).or_default(),
        fields.iter().map(|field| combine_paths(&suffix, field)),
    );
    if !is_interface_root_local(place.local, body) {
        for origin in place_origins(place, body, origins) {
            changed |= assign_fields_to_interface_root(origin, fields, local_contract);
        }
    }
    changed
}

fn assign_fields_to_interface_root(
    origin: InterfacePlace,
    fields: &FieldSet,
    local_unsafe: &mut BTreeMap<Local, FieldSet>,
) -> bool {
    let local = match origin.root {
        InterfaceRoot::Param(index) => Local::from_usize(index + 1),
        InterfaceRoot::Return => mir::RETURN_PLACE,
    };
    extend_field_set(
        local_unsafe.entry(local).or_default(),
        fields
            .iter()
            .map(|field| combine_paths(&origin.path, field)),
    )
}

fn extend_field_set(set: &mut FieldSet, paths: impl IntoIterator<Item = FieldPath>) -> bool {
    let before = set.len();
    set.extend(paths);
    set.len() != before
}

fn rvalue_unsafe_fields<'tcx>(
    rvalue: &Rvalue<'tcx>,
    body: &Body<'tcx>,
    local_unsafe: &BTreeMap<Local, FieldSet>,
) -> FieldSet {
    match rvalue {
        Rvalue::Use(operand)
        | Rvalue::Cast(_, operand, _)
        | Rvalue::UnaryOp(_, operand)
        | Rvalue::WrapUnsafeBinder(operand, _) => {
            operand_unsafe_fields(operand, body, local_unsafe)
        }
        Rvalue::Repeat(operand, _) => operand_unsafe_fields(operand, body, local_unsafe),
        Rvalue::Ref(_, _, place)
        | Rvalue::RawPtr(_, place)
        | Rvalue::CopyForDeref(place)
        | Rvalue::Discriminant(place) => place_unsafe_fields(*place, local_unsafe),
        Rvalue::BinaryOp(_, operands) => {
            let (lhs, rhs) = &**operands;
            let mut fields = operand_unsafe_fields(lhs, body, local_unsafe);
            fields.extend(operand_unsafe_fields(rhs, body, local_unsafe));
            fields
        }
        Rvalue::Aggregate(_, operands) => {
            operands.iter().fold(FieldSet::new(), |mut acc, operand| {
                acc.extend(operand_unsafe_fields(operand, body, local_unsafe));
                acc
            })
        }
        Rvalue::ThreadLocalRef(_) => FieldSet::new(),
    }
}

fn operand_unsafe_fields<'tcx>(
    operand: &Operand<'tcx>,
    _body: &Body<'tcx>,
    local_unsafe: &BTreeMap<Local, FieldSet>,
) -> FieldSet {
    match operand {
        Operand::Copy(place) | Operand::Move(place) => place_unsafe_fields(*place, local_unsafe),
        Operand::Constant(..) | Operand::RuntimeChecks(..) => FieldSet::new(),
    }
}

fn rvalue_contract_fields<'tcx>(
    rvalue: &Rvalue<'tcx>,
    body: &Body<'tcx>,
    local_contract: &BTreeMap<Local, FieldSet>,
) -> FieldSet {
    match rvalue {
        Rvalue::Use(operand)
        | Rvalue::Cast(_, operand, _)
        | Rvalue::UnaryOp(_, operand)
        | Rvalue::WrapUnsafeBinder(operand, _) => operand_contract_fields(operand, local_contract),
        Rvalue::Repeat(operand, _) => operand_contract_fields(operand, local_contract),
        Rvalue::Ref(_, _, place)
        | Rvalue::RawPtr(_, place)
        | Rvalue::CopyForDeref(place)
        | Rvalue::Discriminant(place) => place_contract_fields(*place, local_contract),
        Rvalue::BinaryOp(_, operands) => {
            let (lhs, rhs) = &**operands;
            let mut fields = operand_contract_fields(lhs, local_contract);
            fields.extend(operand_contract_fields(rhs, local_contract));
            fields
        }
        Rvalue::Aggregate(_, operands) => {
            operands.iter().fold(FieldSet::new(), |mut acc, operand| {
                acc.extend(operand_contract_fields(operand, local_contract));
                acc
            })
        }
        Rvalue::ThreadLocalRef(_) => {
            let _ = body;
            FieldSet::new()
        }
    }
}

fn operand_contract_fields(
    operand: &Operand<'_>,
    local_contract: &BTreeMap<Local, FieldSet>,
) -> FieldSet {
    match operand {
        Operand::Copy(place) | Operand::Move(place) => {
            place_contract_fields(*place, local_contract)
        }
        Operand::Constant(..) | Operand::RuntimeChecks(..) => FieldSet::new(),
    }
}

fn place_contract_fields(place: Place<'_>, local_contract: &BTreeMap<Local, FieldSet>) -> FieldSet {
    let suffix = place_suffix(place);
    let Some(fields) = local_contract.get(&place.local) else {
        return FieldSet::new();
    };
    let mut projected = FieldSet::new();
    for field in fields {
        if field.starts_with(&suffix) {
            projected.insert(field[suffix.len()..].to_vec());
        } else if suffix.starts_with(field) {
            projected.insert(Vec::new());
        }
    }
    projected
}

fn place_unsafe_fields(place: Place<'_>, local_unsafe: &BTreeMap<Local, FieldSet>) -> FieldSet {
    let suffix = place_suffix(place);
    let Some(fields) = local_unsafe.get(&place.local) else {
        return FieldSet::new();
    };
    let mut projected = FieldSet::new();
    for field in fields {
        if field.starts_with(&suffix) {
            projected.insert(field[suffix.len()..].to_vec());
        } else if suffix.starts_with(field) {
            projected.insert(Vec::new());
        }
    }
    projected
}

fn required_rhs_fields_for_destination(
    destination: Place<'_>,
    local_unsafe: &BTreeMap<Local, FieldSet>,
) -> FieldSet {
    let suffix = place_suffix(destination);
    let Some(fields) = local_unsafe.get(&destination.local) else {
        return FieldSet::new();
    };
    let mut required = FieldSet::new();
    for field in fields {
        if field.starts_with(&suffix) {
            required.insert(field[suffix.len()..].to_vec());
        } else if suffix.starts_with(field) {
            required.insert(Vec::new());
        }
    }
    required
}

fn extract_statement_contract<'tcx>(
    statement: &Statement<'tcx>,
    body: &Body<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> UnsafeContractExtraction<'tcx> {
    let StatementKind::Assign(assign) = &statement.kind else {
        return UnsafeContractExtraction {
            relevant_places: Vec::new(),
            relevant_operands: Vec::new(),
            mark_destination_output: false,
            detected: false,
        };
    };
    let (destination, rvalue) = &**assign;

    let mut relevant_places = Vec::new();
    if place_requires_contract(*destination, body, tcx) {
        relevant_places.push(*destination);
    }
    collect_contract_places_from_rvalue(rvalue, body, tcx, &mut relevant_places);

    UnsafeContractExtraction {
        detected: !relevant_places.is_empty(),
        relevant_places,
        relevant_operands: Vec::new(),
        mark_destination_output: false,
    }
}

fn extract_call_contract<'tcx>(
    terminator: &Terminator<'tcx>,
    body: &Body<'tcx>,
    tcx: TyCtxt<'tcx>,
    callee: DefId,
) -> UnsafeContractExtraction<'tcx> {
    let TerminatorKind::Call { args, .. } = &terminator.kind else {
        return UnsafeContractExtraction {
            relevant_places: Vec::new(),
            relevant_operands: Vec::new(),
            mark_destination_output: false,
            detected: false,
        };
    };

    let sig = tcx.fn_sig(callee).instantiate_identity();
    let is_unsafe_call = sig.safety().is_unsafe();
    let relevant_arg_indices = if is_unsafe_call {
        unsafe_call_relevant_arg_indices(callee, args.len(), tcx)
            .unwrap_or_else(|| (0..args.len()).collect::<Vec<_>>())
    } else {
        Vec::new()
    };
    let relevant_operands = relevant_arg_indices
        .iter()
        .filter_map(|index| args.get(*index).map(|arg| arg.node.clone()))
        .collect::<Vec<_>>();

    let mut relevant_places = Vec::new();
    for index in &relevant_arg_indices {
        if let Some(arg) = args.get(*index) {
            collect_contract_places_from_operand(&arg.node, body, tcx, &mut relevant_places);
        }
    }

    UnsafeContractExtraction {
        detected: is_unsafe_call || !relevant_places.is_empty(),
        relevant_places,
        relevant_operands,
        mark_destination_output: is_unsafe_call,
    }
}

fn unsafe_call_relevant_arg_indices<'tcx>(
    callee: DefId,
    arg_count: usize,
    tcx: TyCtxt<'tcx>,
) -> Option<Vec<usize>> {
    let arg_names = callee_param_names(callee, tcx)?;
    if unsafe_call_requires_all_args(&arg_names) {
        return Some((0..arg_count).collect());
    }
    unsafe_call_relevant_param_names(callee, tcx)
        .and_then(|param_names| map_param_names_to_indices(&arg_names, &param_names))
}

fn unsafe_call_relevant_param_names<'tcx>(callee: DefId, tcx: TyCtxt<'tcx>) -> Option<Vec<String>> {
    std_annotated_param_names(callee, tcx).or_else(|| unsafe_safety_doc_param_names(callee, tcx))
}

fn std_annotated_param_names<'tcx>(callee: DefId, tcx: TyCtxt<'tcx>) -> Option<Vec<String>> {
    let def_path = tcx.def_path_str(callee);
    std_annotated_params()
        .get(&def_path)
        .filter(|entry| !entry.params.is_empty())
        .map(|entry| entry.params.clone())
}

fn unsafe_safety_doc_param_names<'tcx>(callee: DefId, tcx: TyCtxt<'tcx>) -> Option<Vec<String>> {
    let section = unsafe_safety_section_text(callee, tcx)?;
    let arg_names = callee_param_names(callee, tcx)?;
    let matched = doc_mentioned_params(&section, &arg_names);
    (!matched.is_empty()).then_some(matched)
}

fn map_param_names_to_indices(
    arg_names: &[String],
    relevant_param_names: &[String],
) -> Option<Vec<usize>> {
    if arg_names.is_empty() {
        return None;
    }

    let indices = arg_names
        .iter()
        .enumerate()
        .filter_map(|(index, arg_name)| relevant_param_names.contains(arg_name).then_some(index))
        .collect::<Vec<_>>();
    (!indices.is_empty()).then_some(indices)
}

fn unsafe_call_requires_all_args(arg_names: &[String]) -> bool {
    arg_names.first().is_some_and(|name| name == "self")
}

fn callee_param_names<'tcx>(callee: DefId, tcx: TyCtxt<'tcx>) -> Option<Vec<String>> {
    let span = tcx.def_span(callee);
    let snippet = tcx.sess.source_map().span_to_snippet(span).ok()?;
    let item_name_symbol = tcx.item_name(callee);
    let item_name = item_name_symbol.as_str();
    let params = function_param_list_from_snippet(&snippet, item_name)?;
    let names = params
        .into_iter()
        .filter_map(|param| function_param_name(&param))
        .collect::<Vec<_>>();
    (!names.is_empty()).then_some(names)
}

fn function_param_list_from_snippet(snippet: &str, item_name: &str) -> Option<Vec<String>> {
    let fn_start = find_fn_signature_start(snippet, item_name)?;
    let start = snippet[fn_start..]
        .find('(')
        .map(|offset| fn_start + offset)?;
    let mut depth = 0usize;
    let mut end = None;
    for (offset, ch) in snippet[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = Some(start + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    split_top_level_commas(&snippet[start + 1..end])
}

fn find_fn_signature_start(snippet: &str, item_name: &str) -> Option<usize> {
    snippet.match_indices("fn").fold(None, |best, (index, _)| {
        fn_signature_name_end(snippet, index, item_name).or(best)
    })
}

fn fn_signature_name_end(snippet: &str, fn_index: usize, item_name: &str) -> Option<usize> {
    let before = snippet[..fn_index].chars().next_back();
    let after = snippet[fn_index + 2..].chars().next();
    let before_ok = before.is_none_or(|ch| !(ch.is_ascii_alphanumeric() || ch == '_'));
    let after_ok = after.is_none_or(|ch| !(ch.is_ascii_alphanumeric() || ch == '_'));
    if !(before_ok && after_ok) {
        return None;
    }

    let after_fn = &snippet[fn_index + 2..];
    let name_start_offset = after_fn
        .char_indices()
        .find_map(|(offset, ch)| (!ch.is_whitespace()).then_some(offset))?;
    let after_name_start = &after_fn[name_start_offset..];
    let name_end_offset = after_name_start
        .char_indices()
        .find_map(|(offset, ch)| (!(ch.is_ascii_alphanumeric() || ch == '_')).then_some(offset))
        .unwrap_or(after_name_start.len());
    let name = &after_name_start[..name_end_offset];
    if name != item_name {
        return None;
    }

    let remainder = &after_name_start[name_end_offset..];
    let next_non_ws = remainder
        .char_indices()
        .find_map(|(offset, ch)| (!ch.is_whitespace()).then_some((offset, ch)))?;
    matches!(next_non_ws.1, '(' | '<').then_some(fn_index + 2)
}

fn split_top_level_commas(text: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut angle = 0usize;
    let mut paren = 0usize;
    let mut bracket = 0usize;
    let mut brace = 0usize;
    for ch in text.chars() {
        match ch {
            '<' => angle += 1,
            '>' => angle = angle.saturating_sub(1),
            '(' => paren += 1,
            ')' => paren = paren.saturating_sub(1),
            '[' => bracket += 1,
            ']' => bracket = bracket.saturating_sub(1),
            '{' => brace += 1,
            '}' => brace = brace.saturating_sub(1),
            ',' if angle == 0 && paren == 0 && bracket == 0 && brace == 0 => {
                parts.push(current.trim().to_string());
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(ch);
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    Some(parts)
}

fn function_param_name(param: &str) -> Option<String> {
    let param = param.trim();
    if param.is_empty() {
        return None;
    }
    if param == "self"
        || param == "&self"
        || param == "&mut self"
        || param.ends_with(" self")
        || param.ends_with(" mut self")
    {
        return Some("self".to_string());
    }
    let (pattern, _) = param.split_once(':')?;
    let name = pattern
        .split_whitespace()
        .last()?
        .trim_start_matches('&')
        .trim_start_matches("mut ")
        .to_string();
    (!name.is_empty()).then_some(name)
}

fn unsafe_safety_section_text<'tcx>(callee: DefId, tcx: TyCtxt<'tcx>) -> Option<String> {
    #[allow(deprecated)]
    let docs = tcx
        .get_attrs(callee, sym::doc)
        .filter_map(|attr| attr.doc_str().map(|symbol| symbol.as_str().to_string()))
        .collect::<Vec<_>>();
    if docs.is_empty() {
        return None;
    }

    extract_safety_section_from_docs(&docs.join("\n"))
}

fn extract_safety_section_from_docs(docs: &str) -> Option<String> {
    let mut section_lines = Vec::new();
    let mut in_safety_section = false;
    for line in docs.lines() {
        let trimmed = line.trim();
        if is_markdown_safety_heading(trimmed) {
            in_safety_section = true;
            continue;
        }
        if in_safety_section && is_markdown_heading(trimmed) {
            break;
        }
        if in_safety_section {
            section_lines.push(trimmed);
        }
    }

    let section = section_lines.join("\n").trim().to_string();
    (!section.is_empty()).then_some(section)
}

fn is_markdown_safety_heading(line: &str) -> bool {
    markdown_heading_text(line).is_some_and(|heading| heading.eq_ignore_ascii_case("safety"))
}

fn is_markdown_heading(line: &str) -> bool {
    markdown_heading_text(line).is_some()
}

fn markdown_heading_text(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let heading = trimmed.strip_prefix('#')?;
    Some(heading.trim_start_matches('#').trim())
}

fn doc_section_mentions_param(section: &str, param_name: &str) -> bool {
    section
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .any(|token| token == param_name)
        || section.contains(&format!("`{param_name}`"))
}

fn doc_mentioned_params(section: &str, arg_names: &[String]) -> Vec<String> {
    arg_names
        .iter()
        .filter(|name| doc_section_mentions_param(section, name))
        .cloned()
        .collect()
}

fn collect_contract_places_from_rvalue<'tcx>(
    rvalue: &Rvalue<'tcx>,
    body: &Body<'tcx>,
    tcx: TyCtxt<'tcx>,
    out: &mut Vec<Place<'tcx>>,
) {
    match rvalue {
        Rvalue::Use(operand)
        | Rvalue::Cast(_, operand, _)
        | Rvalue::UnaryOp(_, operand)
        | Rvalue::WrapUnsafeBinder(operand, _)
        | Rvalue::Repeat(operand, _) => {
            collect_contract_places_from_operand(operand, body, tcx, out)
        }
        Rvalue::Ref(_, _, place)
        | Rvalue::RawPtr(_, place)
        | Rvalue::CopyForDeref(place)
        | Rvalue::Discriminant(place) => {
            if place_requires_contract(*place, body, tcx) {
                out.push(*place);
            }
        }
        Rvalue::BinaryOp(_, operands) => {
            let (lhs, rhs) = &**operands;
            collect_contract_places_from_operand(lhs, body, tcx, out);
            collect_contract_places_from_operand(rhs, body, tcx, out);
        }
        Rvalue::Aggregate(_, operands) => {
            for operand in operands {
                collect_contract_places_from_operand(operand, body, tcx, out);
            }
        }
        Rvalue::ThreadLocalRef(_) => {}
    }
}

fn collect_contract_places_from_operand<'tcx>(
    operand: &Operand<'tcx>,
    body: &Body<'tcx>,
    tcx: TyCtxt<'tcx>,
    out: &mut Vec<Place<'tcx>>,
) {
    if let Operand::Copy(place) | Operand::Move(place) = operand
        && place_requires_contract(*place, body, tcx)
    {
        out.push(*place);
    }
}

fn place_requires_contract<'tcx>(place: Place<'tcx>, body: &Body<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    place_has_raw_pointer_deref(place, body) || place_accesses_union_field(place, body, tcx)
}

fn place_has_raw_pointer_deref<'tcx>(place: Place<'tcx>, body: &Body<'tcx>) -> bool {
    let mut current_ty = body.local_decls[place.local].ty;
    for elem in place.projection.iter() {
        match elem {
            ProjectionElem::Deref => {
                if matches!(current_ty.kind(), ty::TyKind::RawPtr(_, _)) {
                    return true;
                }
                current_ty = match current_ty.kind() {
                    ty::TyKind::Ref(_, inner, _) | ty::TyKind::RawPtr(inner, _) => *inner,
                    _ => return false,
                };
            }
            ProjectionElem::Field(_, field_ty) => {
                current_ty = field_ty;
            }
            ProjectionElem::Downcast(_, _) => {}
            _ => break,
        }
    }
    false
}

fn place_accesses_union_field<'tcx>(
    place: Place<'tcx>,
    body: &Body<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    let mut current_ty = body.local_decls[place.local].ty;
    for elem in place.projection.iter() {
        match elem {
            ProjectionElem::Deref => {
                current_ty = match current_ty.kind() {
                    ty::TyKind::Ref(_, inner, _) | ty::TyKind::RawPtr(inner, _) => *inner,
                    _ => return false,
                };
            }
            ProjectionElem::Field(_, field_ty) => {
                let erased = tcx.erase_and_anonymize_regions(current_ty);
                if let ty::TyKind::Adt(def, _) = erased.kind()
                    && def.is_union()
                {
                    return true;
                }
                current_ty = field_ty;
            }
            ProjectionElem::Downcast(_, _) => {}
            _ => break,
        }
    }
    false
}

fn project_aggregate_fields(fields: &FieldSet, index: u32) -> FieldSet {
    let mut projected = FieldSet::new();
    for field in fields {
        if field.is_empty() {
            projected.insert(Vec::new());
            continue;
        }
        if field[0] == index {
            projected.insert(field[1..].to_vec());
        }
    }
    projected
}

fn root_field_set() -> FieldSet {
    FieldSet::from([Vec::new()])
}

fn is_interface_root_local(local: Local, body: &Body<'_>) -> bool {
    local == mir::RETURN_PLACE || body.args_iter().any(|arg| arg == local)
}

fn is_in_unsafe_span(span: Span, unsafe_spans: &[Span]) -> bool {
    unsafe_spans
        .iter()
        .any(|unsafe_span| unsafe_span.contains(span))
}

fn callee_def_id_from_operand<'tcx>(
    operand: &Operand<'tcx>,
    body: &Body<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> Option<DefId> {
    let ty = operand.ty(&body.local_decls, tcx);
    match ty.kind() {
        ty::TyKind::FnDef(def_id, _) => Some(*def_id),
        _ => None,
    }
}

fn terminator_call_args<'tcx>(
    terminator: &Terminator<'tcx>,
) -> Vec<rustc_span::Spanned<Operand<'tcx>>> {
    let TerminatorKind::Call { args, .. } = &terminator.kind else {
        return Vec::new();
    };
    args.to_vec()
}

struct ExplicitUnsafeSpanCollector {
    spans: Vec<Span>,
}

impl<'hir> Visitor<'hir> for ExplicitUnsafeSpanCollector {
    fn visit_block(&mut self, block: &'hir rustc_hir::Block<'hir>) -> Self::Result {
        if matches!(
            block.rules,
            rustc_hir::BlockCheckMode::UnsafeBlock(rustc_hir::UnsafeSource::UserProvided)
        ) {
            self.spans.push(block.span);
        }
        rustc_hir::intravisit::walk_block(self, block);
    }
}

/// MIR visitor that accumulates field-sensitive dataflow facts before unsafe
/// propagation extraction.
struct DataflowFactsCollector<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    body: &'a Body<'tcx>,
    origins: &'a OriginMap,
    callee_summaries: &'a HashMap<DefId, FunctionFieldFacts>,
    facts: FunctionFieldFacts,
}

impl<'tcx> DataflowFactsCollector<'_, 'tcx> {
    fn record_origins(&mut self, origins: BTreeSet<InterfacePlace>) {
        for origin in origins {
            match origin.root {
                InterfaceRoot::Param(index) => {
                    if let Some(slot) = self.facts.touched_inputs.get_mut(index) {
                        slot.insert(origin.path);
                    }
                }
                InterfaceRoot::Return => {
                    self.facts.touched_output.insert(origin.path);
                }
            }
        }
    }

    fn record_mutated_origins(&mut self, origins: BTreeSet<InterfacePlace>) {
        for origin in origins {
            if let InterfaceRoot::Param(index) = origin.root
                && let Some(slot) = self.facts.mutated_inputs.get_mut(index)
            {
                slot.insert(origin.path);
            }
        }
    }

    fn record_mutated_fields(
        &mut self,
        origins: BTreeSet<InterfacePlace>,
        mutated_fields: &FieldSet,
    ) {
        if mutated_fields.is_empty() {
            return;
        }
        for origin in origins {
            let InterfaceRoot::Param(index) = origin.root else {
                continue;
            };
            let Some(slot) = self.facts.mutated_inputs.get_mut(index) else {
                continue;
            };
            for field in mutated_fields {
                slot.insert(combine_paths(&origin.path, field));
            }
        }
    }

    fn callee_def_id(&self, operand: &Operand<'tcx>) -> Option<DefId> {
        let ty = operand.ty(&self.body.local_decls, self.tcx);
        match ty.kind() {
            ty::TyKind::FnDef(def_id, _) => Some(*def_id),
            _ => None,
        }
    }
}

impl<'tcx> MirVisitor<'tcx> for DataflowFactsCollector<'_, 'tcx> {
    fn visit_statement(&mut self, statement: &Statement<'tcx>, location: mir::Location) {
        if let StatementKind::Assign(assign) = &statement.kind {
            let (destination, _) = &**assign;
            let origins = writeback_origins(*destination, self.body, self.origins);
            self.record_mutated_origins(origins);
        }
        self.super_statement(statement, location);
    }

    fn visit_place(&mut self, place: &Place<'tcx>, context: PlaceContext, location: mir::Location) {
        let origins = place_origins(*place, self.body, self.origins);
        self.record_origins(origins);
        if context.is_mutating_use() && !is_assignment_destination(*place, self.body, location) {
            let mutated = writeback_origins(*place, self.body, self.origins);
            self.record_mutated_origins(mutated);
        }
    }

    fn visit_terminator(&mut self, terminator: &Terminator<'tcx>, location: mir::Location) {
        if let TerminatorKind::Call { func, args, .. } = &terminator.kind
            && let Some(callee) = self.callee_def_id(func)
        {
            let arg_origins = args
                .iter()
                .map(|operand| operand_origins(&operand.node, self.body, self.origins))
                .collect();
            self.facts.callsites.push(CallSiteFacts {
                callee,
                arg_origins,
            });
            if let Some(callee_facts) = self.callee_summaries.get(&callee) {
                for (index, arg) in args.iter().enumerate() {
                    let Some(mutated_fields) = callee_facts.mutated_inputs.get(index) else {
                        continue;
                    };
                    if mutated_fields.is_empty() {
                        continue;
                    }
                    let origins = operand_origins(&arg.node, self.body, self.origins);
                    self.record_mutated_fields(origins, mutated_fields);
                }
            }
        }
        self.super_terminator(terminator, location);
    }
}

fn writeback_origins<'tcx>(
    destination: Place<'tcx>,
    body: &Body<'tcx>,
    origins: &OriginMap,
) -> BTreeSet<InterfacePlace> {
    if destination
        .projection
        .iter()
        .any(|elem| matches!(elem, ProjectionElem::Deref))
    {
        return place_origins(destination, body, origins)
            .into_iter()
            .filter(|origin| origin_is_externally_mutable_alias(origin, body))
            .collect();
    }

    BTreeSet::new()
}

fn origin_is_externally_mutable_alias(origin: &InterfacePlace, body: &Body<'_>) -> bool {
    let InterfaceRoot::Param(index) = origin.root else {
        return false;
    };
    let Some(local) = body.args_iter().nth(index) else {
        return false;
    };
    matches!(
        body.local_decls[local].ty.kind(),
        ty::TyKind::Ref(_, _, ty::Mutability::Mut) | ty::TyKind::RawPtr(_, ty::Mutability::Mut)
    )
}

fn is_assignment_destination<'tcx>(
    place: Place<'tcx>,
    body: &Body<'tcx>,
    location: mir::Location,
) -> bool {
    let Some(block) = body.basic_blocks.get(location.block) else {
        return false;
    };
    let Some(statement) = block.statements.get(location.statement_index) else {
        return false;
    };
    let StatementKind::Assign(assign) = &statement.kind else {
        return false;
    };
    let (destination, _) = &**assign;
    *destination == place
}

fn direct_api_safety<'tcx>(def_id: DefId, tcx: TyCtxt<'tcx>) -> ApiSafety {
    let Some(local_def_id) = def_id.as_local() else {
        return ApiSafety::Safe;
    };

    if tcx
        .fn_sig(def_id)
        .instantiate_identity()
        .safety()
        .is_unsafe()
    {
        return ApiSafety::UnsafeFn;
    }
    if tcx.hir_maybe_body_owned_by(local_def_id).is_none() {
        return ApiSafety::Safe;
    }

    let mut visitor = UnsafeBlockVisitor::default();
    let body = tcx
        .hir_maybe_body_owned_by(local_def_id)
        .expect("body presence was checked above");
    visitor.visit_body(body);
    if visitor.found_user_unsafe {
        ApiSafety::UnsafeBlock
    } else if visitor.found_compiler_unsafe {
        ApiSafety::BuiltinUnsafe
    } else {
        ApiSafety::Safe
    }
}

#[derive(Default)]
struct UnsafeBlockVisitor {
    found_user_unsafe: bool,
    found_compiler_unsafe: bool,
}

impl<'hir> Visitor<'hir> for UnsafeBlockVisitor {
    fn visit_block(&mut self, block: &'hir rustc_hir::Block<'hir>) -> Self::Result {
        match block.rules {
            rustc_hir::BlockCheckMode::UnsafeBlock(rustc_hir::UnsafeSource::UserProvided) => {
                self.found_user_unsafe = true;
                return;
            }
            rustc_hir::BlockCheckMode::UnsafeBlock(rustc_hir::UnsafeSource::CompilerGenerated) => {
                self.found_compiler_unsafe = true;
            }
            rustc_hir::BlockCheckMode::DefaultBlock => {}
        }

        if self.found_user_unsafe {
            return;
        }
        rustc_hir::intravisit::walk_block(self, block);
    }
}

fn collect_callable_def_ids<'tcx>(tcx: TyCtxt<'tcx>) -> Vec<DefId> {
    let mut visitor = CallableCollector {
        tcx,
        defs: HashSet::new(),
    };
    tcx.hir_visit_all_item_likes_in_crate(&mut visitor);
    for trait_item_id in tcx.hir_crate_items(()).trait_items() {
        let trait_item = tcx.hir_trait_item(trait_item_id);
        if matches!(
            trait_item.kind,
            rustc_hir::TraitItemKind::Fn(_, rustc_hir::TraitFn::Provided(_))
        ) {
            visitor.defs.insert(trait_item.owner_id.to_def_id());
        }
    }
    visitor.defs.into_iter().collect()
}

struct CallableCollector<'tcx> {
    tcx: TyCtxt<'tcx>,
    defs: HashSet<DefId>,
}

impl CallableCollector<'_> {
    fn maybe_record_fn(&mut self, fn_did: DefId) {
        let Some(local_def_id) = fn_did.as_local() else {
            return;
        };
        if self.tcx.hir_maybe_body_owned_by(local_def_id).is_none() {
            return;
        }
        self.defs.insert(fn_did);
    }
}

impl<'tcx> Visitor<'tcx> for CallableCollector<'tcx> {
    fn visit_fn<'v>(
        &mut self,
        fk: FnKind<'v>,
        _fd: &'v rustc_hir::FnDecl<'v>,
        _b: rustc_hir::BodyId,
        _span: Span,
        id: LocalDefId,
    ) -> Self::Result {
        if matches!(fk, FnKind::ItemFn(..)) {
            self.maybe_record_fn(id.to_def_id());
        }
    }

    fn visit_trait_item(&mut self, item: &'tcx rustc_hir::TraitItem<'tcx>) -> Self::Result {
        if matches!(
            item.kind,
            rustc_hir::TraitItemKind::Fn(_, rustc_hir::TraitFn::Provided(_))
        ) {
            self.maybe_record_fn(item.owner_id.to_def_id());
        }
        rustc_hir::intravisit::walk_trait_item(self, item);
    }

    fn visit_impl_item(&mut self, item: &'tcx rustc_hir::ImplItem<'tcx>) -> Self::Result {
        if matches!(item.kind, rustc_hir::ImplItemKind::Fn(..)) {
            self.maybe_record_fn(item.owner_id.to_def_id());
        }
        rustc_hir::intravisit::walk_impl_item(self, item);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        STD_ANNOTATED_PARAMS_JSON, doc_mentioned_params, extract_safety_section_from_docs,
        function_param_list_from_snippet, function_param_name, std_annotated_params,
        unsafe_call_requires_all_args,
    };

    #[test]
    fn extracts_relevant_params_from_safety_section() {
        let docs = r#"
Intro.

# Safety
`data` must point to `len` initialized bytes.
The caller must uphold the byte-count precondition.

# Panics
Never.
"#;
        let section = extract_safety_section_from_docs(docs).expect("expected safety section");
        let args = vec!["data".to_string(), "len".to_string(), "salt".to_string()];

        assert_eq!(
            doc_mentioned_params(&section, &args),
            vec!["data".to_string(), "len".to_string()]
        );
    }

    #[test]
    fn loads_std_annotated_param_contracts() {
        let params = std_annotated_params()
            .get("std::slice::from_raw_parts")
            .expect("expected std::slice::from_raw_parts entry");

        assert!(
            !std_annotated_params().is_empty(),
            "expected annotated params to load from embedded std annotation json"
        );
        assert!(STD_ANNOTATED_PARAMS_JSON.contains("std::slice::from_raw_parts"));
        assert_eq!(params.params, vec!["data".to_string(), "len".to_string()]);
    }

    #[test]
    fn parses_function_param_names_from_source_snippet() {
        let params = function_param_list_from_snippet(
            "pub unsafe fn inspect_core(data: Vec<u8>, len: usize, noise: Vec<u8>) -> usize {",
            "inspect_core",
        )
        .expect("expected params");
        let names = params
            .iter()
            .filter_map(|param| function_param_name(param))
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["data", "len", "noise"]);
    }

    #[test]
    fn ignores_doc_example_fns_before_real_signature() {
        let params = function_param_list_from_snippet(
            r#"
/// Example:
/// ```rust
/// fn demo(fake: usize) {}
/// ```
/// ```rust
/// fn inspect_core(example: bool) {}
/// ```
pub unsafe fn inspect_core(data: Vec<u8>, len: usize, noise: Vec<u8>) -> usize {
"#,
            "inspect_core",
        )
        .expect("expected params");
        let names = params
            .iter()
            .filter_map(|param| function_param_name(param))
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["data", "len", "noise"]);
    }

    #[test]
    fn receiver_bearing_unsafe_calls_keep_all_args_relevant() {
        let args = vec!["self".to_string(), "key".to_string()];
        assert!(unsafe_call_requires_all_args(&args));

        let free_fn_args = vec!["ptr".to_string(), "len".to_string()];
        assert!(!unsafe_call_requires_all_args(&free_fn_args));
    }
}
