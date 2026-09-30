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
    is_pyo3_python_token_input, kani_seed_ty_for_input, needs_symbolic_seed_binding,
    render_kani_seed_expr_for_value_ty, render_value_from_symbolic_seed, seed_value_ty,
};
use crate::synth::{ArgPlan, BorrowMode, OutputAdapter, SynthesizedSuite, ValueSource};
use crate::workspace::WorkspaceTarget;
use anyhow::{Context, Result};
use rustc_data_structures::sync::par_map;
use rustc_middle::ty::Ty;
use rustc_middle::ty::TyCtxt;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

#[derive(Clone)]
pub(super) struct KaniParam<'tcx> {
    pub(super) name: String,
    pub(super) ty: String,
    pub(super) value_ty: Ty<'tcx>,
}

struct RenderArgContext<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    graph: &'a crate::rapx_graph::ApiDependencyGraph<'tcx>,
    step_index: usize,
    params: &'a mut BTreeMap<SymbolicParamKey, KaniParam<'tcx>>,
    initialized_symbolic_params: &'a mut BTreeSet<SymbolicParamKey>,
    materialized_param_values: &'a mut HashMap<(usize, String), String>,
    lines: &'a mut Vec<String>,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct SymbolicParamKey {
    source_index: usize,
    ty: String,
}

struct KaniSequenceFunctionRenderer<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    graph: &'a crate::rapx_graph::ApiDependencyGraph<'tcx>,
    params: BTreeMap<SymbolicParamKey, KaniParam<'tcx>>,
    initialized_symbolic_params: BTreeSet<SymbolicParamKey>,
    materialized_param_values: HashMap<(usize, String), String>,
    uses_pyo3_python_token: bool,
}

pub(super) struct KaniBackend;
pub(super) static KANI_BACKEND: KaniBackend = KaniBackend;

impl SuiteBackend for KaniBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Kani
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
        let target_dir = manifest.parent()?.join("target").join("kani");
        Some(BackendValidationCommand {
            program: "cargo",
            args: vec![
                "varies-kani".into(),
                "--manifest-path".into(),
                manifest.as_os_str().to_owned(),
                "--target-dir".into(),
                target_dir.into_os_string(),
                "--only-codegen".into(),
            ],
            description: "generated Kani harness cargo varies-kani --only-codegen",
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
        render_harness_manifest(
            target,
            cli,
            &[
                "[lints.rust]",
                "unexpected_cfgs = { level = \"allow\", check-cfg = ['cfg(kani)'] }",
            ],
        ),
    )
    .with_context(|| format!("failed to write {}/Cargo.toml", harness_dir.display()))?;
    reset_lockfile(harness_dir)?;

    let mut lib_rs = String::from(
        "#![feature(rustc_private)]\n#![cfg(kani)]\n#![allow(dead_code, unused_imports, unused_mut, unused_variables)]\n",
    );
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
    let dependency_crate = dependency_crate_ident(target);
    let RenderedSequenceFunction {
        params,
        lines: function_lines,
    } = render_kani_seeded_sequence_function(tcx, graph, &dependency_crate, suite, index);
    let mut lines = vec![
        "#![allow(dead_code, unused_imports, unused_mut, unused_variables)]".to_owned(),
        format!("extern crate {};", dependency_crate),
        String::new(),
    ];
    lines.extend(function_lines);
    lines.push(String::new());
    lines.push("#[cfg_attr(kani, kani::proof)]".to_owned());
    lines.push(format!("fn kani_test_{index}() {{"));
    for param in &params {
        let seed_expr =
            render_kani_seed_expr_for_value_ty(tcx, Some(graph), param.value_ty, &param.ty)
                .unwrap_or_else(|| format!("kani::any::<{}>()", param.ty));
        lines.push(format!("    let {} = {};", param.name, seed_expr));
    }
    let call_args = params
        .iter()
        .map(|param| param.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    lines.push(format!("    let _ = test_function{index}({call_args});"));
    lines.push("}".to_owned());
    lines.join("\n")
}

pub(super) fn render_kani_seeded_sequence_function<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &crate::rapx_graph::ApiDependencyGraph<'tcx>,
    dependency_crate: &str,
    suite: &SynthesizedSuite<'tcx>,
    index: usize,
) -> RenderedSequenceFunction<KaniParam<'tcx>> {
    let mut renderer = KaniSequenceFunctionRenderer::new(tcx, graph);
    render_sequence_function(&mut renderer, dependency_crate, suite, index)
}

impl SequenceFunctionParam for KaniParam<'_> {
    fn name(&self) -> &str {
        &self.name
    }

    fn ty(&self) -> &str {
        &self.ty
    }
}

impl<'a, 'tcx> KaniSequenceFunctionRenderer<'a, 'tcx> {
    fn new(tcx: TyCtxt<'tcx>, graph: &'a crate::rapx_graph::ApiDependencyGraph<'tcx>) -> Self {
        Self {
            tcx,
            graph,
            params: BTreeMap::new(),
            initialized_symbolic_params: BTreeSet::new(),
            materialized_param_values: HashMap::new(),
            uses_pyo3_python_token: false,
        }
    }
}

impl<'tcx> SequenceFunctionRenderer<'tcx> for KaniSequenceFunctionRenderer<'_, 'tcx> {
    type Param = KaniParam<'tcx>;

    fn prepare_step(
        &mut self,
        step_index: usize,
        step: &crate::synth::CallStep,
        input_tys: &[Ty<'tcx>],
        _output_ty: Ty<'tcx>,
        lines: &mut Vec<String>,
    ) -> HashMap<usize, String> {
        prepare_param_snapshots_for_step(
            self.tcx,
            step_index,
            step,
            input_tys,
            &mut self.params,
            &mut self.initialized_symbolic_params,
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
            initialized_symbolic_params: &mut self.initialized_symbolic_params,
            materialized_param_values: &mut self.materialized_param_values,
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

#[cfg(test)]
mod tests {
    use super::snapshot_anchor_arg_index_from_borrows;
    use crate::synth::BorrowMode;

    #[test]
    fn snapshot_anchor_prefers_first_mutable_use() {
        let usages = [
            (0, BorrowMode::Shared),
            (1, BorrowMode::Mutable),
            (2, BorrowMode::Mutable),
        ];

        assert_eq!(snapshot_anchor_arg_index_from_borrows(&usages), Some(1));
    }

    #[test]
    fn snapshot_anchor_is_not_needed_for_shared_reuse() {
        let usages = [(0, BorrowMode::Shared), (1, BorrowMode::Shared)];

        assert_eq!(snapshot_anchor_arg_index_from_borrows(&usages), None);
    }

    #[test]
    fn snapshot_anchor_supports_multiple_mutable_uses() {
        let usages = [
            (0, BorrowMode::Mutable),
            (1, BorrowMode::RawMutable),
            (2, BorrowMode::Shared),
        ];

        assert_eq!(snapshot_anchor_arg_index_from_borrows(&usages), Some(0));
    }
}

fn adapt_call_output(adapter: OutputAdapter, call: String) -> String {
    match adapter {
        OutputAdapter::Plain => call,
        OutputAdapter::Option => format!(
            "match {call} {{ ::core::option::Option::Some(value) => value, ::core::option::Option::None => panic!(\"fallible API returned None\"), }}"
        ),
        OutputAdapter::Result => format!(
            "match {call} {{ ::core::result::Result::Ok(value) => value, ::core::result::Result::Err(_) => panic!(\"fallible API returned Err\"), }}"
        ),
    }
}

// If one symbolic parameter is consumed multiple times in the same call,
// clone it into per-argument snapshots whenever any use moves or exclusively
// borrows the value. This keeps the generated call valid even when the same
// symbolic seed is passed mutably more than once.
fn prepare_param_snapshots_for_step<'tcx>(
    tcx: TyCtxt<'tcx>,
    step_index: usize,
    step: &crate::synth::CallStep,
    input_tys: &[Ty<'tcx>],
    params: &mut BTreeMap<SymbolicParamKey, KaniParam<'tcx>>,
    initialized_symbolic_params: &mut BTreeSet<SymbolicParamKey>,
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
    for (param_index, usages) in usages_by_param {
        let Some(anchor_arg_index) = snapshot_anchor_arg_index(&usages) else {
            continue;
        };
        let anchor_input_ty = usages
            .iter()
            .find(|(arg_index, _, _)| *arg_index == anchor_arg_index)
            .map(|(_, _, input_ty)| *input_ty)
            .expect("anchor usage should exist");
        let binding_name = ensure_symbolic_binding(
            tcx,
            param_index,
            anchor_input_ty,
            params,
            initialized_symbolic_params,
            lines,
        );

        for (arg_index, _, _) in usages {
            if arg_index == anchor_arg_index {
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
    let borrow_usages = usages
        .iter()
        .map(|(arg_index, borrow, _)| (*arg_index, *borrow))
        .collect::<Vec<_>>();
    snapshot_anchor_arg_index_from_borrows(&borrow_usages)
}

fn snapshot_anchor_arg_index_from_borrows(usages: &[(usize, BorrowMode)]) -> Option<usize> {
    if !param_usages_need_snapshots_from_borrows(usages) {
        return None;
    }

    Some(
        usages
            .iter()
            .find(|(_, borrow)| is_mutable_like(*borrow))
            .map(|(arg_index, _)| *arg_index)
            .unwrap_or_else(|| usages[0].0),
    )
}

fn param_usages_need_snapshots_from_borrows(usages: &[(usize, BorrowMode)]) -> bool {
    usages.len() > 1
        && usages.iter().any(|(_, borrow)| {
            matches!(
                borrow,
                BorrowMode::Move | BorrowMode::Mutable | BorrowMode::RawMutable
            )
        })
}

fn is_mutable_like(borrow: BorrowMode) -> bool {
    matches!(borrow, BorrowMode::Mutable | BorrowMode::RawMutable)
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
                return render_borrowed_python_token(arg.borrow);
            }
            let binding_name = binding_override.map(ToOwned::to_owned).unwrap_or_else(|| {
                ensure_symbolic_binding(
                    context.tcx,
                    *param_index,
                    input_ty,
                    context.params,
                    context.initialized_symbolic_params,
                    context.lines,
                )
            });
            if let Some(adapted_binding) = ensure_materialized_value_binding_from_seed(
                context,
                *param_index,
                arg_index,
                input_ty,
                &binding_name,
                binding_override.is_some(),
            ) {
                return match arg.borrow {
                    BorrowMode::Shared
                        if matches!(input_ty.kind(), rustc_middle::ty::TyKind::Ref(..)) =>
                    {
                        format!("&*{adapted_binding}")
                    }
                    BorrowMode::Mutable
                        if matches!(input_ty.kind(), rustc_middle::ty::TyKind::Ref(..)) =>
                    {
                        format!("&mut *{adapted_binding}")
                    }
                    BorrowMode::Move => adapted_binding,
                    BorrowMode::Shared => format!("&{adapted_binding}"),
                    BorrowMode::Mutable => format!("&mut {adapted_binding}"),
                    BorrowMode::RawConst => format!("&{adapted_binding} as *const _"),
                    BorrowMode::RawMutable => format!("&mut {adapted_binding} as *mut _"),
                };
            }
            match arg.borrow {
                BorrowMode::Move => binding_name,
                BorrowMode::Shared => format!("&{binding_name}"),
                BorrowMode::Mutable => format!("&mut {binding_name}"),
                BorrowMode::RawConst => format!("&{binding_name} as *const _"),
                BorrowMode::RawMutable => format!("&mut {binding_name} as *mut _"),
            }
        }
        ValueSource::Step(source_step) => {
            let value = format!("value_{source_step}");
            match arg.borrow {
                BorrowMode::Move => value,
                BorrowMode::Shared => format!("&{value}"),
                BorrowMode::Mutable => format!("&mut {value}"),
                BorrowMode::RawConst => format!("&{value} as *const _"),
                BorrowMode::RawMutable => format!("&mut {value} as *mut _"),
            }
        }
    }
}

fn render_borrowed_python_token(borrow: BorrowMode) -> String {
    match borrow {
        BorrowMode::Move => "py".to_owned(),
        BorrowMode::Shared => "&py".to_owned(),
        BorrowMode::Mutable => "&mut py".to_owned(),
        BorrowMode::RawConst => "&py as *const _".to_owned(),
        BorrowMode::RawMutable => "&mut py as *mut _".to_owned(),
    }
}

fn wrap_body_in_pyo3_attach(uses_pyo3_python_token: bool, body_lines: Vec<String>) -> Vec<String> {
    if !uses_pyo3_python_token {
        return body_lines;
    }

    let mut wrapped = vec![
        "    ::pyo3::Python::initialize();".to_owned(),
        "    ::pyo3::Python::attach(|py| {".to_owned(),
    ];
    wrapped.extend(body_lines.into_iter().map(|line| format!("    {line}")));
    wrapped.push("    });".to_owned());
    wrapped
}

// Materialize an owned call-site value from a symbolic seed when the API
// expects a reconstructed wrapper type such as `String`, `Vec`, or a local ADT.
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

fn symbolic_param_key<'tcx>(
    tcx: TyCtxt<'tcx>,
    param_index: usize,
    input_ty: Ty<'tcx>,
) -> SymbolicParamKey {
    let ty = kani_seed_ty_for_input(tcx, input_ty)
        .expect("symbolic params should only be allocated for supported Kani input types");
    SymbolicParamKey {
        source_index: param_index,
        ty,
    }
}

fn ensure_symbolic_binding<'tcx>(
    tcx: TyCtxt<'tcx>,
    param_index: usize,
    input_ty: Ty<'tcx>,
    params: &mut BTreeMap<SymbolicParamKey, KaniParam<'tcx>>,
    initialized_symbolic_params: &mut BTreeSet<SymbolicParamKey>,
    lines: &mut Vec<String>,
) -> String {
    let key = symbolic_param_key(tcx, param_index, input_ty);
    let param_name = ensure_symbolic_param(tcx, input_ty, &key, params);
    let binding_name = format!("symbolic_param_{}", param_name.trim_start_matches("_param"));
    if initialized_symbolic_params.insert(key) {
        lines.push(format!("    let mut {binding_name} = {param_name};"));
    }
    binding_name
}

fn ensure_symbolic_param<'tcx>(
    tcx: TyCtxt<'tcx>,
    input_ty: Ty<'tcx>,
    key: &SymbolicParamKey,
    params: &mut BTreeMap<SymbolicParamKey, KaniParam<'tcx>>,
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
    params.insert(
        key.clone(),
        KaniParam {
            name: name.clone(),
            ty: key.ty.clone(),
            value_ty: tcx.erase_and_anonymize_regions(seed_value_ty(input_ty)),
        },
    );
    name
}
