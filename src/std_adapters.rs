use crate::rapx_graph::ApiDependencyGraph;
use rustc_hir::LangItem;
use rustc_hir::def_id::{CRATE_DEF_INDEX, DefId};
use rustc_middle::ty::{self, AdtDef, GenericArgsRef, Ty, TyCtxt, TyKind, VariantDef};
use rustc_span::sym;

const SYMBOLIC_COLLECTION_LEN: usize = 8;
const MAX_COMPOSITE_LITERAL_OPTIONS: usize = 6;
const KANI_TUPLE_ARBITRARY_LIMIT: usize = 12;

/// Strip reference-like wrappers from an input type to recover the symbolic
/// seed value type used by the harness.
pub fn seed_value_ty(input_ty: Ty<'_>) -> Ty<'_> {
    match input_ty.kind() {
        TyKind::Ref(_, inner, _) | TyKind::RawPtr(inner, _) => *inner,
        _ => input_ty,
    }
}

/// Return the Kani parameter type used to seed a given input type, if the
/// input is currently supported by the harness adapters.
pub fn kani_seed_ty_for_input<'tcx>(tcx: TyCtxt<'tcx>, input_ty: Ty<'tcx>) -> Option<String> {
    AdapterContext::new(tcx, None).kani_seed_ty_for_value_ty(seed_value_ty(input_ty))
}

pub fn render_kani_seed_expr_for_value_ty<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: Option<&ApiDependencyGraph<'tcx>>,
    value_ty: Ty<'tcx>,
    seed_ty: &str,
) -> Option<String> {
    AdapterContext::new(tcx, graph).render_kani_seed_expr_for_value_ty(value_ty, seed_ty)
}

/// Report whether the input can be driven directly from a symbolic Kani
/// parameter without extra reconstruction steps.
pub fn supports_direct_symbolic_input<'tcx>(tcx: TyCtxt<'tcx>, input_ty: Ty<'tcx>) -> bool {
    kani_seed_ty_for_input(tcx, input_ty).is_some()
}

/// Report whether an input is PyO3's ambient `Python<'py>` interpreter token.
pub fn is_pyo3_python_token_input<'tcx>(tcx: TyCtxt<'tcx>, input_ty: Ty<'tcx>) -> bool {
    let TyKind::Adt(def, _) = seed_value_ty(input_ty).kind() else {
        return false;
    };
    let crate_name = tcx.crate_name(def.did().krate).to_string();
    if crate_name != "pyo3" {
        return false;
    }
    let path = tcx.def_path_str(def.did());
    path == "marker::Python" || path.ends_with("::marker::Python")
}

/// Report whether a std/core adapter type has a stable public path even when
/// rustc's definition path points at a private implementation module.
pub fn is_public_std_adapter_value_ty<'tcx>(tcx: TyCtxt<'tcx>, value_ty: Ty<'tcx>) -> bool {
    is_std_io_empty_value_ty(value_ty, tcx)
        || is_std_io_sink_value_ty(value_ty, tcx)
        || is_std_io_repeat_value_ty(value_ty, tcx)
        || cursor_inner_ty(value_ty, tcx).is_some()
        || is_std_io_error_value_ty(value_ty, tcx)
}

/// Report whether the input needs an intermediate reconstructed value binding
/// before it can be passed to the target call.
pub fn needs_symbolic_seed_binding<'tcx>(tcx: TyCtxt<'tcx>, input_ty: Ty<'tcx>) -> bool {
    AdapterContext::new(tcx, None).needs_symbolic_seed_binding_for_ty(seed_value_ty(input_ty))
}

/// Render the Rust expression that reconstructs the concrete call-site value
/// from a symbolic seed binding.
pub fn render_value_from_symbolic_seed<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: Option<&ApiDependencyGraph<'tcx>>,
    input_ty: Ty<'tcx>,
    seed_binding: &str,
) -> Option<String> {
    AdapterContext::new(tcx, graph)
        .render_value_from_symbolic_seed_for_ty(seed_value_ty(input_ty), seed_binding)
}

/// Return a small deterministic set of Rust literal expressions that can seed
/// the same input adapter path used for Kani symbolic inputs.
pub fn literal_seed_values_for_input<'tcx>(
    tcx: TyCtxt<'tcx>,
    input_ty: Ty<'tcx>,
) -> Option<Vec<String>> {
    AdapterContext::new(tcx, None).literal_seed_values_for_value_ty(seed_value_ty(input_ty))
}

/// Return the fixed byte count needed to construct the same seed type used by
/// Kani symbolic and literal-test inputs.
pub fn fuzz_seed_byte_len_for_value_ty<'tcx>(
    tcx: TyCtxt<'tcx>,
    value_ty: Ty<'tcx>,
) -> Option<usize> {
    AdapterContext::new(tcx, None).fuzz_seed_byte_len_for_value_ty(value_ty)
}

/// Render a Rust expression that constructs the same seed value used by Kani
/// symbolic and literal-test inputs from a fuzz data buffer.
pub fn render_fuzz_seed_from_data_for_value_ty<'tcx>(
    tcx: TyCtxt<'tcx>,
    value_ty: Ty<'tcx>,
    data_binding: &str,
    start_index: usize,
) -> Option<String> {
    AdapterContext::new(tcx, None).render_fuzz_seed_from_data_for_value_ty(
        value_ty,
        data_binding,
        start_index,
    )
}

/// Return a bounded set of common std/core value types that can be rebuilt
/// from symbolic seeds and are useful as generic seed candidates.
pub fn common_symbolic_seed_value_tys<'tcx>(tcx: TyCtxt<'tcx>) -> Vec<Ty<'tcx>> {
    let mut tys = Vec::new();

    if let Some(vec_def_id) = tcx.get_diagnostic_item(sym::Vec) {
        let vec_u8 = Ty::new_adt(
            tcx,
            tcx.adt_def(vec_def_id),
            tcx.mk_args(&[tcx.types.u8.into()]),
        );
        tys.push(vec_u8);
        if let Some(cursor_vec_u8) = std_io_cursor_value_ty(tcx, vec_u8) {
            tys.push(cursor_vec_u8);
        }
    }
    if let Some(string_def_id) = tcx.lang_items().string() {
        tys.push(Ty::new_adt(
            tcx,
            tcx.adt_def(string_def_id),
            tcx.mk_args(&[]),
        ));
    }
    if let Some(empty_ty) = std_io_empty_value_ty(tcx) {
        tys.push(empty_ty);
    }
    if let Some(sink_ty) = std_io_sink_value_ty(tcx) {
        tys.push(sink_ty);
    }
    if let Some(repeat_ty) = std_io_repeat_value_ty(tcx) {
        tys.push(repeat_ty);
    }

    tys
}

struct AdapterContext<'tcx, 'graph> {
    tcx: TyCtxt<'tcx>,
    graph: Option<&'graph ApiDependencyGraph<'tcx>>,
    local_adt_stack: Vec<DefId>,
}

impl<'tcx, 'graph> AdapterContext<'tcx, 'graph> {
    fn new(tcx: TyCtxt<'tcx>, graph: Option<&'graph ApiDependencyGraph<'tcx>>) -> Self {
        Self {
            tcx,
            graph,
            local_adt_stack: Vec::new(),
        }
    }

    fn pointer_byte_len(&self) -> usize {
        self.tcx.data_layout.pointer_size().bytes() as usize
    }

    fn array_len_value(&self, len: ty::Const<'tcx>) -> Option<usize> {
        if let Some(value) = len.try_to_target_usize(self.tcx) {
            return Some(value as usize);
        }

        let ty::ConstKind::Unevaluated(unevaluated) = len.kind() else {
            return None;
        };
        let valtree = self
            .tcx
            .const_eval_resolve_for_typeck(
                ty::TypingEnv::fully_monomorphized(),
                unevaluated,
                self.tcx.def_span(unevaluated.def),
            )
            .ok()?
            .ok()?;
        ty::Value {
            ty: self.tcx.types.usize,
            valtree,
        }
        .try_to_target_usize(self.tcx)
        .map(|value| value as usize)
    }

    fn kani_seed_ty_for_value_ty(&mut self, value_ty: Ty<'tcx>) -> Option<String> {
        if is_std_io_empty_value_ty(value_ty, self.tcx)
            || is_std_io_sink_value_ty(value_ty, self.tcx)
        {
            return Some("()".to_owned());
        }
        if is_std_io_repeat_value_ty(value_ty, self.tcx) {
            return Some("u8".to_owned());
        }
        if let Some(inner) = cursor_inner_ty(value_ty, self.tcx) {
            return self.kani_seed_ty_for_value_ty(inner);
        }
        if is_symbolic_string_value_ty(value_ty, self.tcx) {
            return Some(format_tuple_seed_ty(&[
                format!("[u8; {SYMBOLIC_COLLECTION_LEN}]"),
                "usize".to_owned(),
            ]));
        }
        if let Some(inner) = symbolic_vec_inner_ty(value_ty, self.tcx) {
            let seed_ty = self.kani_seed_ty_for_value_ty(inner)?;
            return Some(format_tuple_seed_ty(&[
                format!("[{seed_ty}; {SYMBOLIC_COLLECTION_LEN}]"),
                "usize".to_owned(),
            ]));
        }
        if let Some(inner) = boxed_inner_ty(value_ty, self.tcx) {
            return self.kani_seed_ty_for_value_ty(inner);
        }
        if let Some((inner, variant)) = option_inner_ty(value_ty, self.tcx) {
            let seed_ty = self.kani_seed_ty_for_value_ty(inner)?;
            return Some(format!("{variant}<{seed_ty}>"));
        }
        if let Some(((ok_ty, err_ty), variant)) = result_inner_tys(value_ty, self.tcx) {
            let ok_seed_ty = self.kani_seed_ty_for_value_ty(ok_ty)?;
            let err_seed_ty = self.kani_seed_ty_for_value_ty(err_ty)?;
            return Some(format!("{variant}<{ok_seed_ty}, {err_seed_ty}>"));
        }
        if let TyKind::Tuple(types) = value_ty.kind() {
            let element_seed_tys = types
                .iter()
                .map(|ty| self.kani_seed_ty_for_value_ty(ty))
                .collect::<Option<Vec<_>>>()?;
            return Some(format_tuple_seed_ty(&element_seed_tys));
        }
        if let TyKind::Slice(inner) = value_ty.kind() {
            let inner_seed_ty = self.kani_seed_ty_for_value_ty(*inner)?;
            return Some(format_tuple_seed_ty(&[
                format!("[{inner_seed_ty}; {SYMBOLIC_COLLECTION_LEN}]"),
                "usize".to_owned(),
            ]));
        }
        if let TyKind::Array(inner, len) = value_ty.kind() {
            let inner_seed_ty = self.kani_seed_ty_for_value_ty(*inner)?;
            let len = self.array_len_value(*len)?;
            return Some(format!("[{inner_seed_ty}; {len}]"));
        }
        if let Some(enum_spec) = self.local_enum_spec(value_ty) {
            if enum_spec
                .variants
                .iter()
                .all(|variant| variant.field_seed_tys.is_empty())
            {
                return Some("usize".to_owned());
            }
            let mut elements = vec!["usize".to_owned()];
            for variant in enum_spec.variants {
                elements.push(format_tuple_seed_ty(&variant.field_seed_tys));
            }
            return Some(format_tuple_seed_ty(&elements));
        }
        if let Some(struct_spec) = self.local_struct_spec(value_ty) {
            return Some(format_tuple_seed_ty(&struct_spec.field_seed_tys));
        }

        match value_ty.kind() {
            TyKind::Bool | TyKind::Char | TyKind::Int(_) | TyKind::Uint(_) | TyKind::Float(_) => {
                Some(value_ty.to_string())
            }
            _ => None,
        }
    }

    fn render_kani_seed_expr_for_value_ty(
        &mut self,
        value_ty: Ty<'tcx>,
        seed_ty: &str,
    ) -> Option<String> {
        if !self.needs_structural_kani_seed_expr(value_ty) {
            return Some(format!("kani::any::<{seed_ty}>()"));
        }
        self.render_structural_kani_seed_expr_for_ty(value_ty)
    }

    fn needs_structural_kani_seed_expr(&mut self, value_ty: Ty<'tcx>) -> bool {
        if let Some(inner) = cursor_inner_ty(value_ty, self.tcx) {
            return self.needs_structural_kani_seed_expr(inner);
        }
        if let Some(inner) = boxed_inner_ty(value_ty, self.tcx) {
            return self.needs_structural_kani_seed_expr(inner);
        }
        if let Some(inner) = symbolic_vec_inner_ty(value_ty, self.tcx) {
            return self.needs_structural_kani_seed_expr(inner);
        }
        if let Some((inner, _)) = option_inner_ty(value_ty, self.tcx) {
            return self.needs_structural_kani_seed_expr(inner);
        }
        if let Some(((ok_ty, err_ty), _)) = result_inner_tys(value_ty, self.tcx) {
            return self.needs_structural_kani_seed_expr(ok_ty)
                || self.needs_structural_kani_seed_expr(err_ty);
        }
        match value_ty.kind() {
            TyKind::Tuple(types) => {
                types.len() > KANI_TUPLE_ARBITRARY_LIMIT
                    || types
                        .iter()
                        .any(|ty| self.needs_structural_kani_seed_expr(ty))
            }
            TyKind::Array(inner, _) | TyKind::Slice(inner) => {
                self.needs_structural_kani_seed_expr(*inner)
            }
            _ => {
                if let Some(enum_spec) = self.local_enum_spec(value_ty) {
                    return enum_spec.variants.len() + 1 > KANI_TUPLE_ARBITRARY_LIMIT
                        || enum_spec.variants.iter().any(|variant| {
                            variant.field_tys.len() > KANI_TUPLE_ARBITRARY_LIMIT
                                || variant
                                    .field_tys
                                    .iter()
                                    .any(|ty| self.needs_structural_kani_seed_expr(*ty))
                        });
                }
                if let Some(struct_spec) = self.local_struct_spec(value_ty) {
                    return struct_spec.field_tys.len() > KANI_TUPLE_ARBITRARY_LIMIT
                        || struct_spec
                            .field_tys
                            .iter()
                            .any(|ty| self.needs_structural_kani_seed_expr(*ty));
                }
                false
            }
        }
    }

    fn render_structural_kani_seed_expr_for_ty(&mut self, value_ty: Ty<'tcx>) -> Option<String> {
        let seed_ty = self.kani_seed_ty_for_value_ty(value_ty)?;
        if is_std_io_empty_value_ty(value_ty, self.tcx)
            || is_std_io_sink_value_ty(value_ty, self.tcx)
            || is_std_io_repeat_value_ty(value_ty, self.tcx)
            || is_symbolic_string_value_ty(value_ty, self.tcx)
        {
            return Some(format!("kani::any::<{seed_ty}>()"));
        }
        if let Some(inner) = cursor_inner_ty(value_ty, self.tcx) {
            return self.render_kani_seed_expr_for_value_ty(inner, &seed_ty);
        }
        if let Some(inner) = boxed_inner_ty(value_ty, self.tcx) {
            return self.render_kani_seed_expr_for_value_ty(inner, &seed_ty);
        }
        if let Some(inner) = symbolic_vec_inner_ty(value_ty, self.tcx) {
            if !self.needs_structural_kani_seed_expr(inner) {
                return Some(format!("kani::any::<{seed_ty}>()"));
            }
            let inner_seed_ty = self.kani_seed_ty_for_value_ty(inner)?;
            let values =
                self.render_array_kani_seed_expr(inner, &inner_seed_ty, SYMBOLIC_COLLECTION_LEN)?;
            return Some(format!("({values}, kani::any::<usize>())"));
        }
        if let Some((inner, variant)) = option_inner_ty(value_ty, self.tcx) {
            if !self.needs_structural_kani_seed_expr(inner) {
                return Some(format!("kani::any::<{seed_ty}>()"));
            }
            let inner_seed_ty = self.kani_seed_ty_for_value_ty(inner)?;
            let inner_expr = self.render_kani_seed_expr_for_value_ty(inner, &inner_seed_ty)?;
            return Some(format!(
                "if kani::any::<bool>() {{ {variant}::Some({inner_expr}) }} else {{ {variant}::None }}"
            ));
        }
        if let Some(((ok_ty, err_ty), variant)) = result_inner_tys(value_ty, self.tcx) {
            if !self.needs_structural_kani_seed_expr(ok_ty)
                && !self.needs_structural_kani_seed_expr(err_ty)
            {
                return Some(format!("kani::any::<{seed_ty}>()"));
            }
            let ok_seed_ty = self.kani_seed_ty_for_value_ty(ok_ty)?;
            let err_seed_ty = self.kani_seed_ty_for_value_ty(err_ty)?;
            let ok_expr = self.render_kani_seed_expr_for_value_ty(ok_ty, &ok_seed_ty)?;
            let err_expr = self.render_kani_seed_expr_for_value_ty(err_ty, &err_seed_ty)?;
            return Some(format!(
                "if kani::any::<bool>() {{ {variant}::Ok({ok_expr}) }} else {{ {variant}::Err({err_expr}) }}"
            ));
        }
        if let TyKind::Tuple(types) = value_ty.kind() {
            let exprs = types
                .iter()
                .map(|ty| {
                    let field_seed_ty = self.kani_seed_ty_for_value_ty(ty)?;
                    self.render_kani_seed_expr_for_value_ty(ty, &field_seed_ty)
                })
                .collect::<Option<Vec<_>>>()?;
            return Some(format_tuple_expr(&exprs));
        }
        if let TyKind::Slice(inner) = value_ty.kind() {
            let inner_seed_ty = self.kani_seed_ty_for_value_ty(*inner)?;
            let values =
                self.render_array_kani_seed_expr(*inner, &inner_seed_ty, SYMBOLIC_COLLECTION_LEN)?;
            return Some(format!("({values}, kani::any::<usize>())"));
        }
        if let TyKind::Array(inner, len) = value_ty.kind() {
            let inner_seed_ty = self.kani_seed_ty_for_value_ty(*inner)?;
            let len = self.array_len_value(*len)?;
            return self.render_array_kani_seed_expr(*inner, &inner_seed_ty, len);
        }
        if let Some(enum_spec) = self.local_enum_spec(value_ty) {
            if enum_spec
                .variants
                .iter()
                .all(|variant| variant.field_tys.is_empty())
            {
                return Some(format!("kani::any::<{seed_ty}>()"));
            }
            let mut exprs = vec!["kani::any::<usize>()".to_owned()];
            for variant in enum_spec.variants {
                exprs.push(self.render_kani_seed_tuple_expr_for_fields(&variant.field_tys)?);
            }
            return Some(format_tuple_expr(&exprs));
        }
        if let Some(struct_spec) = self.local_struct_spec(value_ty) {
            return self.render_kani_seed_tuple_expr_for_fields(&struct_spec.field_tys);
        }
        Some(format!("kani::any::<{seed_ty}>()"))
    }

    fn render_array_kani_seed_expr(
        &mut self,
        inner: Ty<'tcx>,
        inner_seed_ty: &str,
        len: usize,
    ) -> Option<String> {
        if self.needs_structural_kani_seed_expr(inner) {
            let inner_expr = self.render_kani_seed_expr_for_value_ty(inner, inner_seed_ty)?;
            Some(format!("::std::array::from_fn(|_| {{ {inner_expr} }})"))
        } else {
            Some(format!("kani::any::<[{inner_seed_ty}; {len}]>()"))
        }
    }

    fn render_kani_seed_tuple_expr_for_fields(&mut self, fields: &[Ty<'tcx>]) -> Option<String> {
        let exprs = fields
            .iter()
            .map(|field_ty| {
                let field_seed_ty = self.kani_seed_ty_for_value_ty(*field_ty)?;
                self.render_kani_seed_expr_for_value_ty(*field_ty, &field_seed_ty)
            })
            .collect::<Option<Vec<_>>>()?;
        Some(format_tuple_expr(&exprs))
    }

    fn render_value_from_symbolic_seed_for_ty(
        &mut self,
        value_ty: Ty<'tcx>,
        seed_binding: &str,
    ) -> Option<String> {
        if is_std_io_empty_value_ty(value_ty, self.tcx) {
            return Some("::std::io::empty()".to_owned());
        }
        if is_std_io_sink_value_ty(value_ty, self.tcx) {
            return Some("::std::io::sink()".to_owned());
        }
        if is_std_io_repeat_value_ty(value_ty, self.tcx) {
            return Some(format!("::std::io::repeat({seed_binding})"));
        }
        if let Some(inner) = cursor_inner_ty(value_ty, self.tcx) {
            let inner_expr = self
                .render_value_from_symbolic_seed_for_ty(inner, seed_binding)
                .unwrap_or_else(|| seed_binding.to_owned());
            return Some(format!("::std::io::Cursor::new({inner_expr})"));
        }
        if is_symbolic_string_value_ty(value_ty, self.tcx) {
            return Some(format!(
                "match {seed_binding} {{ (value, len) => {{ let len = ::core::cmp::min(len, value.len()); ::std::string::String::from_utf8_lossy(&value[..len]).into_owned() }} }}"
            ));
        }
        if let Some(inner) = symbolic_vec_inner_ty(value_ty, self.tcx) {
            let inner_expr = self
                .render_value_from_symbolic_seed_for_ty(inner, "value")
                .unwrap_or_else(|| "value".to_owned());
            return Some(format!(
                "match {seed_binding} {{ (values, len) => {{ let len = ::core::cmp::min(len, values.len()); values.into_iter().take(len).map(|value| {inner_expr}).collect::<::std::vec::Vec<_>>() }} }}"
            ));
        }
        if let Some(inner) = boxed_inner_ty(value_ty, self.tcx) {
            let inner_expr = self
                .render_value_from_symbolic_seed_for_ty(inner, seed_binding)
                .unwrap_or_else(|| seed_binding.to_owned());
            return Some(format!("::std::boxed::Box::new({inner_expr})"));
        }
        if let Some((inner, _variant)) = option_inner_ty(value_ty, self.tcx) {
            let inner_expr = self
                .render_value_from_symbolic_seed_for_ty(inner, "value")
                .unwrap_or_else(|| "value".to_owned());
            return Some(format!("{seed_binding}.map(|value| {inner_expr})"));
        }
        if let Some(((ok_ty, err_ty), variant)) = result_inner_tys(value_ty, self.tcx) {
            let ok_expr = self
                .render_value_from_symbolic_seed_for_ty(ok_ty, "value")
                .unwrap_or_else(|| "value".to_owned());
            let err_expr = self
                .render_value_from_symbolic_seed_for_ty(err_ty, "err")
                .unwrap_or_else(|| "err".to_owned());
            return Some(format!(
                "match {seed_binding} {{ {variant}::Ok(value) => {variant}::Ok({ok_expr}), {variant}::Err(err) => {variant}::Err({err_expr}), }}"
            ));
        }
        if let TyKind::Tuple(types) = value_ty.kind() {
            if types.is_empty() {
                return Some(seed_binding.to_owned());
            }

            let bindings = (0..types.len())
                .map(|index| format!("value{index}"))
                .collect::<Vec<_>>();
            let element_exprs = types
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    let binding = format!("value{index}");
                    self.render_value_from_symbolic_seed_for_ty(ty, &binding)
                        .unwrap_or(binding)
                })
                .collect::<Vec<_>>();
            let identity_exprs = (0..types.len())
                .map(|index| format!("value{index}"))
                .collect::<Vec<_>>();
            if element_exprs == identity_exprs {
                return Some(seed_binding.to_owned());
            }

            return Some(format!(
                "match {seed_binding} {{ {} => {} }}",
                format_tuple_pattern(&bindings),
                format_tuple_expr(&element_exprs),
            ));
        }
        if let TyKind::Slice(inner) = value_ty.kind() {
            let inner_expr = self
                .render_value_from_symbolic_seed_for_ty(*inner, "value")
                .unwrap_or_else(|| "value".to_owned());
            return Some(format!(
                "match {seed_binding} {{ (values, len) => {{ let len = ::core::cmp::min(len, values.len()); values.into_iter().take(len).map(|value| {inner_expr}).collect::<::std::vec::Vec<_>>() }} }}"
            ));
        }
        if let TyKind::Array(inner, _) = value_ty.kind() {
            let inner_expr = self
                .render_value_from_symbolic_seed_for_ty(*inner, "value")
                .unwrap_or_else(|| "value".to_owned());
            if inner_expr == "value" {
                return Some(seed_binding.to_owned());
            }
            return Some(format!("{seed_binding}.map(|value| {inner_expr})"));
        }
        if let Some(enum_spec) = self.local_enum_spec(value_ty) {
            if enum_spec
                .variants
                .iter()
                .all(|variant| variant.field_tys.is_empty())
            {
                let variant_count = enum_spec.variants.len();
                let arms = enum_spec
                    .variants
                    .iter()
                    .enumerate()
                    .map(|(index, variant)| {
                        format!("{index} => {}::{}", enum_spec.enum_expr_path, variant.name)
                    })
                    .collect::<Vec<_>>();
                return Some(format!(
                    "match {seed_binding} % {variant_count} {{ {}, _ => unreachable!(), }}",
                    arms.join(", "),
                ));
            }
            let discr_binding = "variant_discriminant".to_owned();
            let payload_bindings = (0..enum_spec.variants.len())
                .map(|index| format!("variant_payload_{index}"))
                .collect::<Vec<_>>();
            let mut outer_bindings = vec![discr_binding.clone()];
            outer_bindings.extend(payload_bindings.iter().cloned());
            let variant_count = enum_spec.variants.len();
            let arms = enum_spec
                .variants
                .iter()
                .enumerate()
                .map(|(index, variant)| {
                    let payload_binding = &payload_bindings[index];
                    let variant_expr = self.render_local_enum_variant_expr(
                        &enum_spec.enum_expr_path,
                        variant,
                        payload_binding,
                    )?;
                    Some(format!("{index} => {variant_expr}"))
                })
                .collect::<Option<Vec<_>>>()?;
            return Some(format!(
                "match {seed_binding} {{ {} => match {discr_binding} % {variant_count} {{ {}, _ => unreachable!(), }} }}",
                format_tuple_pattern(&outer_bindings),
                arms.join(", "),
            ));
        }
        if let Some(struct_spec) = self.local_struct_spec(value_ty) {
            return self.render_local_struct_expr(&struct_spec, seed_binding);
        }

        Some(seed_binding.to_owned())
    }

    fn needs_symbolic_seed_binding_for_ty(&mut self, value_ty: Ty<'tcx>) -> bool {
        is_std_io_empty_value_ty(value_ty, self.tcx)
            || is_std_io_sink_value_ty(value_ty, self.tcx)
            || is_std_io_repeat_value_ty(value_ty, self.tcx)
            || cursor_inner_ty(value_ty, self.tcx).is_some()
            || is_symbolic_string_value_ty(value_ty, self.tcx)
            || symbolic_vec_inner_ty(value_ty, self.tcx).is_some()
            || boxed_inner_ty(value_ty, self.tcx).is_some()
            || self.local_enum_spec(value_ty).is_some()
            || self.local_struct_spec(value_ty).is_some()
            || option_inner_ty(value_ty, self.tcx)
                .map(|(inner, _)| self.needs_symbolic_seed_binding_for_ty(inner))
                .unwrap_or(false)
            || result_inner_tys(value_ty, self.tcx)
                .map(|((ok_ty, err_ty), _)| {
                    self.needs_symbolic_seed_binding_for_ty(ok_ty)
                        || self.needs_symbolic_seed_binding_for_ty(err_ty)
                })
                .unwrap_or(false)
            || matches!(value_ty.kind(), TyKind::Tuple(types) if types.iter().any(|ty| self.needs_symbolic_seed_binding_for_ty(ty)))
            || matches!(value_ty.kind(), TyKind::Slice(_))
            || matches!(value_ty.kind(), TyKind::Array(inner, _) if self.needs_symbolic_seed_binding_for_ty(*inner))
    }

    fn literal_seed_values_for_value_ty(&mut self, value_ty: Ty<'tcx>) -> Option<Vec<String>> {
        if is_std_io_empty_value_ty(value_ty, self.tcx)
            || is_std_io_sink_value_ty(value_ty, self.tcx)
        {
            return Some(vec!["()".to_owned()]);
        }
        if is_std_io_repeat_value_ty(value_ty, self.tcx) {
            return Some(vec!["0u8".to_owned(), "1u8".to_owned()]);
        }
        if let Some(inner) = cursor_inner_ty(value_ty, self.tcx) {
            return self.literal_seed_values_for_value_ty(inner);
        }
        if is_symbolic_string_value_ty(value_ty, self.tcx) {
            return Some(vec![
                format!("([0u8; {SYMBOLIC_COLLECTION_LEN}], 0usize)"),
                format_tuple_expr(&[
                    "[97u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8]".to_owned(),
                    "1usize".to_owned(),
                ]),
                format!("([65u8; {SYMBOLIC_COLLECTION_LEN}], {SYMBOLIC_COLLECTION_LEN}usize)"),
            ]);
        }
        if let Some(inner) = symbolic_vec_inner_ty(value_ty, self.tcx) {
            let inner_values = self.literal_seed_values_for_value_ty(inner)?;
            return Some(length_prefixed_array_literals(
                &inner_values,
                &SYMBOLIC_COLLECTION_LEN.to_string(),
            ));
        }
        if let Some(inner) = boxed_inner_ty(value_ty, self.tcx) {
            return self.literal_seed_values_for_value_ty(inner);
        }
        if let Some((inner, variant)) = option_inner_ty(value_ty, self.tcx) {
            let inner_values = self.literal_seed_values_for_value_ty(inner)?;
            let mut options = vec![format!("{variant}::None")];
            for value in pick_literal_variants(&inner_values, 2) {
                push_unique_literal(&mut options, format!("{variant}::Some({value})"));
            }
            return Some(options);
        }
        if let Some(((ok_ty, err_ty), variant)) = result_inner_tys(value_ty, self.tcx) {
            let ok_values = self.literal_seed_values_for_value_ty(ok_ty)?;
            let err_values = self.literal_seed_values_for_value_ty(err_ty)?;
            let mut options = Vec::new();
            for value in pick_literal_variants(&ok_values, 2) {
                push_unique_literal(&mut options, format!("{variant}::Ok({value})"));
            }
            for value in pick_literal_variants(&err_values, 2) {
                push_unique_literal(&mut options, format!("{variant}::Err({value})"));
            }
            return Some(options);
        }
        if let TyKind::Tuple(types) = value_ty.kind() {
            let element_values = types
                .iter()
                .map(|ty| self.literal_seed_values_for_value_ty(ty))
                .collect::<Option<Vec<_>>>()?;
            return Some(tuple_literal_options(&element_values));
        }
        if let TyKind::Slice(inner) = value_ty.kind() {
            let inner_values = self.literal_seed_values_for_value_ty(*inner)?;
            return Some(length_prefixed_array_literals(
                &inner_values,
                &SYMBOLIC_COLLECTION_LEN.to_string(),
            ));
        }
        if let TyKind::Array(inner, len) = value_ty.kind() {
            let inner_values = self.literal_seed_values_for_value_ty(*inner)?;
            return Some(repeated_array_literals(
                &inner_values,
                &self.array_len_value(*len)?.to_string(),
            ));
        }
        if let Some(enum_spec) = self.local_enum_spec(value_ty) {
            if enum_spec
                .variants
                .iter()
                .all(|variant| variant.field_tys.is_empty())
            {
                return Some(enum_discriminant_literals(enum_spec.variants.len()));
            }

            let mut element_values = vec![enum_discriminant_literals(enum_spec.variants.len())];
            for variant in enum_spec.variants {
                let field_values = variant
                    .field_tys
                    .iter()
                    .map(|ty| self.literal_seed_values_for_value_ty(*ty))
                    .collect::<Option<Vec<_>>>()?;
                element_values.push(tuple_literal_options(&field_values));
            }
            return Some(tuple_literal_options(&element_values));
        }
        if let Some(struct_spec) = self.local_struct_spec(value_ty) {
            let field_values = struct_spec
                .field_tys
                .iter()
                .map(|ty| self.literal_seed_values_for_value_ty(*ty))
                .collect::<Option<Vec<_>>>()?;
            return Some(tuple_literal_options(&field_values));
        }

        match value_ty.kind() {
            TyKind::Bool => Some(vec!["false".to_owned(), "true".to_owned()]),
            TyKind::Char => Some(vec!["'a'".to_owned(), "'\\0'".to_owned(), "'Z'".to_owned()]),
            TyKind::Int(int_ty) => {
                let suffix = value_ty.to_string();
                let mut options = vec![format!("0{suffix}"), format!("1{suffix}")];
                push_unique_literal(&mut options, format!("-1{suffix}"));
                if !matches!(int_ty, ty::IntTy::Isize) {
                    push_unique_literal(&mut options, format!("{suffix}::MIN"));
                    push_unique_literal(&mut options, format!("{suffix}::MAX"));
                } else {
                    push_unique_literal(&mut options, "isize::MIN".to_owned());
                    push_unique_literal(&mut options, "isize::MAX".to_owned());
                }
                Some(options)
            }
            TyKind::Uint(_) => {
                let suffix = value_ty.to_string();
                let mut options = vec![format!("0{suffix}"), format!("1{suffix}")];
                push_unique_literal(&mut options, format!("{suffix}::MAX"));
                Some(options)
            }
            TyKind::Float(_) => {
                let suffix = value_ty.to_string();
                let mut options = vec![
                    format!("0.0{suffix}"),
                    format!("1.0{suffix}"),
                    format!("-1.0{suffix}"),
                ];
                push_unique_literal(&mut options, format!("{suffix}::MIN"));
                push_unique_literal(&mut options, format!("{suffix}::MAX"));
                Some(options)
            }
            _ => None,
        }
    }

    fn fuzz_seed_byte_len_for_value_ty(&mut self, value_ty: Ty<'tcx>) -> Option<usize> {
        if is_std_io_empty_value_ty(value_ty, self.tcx)
            || is_std_io_sink_value_ty(value_ty, self.tcx)
        {
            return Some(0);
        }
        if is_std_io_repeat_value_ty(value_ty, self.tcx) {
            return Some(1);
        }
        if let Some(inner) = cursor_inner_ty(value_ty, self.tcx) {
            return self.fuzz_seed_byte_len_for_value_ty(inner);
        }
        if is_symbolic_string_value_ty(value_ty, self.tcx) {
            return Some(SYMBOLIC_COLLECTION_LEN + self.pointer_byte_len());
        }
        if let Some(inner) = symbolic_vec_inner_ty(value_ty, self.tcx) {
            let inner_len = self.fuzz_seed_byte_len_for_value_ty(inner)?;
            return Some(inner_len * SYMBOLIC_COLLECTION_LEN + self.pointer_byte_len());
        }
        if let Some(inner) = boxed_inner_ty(value_ty, self.tcx) {
            return self.fuzz_seed_byte_len_for_value_ty(inner);
        }
        if let Some((inner, _)) = option_inner_ty(value_ty, self.tcx) {
            return Some(1 + self.fuzz_seed_byte_len_for_value_ty(inner)?);
        }
        if let Some(((ok_ty, err_ty), _)) = result_inner_tys(value_ty, self.tcx) {
            return Some(
                1 + self.fuzz_seed_byte_len_for_value_ty(ok_ty)?
                    + self.fuzz_seed_byte_len_for_value_ty(err_ty)?,
            );
        }
        if let TyKind::Tuple(types) = value_ty.kind() {
            let mut len = 0;
            for ty in types.iter() {
                len += self.fuzz_seed_byte_len_for_value_ty(ty)?;
            }
            return Some(len);
        }
        if let TyKind::Slice(inner) = value_ty.kind() {
            let inner_len = self.fuzz_seed_byte_len_for_value_ty(*inner)?;
            return Some(inner_len * SYMBOLIC_COLLECTION_LEN + self.pointer_byte_len());
        }
        if let TyKind::Array(inner, len) = value_ty.kind() {
            let inner_len = self.fuzz_seed_byte_len_for_value_ty(*inner)?;
            return Some(inner_len * self.array_len_value(*len)?);
        }
        if let Some(enum_spec) = self.local_enum_spec(value_ty) {
            if enum_spec
                .variants
                .iter()
                .all(|variant| variant.field_tys.is_empty())
            {
                return Some(self.pointer_byte_len());
            }

            let mut len = self.pointer_byte_len();
            for variant in enum_spec.variants {
                for field_ty in variant.field_tys {
                    len += self.fuzz_seed_byte_len_for_value_ty(field_ty)?;
                }
            }
            return Some(len);
        }
        if let Some(struct_spec) = self.local_struct_spec(value_ty) {
            let mut len = 0;
            for field_ty in struct_spec.field_tys {
                len += self.fuzz_seed_byte_len_for_value_ty(field_ty)?;
            }
            return Some(len);
        }

        match value_ty.kind() {
            TyKind::Bool => Some(1),
            TyKind::Char | TyKind::Int(ty::IntTy::I32) | TyKind::Uint(ty::UintTy::U32) => Some(4),
            TyKind::Int(ty::IntTy::I8) | TyKind::Uint(ty::UintTy::U8) => Some(1),
            TyKind::Int(ty::IntTy::I16) | TyKind::Uint(ty::UintTy::U16) => Some(2),
            TyKind::Int(ty::IntTy::I64) | TyKind::Uint(ty::UintTy::U64) => Some(8),
            TyKind::Int(ty::IntTy::Isize) | TyKind::Uint(ty::UintTy::Usize) => {
                Some(self.pointer_byte_len())
            }
            TyKind::Int(ty::IntTy::I128) | TyKind::Uint(ty::UintTy::U128) => Some(16),
            TyKind::Float(ty::FloatTy::F32) => Some(4),
            TyKind::Float(ty::FloatTy::F64) => Some(8),
            _ => None,
        }
    }

    fn render_fuzz_seed_from_data_for_value_ty(
        &mut self,
        value_ty: Ty<'tcx>,
        data_binding: &str,
        start_index: usize,
    ) -> Option<String> {
        if is_std_io_empty_value_ty(value_ty, self.tcx)
            || is_std_io_sink_value_ty(value_ty, self.tcx)
        {
            return Some("()".to_owned());
        }
        if is_std_io_repeat_value_ty(value_ty, self.tcx) {
            return Some(format!("_to_u8({data_binding}, {start_index})"));
        }
        if let Some(inner) = cursor_inner_ty(value_ty, self.tcx) {
            return self.render_fuzz_seed_from_data_for_value_ty(inner, data_binding, start_index);
        }
        if is_symbolic_string_value_ty(value_ty, self.tcx) {
            return Some(format!(
                "({}, _to_usize({data_binding}, {}))",
                render_fuzz_u8_array(data_binding, start_index, SYMBOLIC_COLLECTION_LEN),
                start_index + SYMBOLIC_COLLECTION_LEN
            ));
        }
        if let Some(inner) = symbolic_vec_inner_ty(value_ty, self.tcx) {
            return self.render_length_prefixed_fuzz_seed_array(
                inner,
                data_binding,
                start_index,
                SYMBOLIC_COLLECTION_LEN,
            );
        }
        if let Some(inner) = boxed_inner_ty(value_ty, self.tcx) {
            return self.render_fuzz_seed_from_data_for_value_ty(inner, data_binding, start_index);
        }
        if let Some((inner, variant)) = option_inner_ty(value_ty, self.tcx) {
            let inner_expr =
                self.render_fuzz_seed_from_data_for_value_ty(inner, data_binding, start_index + 1)?;
            return Some(format!(
                "if _to_bool({data_binding}, {start_index}) {{ {variant}::Some({inner_expr}) }} else {{ {variant}::None }}"
            ));
        }
        if let Some(((ok_ty, err_ty), variant)) = result_inner_tys(value_ty, self.tcx) {
            let ok_start = start_index + 1;
            let err_start = ok_start + self.fuzz_seed_byte_len_for_value_ty(ok_ty)?;
            let ok_expr =
                self.render_fuzz_seed_from_data_for_value_ty(ok_ty, data_binding, ok_start)?;
            let err_expr =
                self.render_fuzz_seed_from_data_for_value_ty(err_ty, data_binding, err_start)?;
            return Some(format!(
                "if _to_bool({data_binding}, {start_index}) {{ {variant}::Ok({ok_expr}) }} else {{ {variant}::Err({err_expr}) }}"
            ));
        }
        if let TyKind::Tuple(types) = value_ty.kind() {
            let mut offset = start_index;
            let mut exprs = Vec::new();
            for ty in types.iter() {
                exprs.push(self.render_fuzz_seed_from_data_for_value_ty(
                    ty,
                    data_binding,
                    offset,
                )?);
                offset += self.fuzz_seed_byte_len_for_value_ty(ty)?;
            }
            return Some(format_tuple_expr(&exprs));
        }
        if let TyKind::Slice(inner) = value_ty.kind() {
            return self.render_length_prefixed_fuzz_seed_array(
                *inner,
                data_binding,
                start_index,
                SYMBOLIC_COLLECTION_LEN,
            );
        }
        if let TyKind::Array(inner, len) = value_ty.kind() {
            let count = self.array_len_value(*len)?;
            return self.render_fuzz_seed_array(*inner, data_binding, start_index, count);
        }
        if let Some(enum_spec) = self.local_enum_spec(value_ty) {
            if enum_spec
                .variants
                .iter()
                .all(|variant| variant.field_tys.is_empty())
            {
                return Some(format!("_to_usize({data_binding}, {start_index})"));
            }

            let mut elements = vec![format!("_to_usize({data_binding}, {start_index})")];
            let mut offset = start_index + self.pointer_byte_len();
            for variant in enum_spec.variants {
                let mut field_exprs = Vec::new();
                for field_ty in variant.field_tys {
                    field_exprs.push(self.render_fuzz_seed_from_data_for_value_ty(
                        field_ty,
                        data_binding,
                        offset,
                    )?);
                    offset += self.fuzz_seed_byte_len_for_value_ty(field_ty)?;
                }
                elements.push(format_tuple_expr(&field_exprs));
            }
            return Some(format_tuple_expr(&elements));
        }
        if let Some(struct_spec) = self.local_struct_spec(value_ty) {
            let mut offset = start_index;
            let mut field_exprs = Vec::new();
            for field_ty in struct_spec.field_tys {
                field_exprs.push(self.render_fuzz_seed_from_data_for_value_ty(
                    field_ty,
                    data_binding,
                    offset,
                )?);
                offset += self.fuzz_seed_byte_len_for_value_ty(field_ty)?;
            }
            return Some(format_tuple_expr(&field_exprs));
        }

        match value_ty.kind() {
            TyKind::Bool => Some(format!("_to_bool({data_binding}, {start_index})")),
            TyKind::Char => Some(format!("_to_char({data_binding}, {start_index})")),
            TyKind::Int(ty::IntTy::I8) => Some(format!("_to_i8({data_binding}, {start_index})")),
            TyKind::Int(ty::IntTy::I16) => Some(format!("_to_i16({data_binding}, {start_index})")),
            TyKind::Int(ty::IntTy::I32) => Some(format!("_to_i32({data_binding}, {start_index})")),
            TyKind::Int(ty::IntTy::I64) => Some(format!("_to_i64({data_binding}, {start_index})")),
            TyKind::Int(ty::IntTy::I128) => {
                Some(format!("_to_i128({data_binding}, {start_index})"))
            }
            TyKind::Int(ty::IntTy::Isize) => {
                Some(format!("_to_isize({data_binding}, {start_index})"))
            }
            TyKind::Uint(ty::UintTy::U8) => Some(format!("_to_u8({data_binding}, {start_index})")),
            TyKind::Uint(ty::UintTy::U16) => {
                Some(format!("_to_u16({data_binding}, {start_index})"))
            }
            TyKind::Uint(ty::UintTy::U32) => {
                Some(format!("_to_u32({data_binding}, {start_index})"))
            }
            TyKind::Uint(ty::UintTy::U64) => {
                Some(format!("_to_u64({data_binding}, {start_index})"))
            }
            TyKind::Uint(ty::UintTy::U128) => {
                Some(format!("_to_u128({data_binding}, {start_index})"))
            }
            TyKind::Uint(ty::UintTy::Usize) => {
                Some(format!("_to_usize({data_binding}, {start_index})"))
            }
            TyKind::Float(ty::FloatTy::F32) => {
                Some(format!("_to_f32({data_binding}, {start_index})"))
            }
            TyKind::Float(ty::FloatTy::F64) => {
                Some(format!("_to_f64({data_binding}, {start_index})"))
            }
            _ => None,
        }
    }

    fn render_length_prefixed_fuzz_seed_array(
        &mut self,
        inner: Ty<'tcx>,
        data_binding: &str,
        start_index: usize,
        count: usize,
    ) -> Option<String> {
        let array = self.render_fuzz_seed_array(inner, data_binding, start_index, count)?;
        let len_start = start_index + self.fuzz_seed_byte_len_for_value_ty(inner)? * count;
        Some(format!("({array}, _to_usize({data_binding}, {len_start}))"))
    }

    fn render_fuzz_seed_array(
        &mut self,
        inner: Ty<'tcx>,
        data_binding: &str,
        start_index: usize,
        count: usize,
    ) -> Option<String> {
        let inner_len = self.fuzz_seed_byte_len_for_value_ty(inner)?;
        let elements = (0..count)
            .map(|index| {
                self.render_fuzz_seed_from_data_for_value_ty(
                    inner,
                    data_binding,
                    start_index + index * inner_len,
                )
            })
            .collect::<Option<Vec<_>>>()?;
        Some(format!("[{}]", elements.join(", ")))
    }

    fn local_enum_spec(&mut self, value_ty: Ty<'tcx>) -> Option<LocalEnumSpec<'tcx>> {
        let TyKind::Adt(def, args) = value_ty.kind() else {
            return None;
        };
        if !def.is_enum()
            || (def.did().is_local() && def.is_variant_list_non_exhaustive())
            || (!def.did().is_local() && !enum_is_publicly_constructible(*def, self.tcx))
            || !self.has_usable_adt_path(*def)
        {
            return None;
        }
        if self.local_adt_stack.contains(&def.did()) {
            return None;
        }

        self.local_adt_stack.push(def.did());
        let spec = self.build_local_enum_spec(*def, args);
        self.local_adt_stack.pop();
        spec
    }

    fn local_struct_spec(&mut self, value_ty: Ty<'tcx>) -> Option<LocalStructSpec<'tcx>> {
        let TyKind::Adt(def, args) = value_ty.kind() else {
            return None;
        };
        if !def.is_struct()
            || !struct_is_publicly_constructible(*def, self.tcx)
            || !self.has_usable_adt_path(*def)
        {
            return None;
        }
        if self.local_adt_stack.contains(&def.did()) {
            return None;
        }

        self.local_adt_stack.push(def.did());
        let spec = self.build_local_struct_spec(*def, args);
        self.local_adt_stack.pop();
        spec
    }

    fn build_local_enum_spec(
        &mut self,
        def: AdtDef<'tcx>,
        args: GenericArgsRef<'tcx>,
    ) -> Option<LocalEnumSpec<'tcx>> {
        let mut variants = Vec::new();
        for variant in def.variants().iter() {
            if variant.is_field_list_non_exhaustive() {
                return None;
            }
            variants.push(self.build_local_enum_variant_spec(variant, args)?);
        }
        if variants.is_empty() {
            return None;
        }

        Some(LocalEnumSpec {
            enum_expr_path: self.qualified_adt_expr_path(def, args),
            variants,
        })
    }

    fn build_local_enum_variant_spec(
        &mut self,
        variant: &VariantDef,
        args: GenericArgsRef<'tcx>,
    ) -> Option<LocalEnumVariantSpec<'tcx>> {
        let ctor_style = if variant.fields.is_empty() {
            LocalEnumVariantStyle::Unit
        } else if variant.ctor_kind().is_some() {
            LocalEnumVariantStyle::Tuple
        } else {
            LocalEnumVariantStyle::Struct
        };

        let mut field_names = Vec::new();
        let mut field_tys = Vec::new();
        let mut field_seed_tys = Vec::new();
        for field in variant.fields.iter() {
            let field_ty = field.ty(self.tcx, args);
            let field_seed_ty = self.kani_seed_ty_for_value_ty(field_ty)?;
            field_names.push(field.name.to_string());
            field_tys.push(field_ty);
            field_seed_tys.push(field_seed_ty);
        }

        Some(LocalEnumVariantSpec {
            name: variant.name.to_string(),
            style: ctor_style,
            field_names,
            field_tys,
            field_seed_tys,
        })
    }

    fn build_local_struct_spec(
        &mut self,
        def: AdtDef<'tcx>,
        args: GenericArgsRef<'tcx>,
    ) -> Option<LocalStructSpec<'tcx>> {
        let variant = def.non_enum_variant();
        if variant.is_field_list_non_exhaustive() {
            return None;
        }

        let style = if variant.fields.is_empty() {
            LocalStructStyle::Unit
        } else if variant.ctor_kind().is_some() {
            LocalStructStyle::Tuple
        } else {
            LocalStructStyle::Named
        };

        let mut field_names = Vec::new();
        let mut field_tys = Vec::new();
        let mut field_seed_tys = Vec::new();
        for field in variant.fields.iter() {
            let field_ty = field.ty(self.tcx, args);
            let field_seed_ty = self.kani_seed_ty_for_value_ty(field_ty)?;
            field_names.push(field.name.to_string());
            field_tys.push(field_ty);
            field_seed_tys.push(field_seed_ty);
        }

        Some(LocalStructSpec {
            struct_expr_path: self.qualified_adt_expr_path(def, args),
            style,
            field_names,
            field_tys,
            field_seed_tys,
        })
    }

    fn render_local_enum_variant_expr(
        &mut self,
        enum_path: &str,
        variant: &LocalEnumVariantSpec<'tcx>,
        payload_binding: &str,
    ) -> Option<String> {
        if variant.field_tys.is_empty() {
            return Some(format!("{enum_path}::{}", variant.name));
        }

        let bindings = (0..variant.field_tys.len())
            .map(|index| format!("field{index}"))
            .collect::<Vec<_>>();
        let field_exprs = variant
            .field_tys
            .iter()
            .enumerate()
            .map(|(index, field_ty)| {
                let binding = &bindings[index];
                self.render_value_from_symbolic_seed_for_ty(*field_ty, binding)
                    .unwrap_or_else(|| binding.clone())
            })
            .collect::<Vec<_>>();

        let variant_expr = match variant.style {
            LocalEnumVariantStyle::Unit => unreachable!(),
            LocalEnumVariantStyle::Tuple => {
                format!("{enum_path}::{}({})", variant.name, field_exprs.join(", "))
            }
            LocalEnumVariantStyle::Struct => format!(
                "{enum_path}::{} {{ {} }}",
                variant.name,
                variant
                    .field_names
                    .iter()
                    .zip(field_exprs.iter())
                    .map(|(field_name, field_expr)| format!("{field_name}: {field_expr}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };

        Some(format!(
            "match {payload_binding} {{ {} => {variant_expr} }}",
            format_tuple_pattern(&bindings),
        ))
    }

    fn render_local_struct_expr(
        &mut self,
        struct_spec: &LocalStructSpec<'tcx>,
        seed_binding: &str,
    ) -> Option<String> {
        if struct_spec.field_tys.is_empty() {
            return Some(struct_spec.struct_expr_path.clone());
        }

        let bindings = (0..struct_spec.field_tys.len())
            .map(|index| format!("field{index}"))
            .collect::<Vec<_>>();
        let field_exprs = struct_spec
            .field_tys
            .iter()
            .enumerate()
            .map(|(index, field_ty)| {
                let binding = &bindings[index];
                self.render_value_from_symbolic_seed_for_ty(*field_ty, binding)
                    .unwrap_or_else(|| binding.clone())
            })
            .collect::<Vec<_>>();
        let struct_expr = match struct_spec.style {
            LocalStructStyle::Unit => unreachable!(),
            LocalStructStyle::Tuple => {
                format!(
                    "{}({})",
                    struct_spec.struct_expr_path,
                    field_exprs.join(", ")
                )
            }
            LocalStructStyle::Named => format!(
                "{} {{ {} }}",
                struct_spec.struct_expr_path,
                struct_spec
                    .field_names
                    .iter()
                    .zip(field_exprs.iter())
                    .map(|(field_name, field_expr)| format!("{field_name}: {field_expr}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };

        Some(format!(
            "match {seed_binding} {{ {} => {struct_expr} }}",
            format_tuple_pattern(&bindings),
        ))
    }

    fn qualified_adt_path(&self, def: AdtDef<'tcx>, args: GenericArgsRef<'tcx>) -> String {
        self.render_qualified_adt_path(def, args, false)
    }

    fn qualified_adt_expr_path(&self, def: AdtDef<'tcx>, args: GenericArgsRef<'tcx>) -> String {
        self.render_qualified_adt_path(def, args, true)
    }

    fn render_qualified_adt_path(
        &self,
        def: AdtDef<'tcx>,
        args: GenericArgsRef<'tcx>,
        expr_path: bool,
    ) -> String {
        let base = self
            .graph
            .and_then(|graph| graph.public_path(def.did()))
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| self.tcx.def_path_str(def.did()));
        let crate_name = self.tcx.crate_name(def.did().krate).to_string();
        let qualified_base = if base == crate_name || base.starts_with(&format!("{crate_name}::")) {
            base
        } else {
            format!("{crate_name}::{base}")
        };
        let rendered_args = args
            .iter()
            .map(|arg| match arg.kind() {
                ty::GenericArgKind::Type(ty) => self.qualified_ty_path(ty),
                ty::GenericArgKind::Const(ct) => ct.to_string(),
                ty::GenericArgKind::Lifetime(_) => "'_".to_owned(),
            })
            .collect::<Vec<_>>();
        if rendered_args.is_empty() {
            qualified_base
        } else if expr_path {
            format!("{qualified_base}::<{}>", rendered_args.join(", "))
        } else {
            format!("{qualified_base}<{}>", rendered_args.join(", "))
        }
    }

    fn has_usable_adt_path(&self, def: AdtDef<'tcx>) -> bool {
        def.did().is_local()
            || self
                .graph
                .and_then(|graph| graph.public_path(def.did()))
                .is_some()
    }

    fn qualified_ty_path(&self, ty: Ty<'tcx>) -> String {
        match ty.kind() {
            TyKind::Adt(def, args) => self.qualified_adt_path(*def, args),
            TyKind::Ref(_, inner, mutbl) => match mutbl {
                ty::Mutability::Not => format!("&{}", self.qualified_ty_path(*inner)),
                ty::Mutability::Mut => format!("&mut {}", self.qualified_ty_path(*inner)),
            },
            TyKind::Tuple(types) => {
                let rendered = types
                    .iter()
                    .map(|ty| self.qualified_ty_path(ty))
                    .collect::<Vec<_>>();
                if rendered.len() == 1 {
                    format!("({},)", rendered[0])
                } else {
                    format!("({})", rendered.join(", "))
                }
            }
            _ => ty.to_string(),
        }
    }
}

fn struct_is_publicly_constructible(def: AdtDef<'_>, tcx: TyCtxt<'_>) -> bool {
    if !tcx.visibility(def.did()).is_public() {
        return false;
    }

    let variant = def.non_enum_variant();
    !variant.is_field_list_non_exhaustive()
        && variant.fields.iter().all(|field| field.vis.is_public())
}

fn enum_is_publicly_constructible(def: AdtDef<'_>, tcx: TyCtxt<'_>) -> bool {
    if !tcx.visibility(def.did()).is_public() || def.is_variant_list_non_exhaustive() {
        return false;
    }

    def.variants().iter().all(|variant| {
        !variant.is_field_list_non_exhaustive()
            && variant.fields.iter().all(|field| field.vis.is_public())
    })
}

struct LocalEnumSpec<'tcx> {
    enum_expr_path: String,
    variants: Vec<LocalEnumVariantSpec<'tcx>>,
}

struct LocalEnumVariantSpec<'tcx> {
    name: String,
    style: LocalEnumVariantStyle,
    field_names: Vec<String>,
    field_tys: Vec<Ty<'tcx>>,
    field_seed_tys: Vec<String>,
}

#[derive(Clone, Copy)]
enum LocalEnumVariantStyle {
    Unit,
    Tuple,
    Struct,
}

struct LocalStructSpec<'tcx> {
    struct_expr_path: String,
    style: LocalStructStyle,
    field_names: Vec<String>,
    field_tys: Vec<Ty<'tcx>>,
    field_seed_tys: Vec<String>,
}

#[derive(Clone, Copy)]
enum LocalStructStyle {
    Unit,
    Tuple,
    Named,
}

fn format_tuple_seed_ty(elements: &[String]) -> String {
    match elements {
        [] => "()".to_owned(),
        [only] => format!("({only},)"),
        _ => format!("({})", elements.join(", ")),
    }
}

fn format_tuple_pattern(bindings: &[String]) -> String {
    match bindings {
        [] => "()".to_owned(),
        [only] => format!("({only},)"),
        _ => format!("({})", bindings.join(", ")),
    }
}

fn format_tuple_expr(expressions: &[String]) -> String {
    match expressions {
        [] => "()".to_owned(),
        [only] => format!("({only},)"),
        _ => format!("({})", expressions.join(", ")),
    }
}

fn render_fuzz_u8_array(data_binding: &str, start_index: usize, len: usize) -> String {
    let elements = (0..len)
        .map(|index| format!("_to_u8({data_binding}, {})", start_index + index))
        .collect::<Vec<_>>();
    format!("[{}]", elements.join(", "))
}

fn tuple_literal_options(elements: &[Vec<String>]) -> Vec<String> {
    if elements.is_empty() {
        return vec!["()".to_owned()];
    }

    let first = elements
        .iter()
        .map(|values| values[0].clone())
        .collect::<Vec<_>>();
    let second = elements
        .iter()
        .map(|values| values.get(1).cloned().unwrap_or_else(|| values[0].clone()))
        .collect::<Vec<_>>();
    let last = elements
        .iter()
        .map(|values| values.last().cloned().unwrap_or_else(|| values[0].clone()))
        .collect::<Vec<_>>();

    let mut options = Vec::new();
    push_unique_literal(&mut options, format_tuple_expr(&first));
    push_unique_literal(&mut options, format_tuple_expr(&second));
    push_unique_literal(&mut options, format_tuple_expr(&last));

    for index in 0..elements.len() {
        let mut variant = first.clone();
        if let Some(last_value) = elements[index].last()
            && *last_value != variant[index]
        {
            variant[index] = last_value.clone();
            push_unique_literal(&mut options, format_tuple_expr(&variant));
        }
        if options.len() >= MAX_COMPOSITE_LITERAL_OPTIONS {
            break;
        }
    }

    options.truncate(MAX_COMPOSITE_LITERAL_OPTIONS);
    options
}

fn repeated_array_literals(values: &[String], len: &str) -> Vec<String> {
    let mut options = Vec::new();
    for value in pick_literal_variants(values, 3) {
        push_unique_literal(&mut options, format!("[{value}; {len}]"));
    }
    options
}

fn length_prefixed_array_literals(values: &[String], len: &str) -> Vec<String> {
    let mut options = Vec::new();
    let variants = pick_literal_variants(values, 3);
    if let Some(first) = variants.first() {
        push_unique_literal(&mut options, format!("([{first}; {len}], 0usize)"));
        push_unique_literal(&mut options, format!("([{first}; {len}], 1usize)"));
    }
    if let Some(second) = variants.get(1) {
        push_unique_literal(
            &mut options,
            format!("([{second}; {len}], {SYMBOLIC_COLLECTION_LEN}usize)"),
        );
    }
    if let Some(last) = variants.last() {
        push_unique_literal(
            &mut options,
            format!("([{last}; {len}], {SYMBOLIC_COLLECTION_LEN}usize)"),
        );
    }
    options
}

fn enum_discriminant_literals(variant_count: usize) -> Vec<String> {
    if variant_count == 0 {
        return vec!["0usize".to_owned()];
    }

    let mut options = Vec::new();
    for index in 0..variant_count.min(4) {
        push_unique_literal(&mut options, format!("{index}usize"));
    }
    push_unique_literal(&mut options, format!("{}usize", variant_count - 1));
    options
}

fn pick_literal_variants(values: &[String], max: usize) -> Vec<String> {
    let mut variants = Vec::new();
    if let Some(first) = values.first() {
        push_unique_literal(&mut variants, first.clone());
    }
    if let Some(second) = values.get(1) {
        push_unique_literal(&mut variants, second.clone());
    }
    if let Some(last) = values.last() {
        push_unique_literal(&mut variants, last.clone());
    }
    variants.truncate(max);
    variants
}

fn push_unique_literal(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn symbolic_vec_inner_ty<'tcx>(value_ty: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> Option<Ty<'tcx>> {
    let TyKind::Adt(def, args) = value_ty.kind() else {
        return None;
    };
    if !tcx.is_diagnostic_item(sym::Vec, def.did()) {
        return None;
    }
    args.types().next()
}

fn boxed_inner_ty<'tcx>(value_ty: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> Option<Ty<'tcx>> {
    let TyKind::Adt(def, args) = value_ty.kind() else {
        return None;
    };
    if !tcx.is_lang_item(def.did(), LangItem::OwnedBox) {
        return None;
    }
    args.types().next()
}

fn cursor_inner_ty<'tcx>(value_ty: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> Option<Ty<'tcx>> {
    let TyKind::Adt(def, args) = value_ty.kind() else {
        return None;
    };
    if !tcx.def_path_str(def.did()).ends_with("::io::Cursor") {
        return None;
    }
    args.types().next()
}

fn std_io_cursor_value_ty<'tcx>(tcx: TyCtxt<'tcx>, inner: Ty<'tcx>) -> Option<Ty<'tcx>> {
    let cursor_def_id =
        find_std_item_def_id(tcx, &["std", "core"], &["io", "Cursor"]).or_else(|| {
            find_std_item_def_id_by_suffixes(
                tcx,
                &["std", "core"],
                &["::io::Cursor", "::io::cursor::Cursor"],
            )
        })?;
    Some(Ty::new_adt(
        tcx,
        tcx.adt_def(cursor_def_id),
        tcx.mk_args(&[inner.into()]),
    ))
}

fn std_io_empty_value_ty<'tcx>(tcx: TyCtxt<'tcx>) -> Option<Ty<'tcx>> {
    let empty_def_id = find_std_item_def_id(tcx, &["std", "core"], &["io", "Empty"])?;
    Some(Ty::new_adt(
        tcx,
        tcx.adt_def(empty_def_id),
        tcx.mk_args(&[]),
    ))
}

fn std_io_sink_value_ty<'tcx>(tcx: TyCtxt<'tcx>) -> Option<Ty<'tcx>> {
    let sink_def_id = find_std_item_def_id(tcx, &["std", "core"], &["io", "Sink"])?;
    Some(Ty::new_adt(tcx, tcx.adt_def(sink_def_id), tcx.mk_args(&[])))
}

fn std_io_repeat_value_ty<'tcx>(tcx: TyCtxt<'tcx>) -> Option<Ty<'tcx>> {
    let repeat_def_id = find_std_item_def_id(tcx, &["std", "core"], &["io", "Repeat"])?;
    Some(Ty::new_adt(
        tcx,
        tcx.adt_def(repeat_def_id),
        tcx.mk_args(&[]),
    ))
}

fn is_std_io_empty_value_ty<'tcx>(value_ty: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    let TyKind::Adt(def, _) = value_ty.kind() else {
        return false;
    };
    tcx.def_path_str(def.did()).ends_with("::io::Empty")
}

fn is_std_io_sink_value_ty<'tcx>(value_ty: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    let TyKind::Adt(def, _) = value_ty.kind() else {
        return false;
    };
    tcx.def_path_str(def.did()).ends_with("::io::Sink")
}

fn is_std_io_repeat_value_ty<'tcx>(value_ty: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    let TyKind::Adt(def, _) = value_ty.kind() else {
        return false;
    };
    tcx.def_path_str(def.did()).ends_with("::io::Repeat")
}

fn is_std_io_error_value_ty<'tcx>(value_ty: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    let TyKind::Adt(def, _) = value_ty.kind() else {
        return false;
    };
    let path = tcx.def_path_str(def.did());
    path.ends_with("::io::Error") || path.ends_with("::io::error::Error")
}

fn find_std_item_def_id(tcx: TyCtxt<'_>, crate_names: &[&str], path: &[&str]) -> Option<DefId> {
    for &crate_num in tcx.crates(()).iter() {
        let crate_name = tcx.crate_name(crate_num);
        if !crate_names
            .iter()
            .any(|candidate| *candidate == crate_name.as_str())
        {
            continue;
        }

        let mut current = DefId {
            krate: crate_num,
            index: CRATE_DEF_INDEX,
        };
        let mut resolved = true;
        for segment in path {
            let Some(child) = tcx
                .module_children(current)
                .iter()
                .find(|child| child.ident.name.as_str() == *segment)
            else {
                resolved = false;
                break;
            };
            let Some(def_id) = child.res.opt_def_id() else {
                resolved = false;
                break;
            };
            current = def_id;
        }
        if resolved {
            return Some(current);
        }
    }
    None
}

fn find_std_item_def_id_by_suffixes(
    tcx: TyCtxt<'_>,
    crate_names: &[&str],
    suffixes: &[&str],
) -> Option<DefId> {
    for &crate_num in tcx.crates(()).iter() {
        let crate_name = tcx.crate_name(crate_num);
        if !crate_names
            .iter()
            .any(|candidate| *candidate == crate_name.as_str())
        {
            continue;
        }

        let mut stack = vec![DefId {
            krate: crate_num,
            index: CRATE_DEF_INDEX,
        }];
        while let Some(current) = stack.pop() {
            for child in tcx.module_children(current) {
                let Some(def_id) = child.res.opt_def_id() else {
                    continue;
                };
                let path = tcx.def_path_str(def_id);
                if suffixes.iter().any(|suffix| path.ends_with(suffix)) {
                    return Some(def_id);
                }
                stack.push(def_id);
            }
        }
    }
    None
}

fn option_inner_ty<'tcx>(
    value_ty: Ty<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> Option<(Ty<'tcx>, &'static str)> {
    let TyKind::Adt(def, args) = value_ty.kind() else {
        return None;
    };
    if !tcx.is_diagnostic_item(sym::Option, def.did()) {
        return None;
    }
    let variant = if tcx.crate_name(def.did().krate).as_str() == "std" {
        "std::option::Option"
    } else {
        "core::option::Option"
    };
    Some((args.types().next()?, variant))
}

fn result_inner_tys<'tcx>(
    value_ty: Ty<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> Option<((Ty<'tcx>, Ty<'tcx>), &'static str)> {
    let TyKind::Adt(def, args) = value_ty.kind() else {
        return None;
    };
    if !tcx.is_diagnostic_item(sym::Result, def.did()) {
        return None;
    }
    let variant = if tcx.crate_name(def.did().krate).as_str() == "std" {
        "std::result::Result"
    } else {
        "core::result::Result"
    };
    let mut types = args.types();
    Some(((types.next()?, types.next()?), variant))
}

fn is_symbolic_string_value_ty<'tcx>(value_ty: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    let TyKind::Adt(def, _) = value_ty.kind() else {
        return false;
    };
    tcx.is_lang_item(def.did(), LangItem::String)
}
