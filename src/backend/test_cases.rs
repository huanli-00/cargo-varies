use super::common::{
    dependency_crate_ident, prepare_generated_layout, render_harness_manifest, reset_lockfile,
    sequence_module_name, write_suite_metadata,
};
use super::sequence_fn::{
    RenderedSequenceFunction, SequenceFunctionParam, SequenceFunctionRenderer,
    render_sequence_function,
};
use super::{BackendKind, BackendRenderContext, BackendValidationCommand, SuiteBackend};
use crate::cli::VariesArgs;
use crate::std_adapters::{
    is_pyo3_python_token_input, kani_seed_ty_for_input, literal_seed_values_for_input,
    needs_symbolic_seed_binding, render_value_from_symbolic_seed, seed_value_ty,
};
use crate::synth::{ArgPlan, BorrowMode, OutputAdapter, SynthesizedSuite, ValueSource};
use crate::workspace::WorkspaceTarget;
use anyhow::{Context, Result};
use rustc_data_structures::sync::par_map;
use rustc_middle::ty::{Ty, TyCtxt};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

#[derive(Clone)]
struct TestParam {
    name: String,
    ty: String,
    literals: Vec<String>,
}

struct RenderArgContext<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    graph: &'a crate::rapx_graph::ApiDependencyGraph<'tcx>,
    step_index: usize,
    params: &'a mut BTreeMap<LiteralParamKey, TestParam>,
    initialized_params: &'a mut BTreeSet<LiteralParamKey>,
    materialized_param_values: &'a mut HashMap<(usize, String), String>,
    param_use_counts: &'a mut HashMap<LiteralParamKey, usize>,
    param_total_uses: &'a HashMap<LiteralParamKey, usize>,
    lines: &'a mut Vec<String>,
}

#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct LiteralParamKey {
    source_index: usize,
    ty: String,
}

struct TestSequenceFunctionRenderer<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    graph: &'a crate::rapx_graph::ApiDependencyGraph<'tcx>,
    params: BTreeMap<LiteralParamKey, TestParam>,
    initialized_params: BTreeSet<LiteralParamKey>,
    materialized_param_values: HashMap<(usize, String), String>,
    param_use_counts: HashMap<LiteralParamKey, usize>,
    param_total_uses: HashMap<LiteralParamKey, usize>,
    uses_pyo3_python_token: bool,
}

pub(super) struct TestCasesBackend;
pub(super) static TEST_CASES_BACKEND: TestCasesBackend = TestCasesBackend;

impl SuiteBackend for TestCasesBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Tests
    }

    fn write_suite<'tcx>(&self, context: BackendRenderContext<'_, 'tcx>) -> Result<()> {
        let BackendRenderContext {
            tcx,
            graph,
            target,
            harness_dir,
            suite,
            cli,
        } = context;
        write_suite_impl(tcx, graph, target, harness_dir, suite, cli)
    }

    fn validation_command(&self, manifest: &Path) -> Option<BackendValidationCommand> {
        Some(BackendValidationCommand {
            program: "cargo",
            args: vec![
                "test".into(),
                "--manifest-path".into(),
                manifest.as_os_str().to_owned(),
                "--locked".into(),
            ],
            description: "generated test suite cargo test",
        })
    }
}

fn write_suite_impl<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &crate::rapx_graph::ApiDependencyGraph<'tcx>,
    target: &WorkspaceTarget,
    harness_dir: &Path,
    suite: &SynthesizedSuite<'tcx>,
    cli: &VariesArgs,
) -> Result<()> {
    let layout = prepare_generated_layout(harness_dir)?;
    let src_dir = &layout.src_dir;
    let meta_dir = &layout.meta_dir;
    let dependency_crate = dependency_crate_ident(target);

    fs::write(
        harness_dir.join("Cargo.toml"),
        render_harness_manifest(target, cli, &[]),
    )
    .with_context(|| format!("failed to write {}/Cargo.toml", harness_dir.display()))?;
    reset_lockfile(harness_dir)?;

    let mut lib_rs =
        String::from("#![allow(dead_code, unused_imports, unused_mut, unused_variables)]\n");
    let rendered_modules: Vec<(String, String)> = par_map(0..suite.sequences.len(), |index| {
        let module_name = sequence_module_name(&dependency_crate, index);
        let rendered = render_sequence_file(tcx, graph, target, suite, index);
        (module_name, rendered)
    });
    for (module_name, rendered) in rendered_modules {
        lib_rs.push_str(&format!("mod {module_name};\n"));
        fs::write(src_dir.join(format!("{module_name}.rs")), rendered)
            .with_context(|| format!("failed to write sequence module {module_name}"))?;
    }
    fs::write(src_dir.join("lib.rs"), lib_rs)
        .with_context(|| format!("failed to write {}", src_dir.join("lib.rs").display()))?;

    write_suite_metadata(target, meta_dir, suite)?;

    Ok(())
}

fn render_sequence_file<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &crate::rapx_graph::ApiDependencyGraph<'tcx>,
    target: &WorkspaceTarget,
    suite: &SynthesizedSuite<'tcx>,
    index: usize,
) -> String {
    let sequence = &suite.sequences[index];
    let dependency_crate = dependency_crate_ident(target);
    let mut renderer = TestSequenceFunctionRenderer::new(
        tcx,
        graph,
        sequence_param_use_counts(tcx, suite, sequence),
    );
    let RenderedSequenceFunction {
        params,
        lines: function_lines,
    } = render_sequence_function(&mut renderer, &dependency_crate, suite, index);
    let mut lines = vec![
        "#![allow(dead_code, unused_imports, unused_mut, unused_variables)]".to_owned(),
        format!("extern crate {};", dependency_crate),
        String::new(),
    ];
    lines.extend(function_lines);
    lines.push(String::new());
    lines.push("#[test]".to_owned());
    lines.push(format!("fn generated_test_{index}() {{"));
    let params = params.iter().collect::<Vec<_>>();
    for param in &params {
        lines.push(format!(
            "    let {}_values = [{}];",
            param.name,
            param.literals.join(", ")
        ));
    }
    if params.is_empty() {
        lines.push(format!(
            "    let _ = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| test_function{index}()));"
        ));
    } else {
        render_literal_loops(&mut lines, index, &params, 0, 1);
    }
    lines.push("}".to_owned());
    lines.join("\n")
}

impl SequenceFunctionParam for TestParam {
    fn name(&self) -> &str {
        &self.name
    }

    fn ty(&self) -> &str {
        &self.ty
    }
}

impl<'a, 'tcx> TestSequenceFunctionRenderer<'a, 'tcx> {
    fn new(
        tcx: TyCtxt<'tcx>,
        graph: &'a crate::rapx_graph::ApiDependencyGraph<'tcx>,
        param_total_uses: HashMap<LiteralParamKey, usize>,
    ) -> Self {
        Self {
            tcx,
            graph,
            params: BTreeMap::new(),
            initialized_params: BTreeSet::new(),
            materialized_param_values: HashMap::new(),
            param_use_counts: HashMap::new(),
            param_total_uses,
            uses_pyo3_python_token: false,
        }
    }
}

impl<'tcx> SequenceFunctionRenderer<'tcx> for TestSequenceFunctionRenderer<'_, 'tcx> {
    type Param = TestParam;

    fn prepare_step(
        &mut self,
        step_index: usize,
        step: &crate::synth::CallStep,
        input_tys: &[Ty<'tcx>],
        output_ty: Ty<'tcx>,
        lines: &mut Vec<String>,
    ) -> HashMap<usize, String> {
        prepare_literal_param_snapshots_for_step(
            self.tcx,
            step_index,
            step,
            input_tys,
            output_ty,
            &mut self.params,
            &mut self.initialized_params,
            lines,
        )
    }

    fn render_arg(
        &mut self,
        step_index: usize,
        arg_index: usize,
        arg: &ArgPlan,
        input_ty: Ty<'tcx>,
        binding_override: Option<&str>,
        lines: &mut Vec<String>,
    ) -> String {
        let mut arg_context = RenderArgContext {
            tcx: self.tcx,
            graph: self.graph,
            step_index,
            params: &mut self.params,
            initialized_params: &mut self.initialized_params,
            materialized_param_values: &mut self.materialized_param_values,
            param_use_counts: &mut self.param_use_counts,
            param_total_uses: &self.param_total_uses,
            lines,
        };
        let rendered = render_arg(&mut arg_context, arg_index, arg, input_ty, binding_override);
        if is_pyo3_python_token_input(self.tcx, input_ty) {
            self.uses_pyo3_python_token = true;
        }
        rendered
    }

    fn adapt_call_output(&self, adapter: OutputAdapter, call: String) -> String {
        adapt_call_output(adapter, call)
    }

    fn wrap_body_lines(&self, body_lines: Vec<String>) -> Vec<String> {
        wrap_body_in_pyo3_attach(self.uses_pyo3_python_token, body_lines)
    }

    fn params(&self) -> Vec<Self::Param> {
        self.params.values().cloned().collect()
    }
}

fn sequence_param_use_counts<'tcx>(
    tcx: TyCtxt<'tcx>,
    suite: &SynthesizedSuite<'tcx>,
    sequence: &crate::synth::SequencePlan,
) -> HashMap<LiteralParamKey, usize> {
    let mut counts = HashMap::new();
    for step in &sequence.steps {
        let api = &suite.apis[step.api_index];
        for (arg_index, arg) in step.args.iter().enumerate() {
            let ValueSource::Param(param_index) = arg.source else {
                continue;
            };
            if is_pyo3_python_token_input(tcx, api.inputs[arg_index]) {
                continue;
            }
            let key = literal_param_key(tcx, param_index, api.inputs[arg_index]);
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    counts
}

fn prepare_literal_param_snapshots_for_step<'tcx>(
    tcx: TyCtxt<'tcx>,
    step_index: usize,
    step: &crate::synth::CallStep,
    input_tys: &[Ty<'tcx>],
    output_ty: Ty<'tcx>,
    params: &mut BTreeMap<LiteralParamKey, TestParam>,
    initialized_params: &mut BTreeSet<LiteralParamKey>,
    lines: &mut Vec<String>,
) -> HashMap<usize, String> {
    let mut usages_by_param = HashMap::<usize, Vec<(usize, BorrowMode, Ty<'tcx>)>>::new();
    for (arg_index, arg) in step.args.iter().enumerate() {
        let ValueSource::Param(param_index) = arg.source else {
            continue;
        };
        if is_pyo3_python_token_input(tcx, input_tys[arg_index]) {
            continue;
        }
        usages_by_param.entry(param_index).or_default().push((
            arg_index,
            arg.borrow,
            input_tys[arg_index],
        ));
    }

    let mut snapshots = HashMap::new();
    if !output_ty.is_unit() {
        for (arg_index, arg) in step.args.iter().enumerate() {
            let ValueSource::Param(param_index) = arg.source else {
                continue;
            };
            if is_pyo3_python_token_input(tcx, input_tys[arg_index]) {
                continue;
            }
            if matches!(arg.borrow, BorrowMode::Move) {
                continue;
            }
            let binding_name = ensure_literal_binding(
                tcx,
                param_index,
                input_tys[arg_index],
                params,
                initialized_params,
                lines,
            );
            let snapshot_name = format!("{binding_name}_snapshot_{step_index}_{arg_index}");
            lines.push(format!(
                "    let mut {snapshot_name} = {binding_name}.clone();"
            ));
            snapshots.insert(arg_index, snapshot_name);
        }
    }

    for (param_index, usages) in usages_by_param {
        let Some(anchor_arg_index) = snapshot_anchor_arg_index(&usages) else {
            continue;
        };
        let anchor_input_ty = usages
            .iter()
            .find(|(arg_index, _, _)| *arg_index == anchor_arg_index)
            .map(|(_, _, input_ty)| *input_ty)
            .expect("anchor usage should exist");
        let binding_name = ensure_literal_binding(
            tcx,
            param_index,
            anchor_input_ty,
            params,
            initialized_params,
            lines,
        );

        for (arg_index, _, _) in usages {
            if arg_index == anchor_arg_index || snapshots.contains_key(&arg_index) {
                continue;
            }
            let snapshot_name = format!("{binding_name}_snapshot_{step_index}_{arg_index}");
            lines.push(format!(
                "    let mut {snapshot_name} = {binding_name}.clone();"
            ));
            snapshots.insert(arg_index, snapshot_name);
        }
    }

    snapshots
}

fn snapshot_anchor_arg_index(usages: &[(usize, BorrowMode, Ty<'_>)]) -> Option<usize> {
    if !param_usages_need_snapshots(usages) {
        return None;
    }

    Some(
        usages
            .iter()
            .find(|(_, borrow, _)| is_mutable_like(*borrow))
            .map(|(arg_index, _, _)| *arg_index)
            .unwrap_or_else(|| usages[0].0),
    )
}

fn param_usages_need_snapshots(usages: &[(usize, BorrowMode, Ty<'_>)]) -> bool {
    usages.len() > 1
        && usages.iter().any(|(_, borrow, _)| {
            matches!(
                borrow,
                BorrowMode::Move | BorrowMode::Mutable | BorrowMode::RawMutable
            )
        })
}

fn is_mutable_like(borrow: BorrowMode) -> bool {
    matches!(borrow, BorrowMode::Mutable | BorrowMode::RawMutable)
}

fn render_literal_loops(
    lines: &mut Vec<String>,
    index: usize,
    params: &[&TestParam],
    depth: usize,
    indent: usize,
) {
    let indent_prefix = "    ".repeat(indent);
    let param = params[depth];
    lines.push(format!(
        "{indent_prefix}for {} in {}_values.into_iter() {{",
        param.name, param.name
    ));
    if depth + 1 == params.len() {
        let call_args = params
            .iter()
            .map(|param| param.name.clone())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "{}    let _ = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| test_function{index}({call_args})));",
            indent_prefix
        ));
    } else {
        render_literal_loops(lines, index, params, depth + 1, indent + 1);
    }
    lines.push(format!("{indent_prefix}}}"));
}

fn adapt_call_output(adapter: OutputAdapter, call: String) -> String {
    match adapter {
        OutputAdapter::Plain => call,
        OutputAdapter::Option => format!(
            "match {call} {{ ::core::option::Option::Some(value) => value, ::core::option::Option::None => return ::core::option::Option::None, }}"
        ),
        OutputAdapter::Result => format!(
            "match {call} {{ ::core::result::Result::Ok(value) => value, ::core::result::Result::Err(_) => return ::core::option::Option::None, }}"
        ),
    }
}

fn render_arg<'tcx>(
    context: &mut RenderArgContext<'_, 'tcx>,
    arg_index: usize,
    arg: &ArgPlan,
    input_ty: Ty<'tcx>,
    binding_override: Option<&str>,
) -> String {
    match &arg.source {
        ValueSource::Param(param_index) => {
            if is_pyo3_python_token_input(context.tcx, input_ty) {
                return borrow_value(arg.borrow, "py");
            }
            let (binding_name, force_fresh_seed, has_future_uses) =
                if let Some(binding_override) = binding_override {
                    record_literal_param_use(context, *param_index, input_ty);
                    (binding_override.to_owned(), true, false)
                } else {
                    let (binding_name, has_future_uses) =
                        ensure_literal_binding_for_arg(context, *param_index, input_ty);
                    (binding_name, false, has_future_uses)
                };
            if let Some(adapted_binding) = ensure_materialized_value_binding_from_seed(
                context,
                *param_index,
                arg_index,
                input_ty,
                &binding_name,
                force_fresh_seed,
            ) {
                return borrow_materialized_value(
                    input_ty,
                    arg.borrow,
                    &adapted_binding,
                    has_future_uses,
                );
            }
            borrow_rendered_value(arg.borrow, &binding_name, has_future_uses)
        }
        ValueSource::Step(source_step) => {
            let value = format!("value_{source_step}");
            borrow_value(arg.borrow, &value)
        }
    }
}

fn wrap_body_in_pyo3_attach(uses_pyo3_python_token: bool, body_lines: Vec<String>) -> Vec<String> {
    if !uses_pyo3_python_token {
        return body_lines;
    }

    let mut wrapped = vec![
        "    ::pyo3::Python::initialize();".to_owned(),
        "    let _attach_result = ::pyo3::Python::attach(|py| -> Option<()> {".to_owned(),
    ];
    wrapped.extend(body_lines.into_iter().map(|line| format!("    {line}")));
    wrapped.push("        Some(())".to_owned());
    wrapped.push("    });".to_owned());
    wrapped.push("    _attach_result?;".to_owned());
    wrapped
}

fn ensure_literal_binding_for_arg<'tcx>(
    context: &mut RenderArgContext<'_, 'tcx>,
    param_index: usize,
    input_ty: Ty<'tcx>,
) -> (String, bool) {
    let key = literal_param_key(context.tcx, param_index, input_ty);
    let binding_name = ensure_literal_binding(
        context.tcx,
        param_index,
        input_ty,
        context.params,
        context.initialized_params,
        context.lines,
    );
    let use_count = context.param_use_counts.entry(key.clone()).or_insert(0);
    *use_count += 1;
    let has_future_uses = *use_count < *context.param_total_uses.get(&key).unwrap_or(&1);
    (binding_name, has_future_uses)
}

fn ensure_literal_binding<'tcx>(
    tcx: TyCtxt<'tcx>,
    param_index: usize,
    input_ty: Ty<'tcx>,
    params: &mut BTreeMap<LiteralParamKey, TestParam>,
    initialized_params: &mut BTreeSet<LiteralParamKey>,
    lines: &mut Vec<String>,
) -> String {
    let key = literal_param_key(tcx, param_index, input_ty);
    let param_name = ensure_literal_param(tcx, input_ty, &key, params);
    let binding_name = format!("literal_param_{}", param_name.trim_start_matches("_param"));
    if initialized_params.insert(key) {
        lines.push(format!("    let mut {binding_name} = {param_name};"));
    }
    binding_name
}

fn record_literal_param_use<'tcx>(
    context: &mut RenderArgContext<'_, 'tcx>,
    param_index: usize,
    input_ty: Ty<'tcx>,
) {
    let key = literal_param_key(context.tcx, param_index, input_ty);
    *context.param_use_counts.entry(key).or_insert(0) += 1;
}

fn borrow_rendered_value(borrow: BorrowMode, value: &str, clone_move: bool) -> String {
    match borrow {
        BorrowMode::Move if clone_move => format!("{value}.clone()"),
        _ => borrow_value(borrow, value),
    }
}

fn borrow_materialized_value(
    input_ty: Ty<'_>,
    borrow: BorrowMode,
    value: &str,
    clone_move: bool,
) -> String {
    if matches!(input_ty.kind(), rustc_middle::ty::TyKind::Ref(..)) {
        return match borrow {
            BorrowMode::Shared => format!("&*{value}"),
            BorrowMode::Mutable => format!("&mut *{value}"),
            _ => borrow_value(borrow, value),
        };
    }
    borrow_rendered_value(borrow, value, clone_move)
}

fn borrow_value(borrow: BorrowMode, value: &str) -> String {
    match borrow {
        BorrowMode::Move => value.to_owned(),
        BorrowMode::Shared => format!("&{value}"),
        BorrowMode::Mutable => format!("&mut {value}"),
        BorrowMode::RawConst => format!("&{value} as *const _"),
        BorrowMode::RawMutable => format!("&mut {value} as *mut _"),
    }
}

fn ensure_materialized_value_binding_from_seed<'tcx>(
    context: &mut RenderArgContext<'_, 'tcx>,
    param_index: usize,
    arg_index: usize,
    input_ty: Ty<'tcx>,
    seed_binding: &str,
    force_fresh: bool,
) -> Option<String> {
    if !needs_symbolic_seed_binding(context.tcx, input_ty) {
        return None;
    }

    let cache_key = (
        param_index,
        context
            .tcx
            .erase_and_anonymize_regions(seed_value_ty(input_ty))
            .to_string(),
    );
    if !force_fresh && let Some(existing) = context.materialized_param_values.get(&cache_key) {
        return Some(existing.clone());
    }

    let value_binding = format!("{seed_binding}_value_{}_{arg_index}", context.step_index,);
    let value_expr =
        render_value_from_symbolic_seed(context.tcx, Some(context.graph), input_ty, seed_binding)?;
    let value_initializer = if matches!(input_ty.kind(), rustc_middle::ty::TyKind::Ref(..)) {
        format!("::std::boxed::Box::leak(::std::boxed::Box::new({value_expr}))")
    } else {
        value_expr
    };
    context.lines.push(format!(
        "    let mut {value_binding} = {value_initializer};"
    ));
    if !force_fresh {
        context
            .materialized_param_values
            .insert(cache_key, value_binding.clone());
    }
    Some(value_binding)
}

fn literal_param_key<'tcx>(
    tcx: TyCtxt<'tcx>,
    param_index: usize,
    input_ty: Ty<'tcx>,
) -> LiteralParamKey {
    let ty = kani_seed_ty_for_input(tcx, input_ty)
        .expect("literal params should only be allocated for supported input types");
    LiteralParamKey {
        source_index: param_index,
        ty,
    }
}

fn ensure_literal_param<'tcx>(
    tcx: TyCtxt<'tcx>,
    input_ty: Ty<'tcx>,
    key: &LiteralParamKey,
    params: &mut BTreeMap<LiteralParamKey, TestParam>,
) -> String {
    if let Some(param) = params.get(key) {
        return param.name.clone();
    }

    let collision_count = params
        .keys()
        .filter(|existing| existing.source_index == key.source_index)
        .count();
    let suffix = if collision_count == 0 {
        String::new()
    } else {
        format!("_alt{collision_count}")
    };
    let name = format!("_param{}{}", key.source_index, suffix);
    let literals = literal_seed_values_for_input(tcx, input_ty)
        .expect("literal params should only be allocated for inputs with literal seed values");
    params.insert(
        key.clone(),
        TestParam {
            name: name.clone(),
            ty: key.ty.clone(),
            literals,
        },
    );
    name
}
