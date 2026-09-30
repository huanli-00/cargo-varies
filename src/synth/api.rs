use super::{ApiDescriptor, ApiSafety, OutputAdapter};
use crate::rapx_graph::{ApiDependencyGraph, DepEdge, DepNode, TyWrapper};
use crate::unsafe_analysis::ApiFieldFactsIndex;
use rustc_hir::LangItem;
use rustc_hir::def_id::{DefId, LOCAL_CRATE};
use rustc_hir::intravisit::{self, Visitor};
use rustc_middle::ty::{self, AssocContainer, Ty, TyCtxt, TyKind, TypeVisitableExt};
use rustc_span::sym;

pub(super) fn build_api_descriptors<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    field_facts_index: &ApiFieldFactsIndex,
) -> Vec<ApiDescriptor<'tcx>> {
    let mut apis = Vec::new();

    for api_index in 0..graph.num_api() {
        let (def_id, args) = graph.api_at(api_index);
        let node_index = graph
            .get_index(DepNode::api(def_id, args))
            .expect("api node should exist in RAPx graph");

        let mut inputs: Vec<Option<Ty<'tcx>>> = Vec::new();
        let mut output = tcx.types.unit;

        for (source, edge) in graph.incoming_edges(node_index) {
            if let DepEdge::Arg(slot) = *edge {
                if inputs.len() <= slot {
                    inputs.resize(slot + 1, None);
                }
                inputs[slot] = Some(graph.node_at(*source).expect_ty().ty());
            }
        }

        for (target, edge) in graph.outgoing_edges(node_index) {
            if matches!(edge, DepEdge::Ret) {
                output = graph.node_at(*target).expect_ty().ty();
            }
        }

        let path = render_api_path(tcx, graph, def_id, args);
        let target_family = render_target_family_path(tcx, graph, def_id, args);
        let instantiated_path = tcx.def_path_str_with_args(def_id, args);
        let identity_args = ty::GenericArgs::identity_for_item(tcx, def_id);
        let is_mono =
            tcx.generics_of(def_id).requires_monomorphization(tcx) && args != identity_args;
        let concrete_args = render_concrete_args(tcx, graph, args, identity_args);
        let field_facts = field_facts_index.field_facts_for(def_id);
        let api_safety = classify_api_safety(def_id, tcx, field_facts_index);
        let supported = is_supported_callable(tcx, graph, def_id, args, &path, api_safety);
        let (value_output, output_adapter) = adapted_output(tcx, output);

        apis.push(ApiDescriptor {
            index: api_index,
            path,
            target_family,
            instantiated_path,
            concrete_args,
            is_mono,
            inputs: inputs.into_iter().flatten().collect(),
            value_output,
            value_output_key: TyWrapper::from(value_output),
            output_adapter,
            api_safety,
            field_facts,
            supported,
        });
    }

    apis.sort_by(|left, right| {
        left.target_family
            .cmp(&right.target_family)
            .then_with(|| api_descriptor_sort_rank(left).cmp(&api_descriptor_sort_rank(right)))
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.instantiated_path.cmp(&right.instantiated_path))
            .then_with(|| left.concrete_args.cmp(&right.concrete_args))
    });
    for (index, api) in apis.iter_mut().enumerate() {
        api.index = index;
    }

    apis
}

fn api_descriptor_sort_rank(api: &ApiDescriptor<'_>) -> (usize, usize, usize) {
    (
        usize::from(!api.is_mono),
        api.concrete_args
            .iter()
            .map(|arg| canonical_metadata_arg_rank(arg))
            .sum(),
        api.concrete_args.iter().map(|arg| arg.len()).sum(),
    )
}

fn canonical_metadata_arg_rank(arg: &str) -> usize {
    let trimmed = arg.trim();
    let (borrow_penalty, value) = strip_metadata_borrow_prefix(trimmed);
    borrow_penalty + canonical_metadata_value_rank(value)
}

fn strip_metadata_borrow_prefix(arg: &str) -> (usize, &str) {
    if let Some(rest) = arg.strip_prefix("&mut ") {
        return (24, rest);
    }
    if let Some(rest) = arg.strip_prefix('&') {
        return (16, rest);
    }
    (0, arg)
}

fn canonical_metadata_value_rank(value: &str) -> usize {
    if value == "std::string::String" {
        return 0;
    }
    if value == "f32" {
        return 1;
    }
    if value == "u8" || is_local_metadata_arg(value) {
        return 2;
    }
    if value == "::std::vec::Vec::<u8>" || value == "std::vec::Vec::<u8>" {
        return 3;
    }
    if value == "std::io::Cursor::<::std::vec::Vec::<u8>>"
        || value == "std::io::Cursor::<std::vec::Vec::<u8>>"
    {
        return 4;
    }
    if matches!(
        value,
        "bool"
            | "char"
            | "f64"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "isize"
            | "u16"
            | "u32"
            | "u64"
            | "usize"
    ) {
        return 5;
    }
    if value == "std::io::Empty" || value == "std::io::Sink" || value == "std::io::Repeat" {
        return 6;
    }
    8
}

fn is_local_metadata_arg(value: &str) -> bool {
    value.contains("::") && !value.starts_with("std::") && !value.starts_with("::std::")
}

fn adapted_output<'tcx>(tcx: TyCtxt<'tcx>, output: Ty<'tcx>) -> (Ty<'tcx>, OutputAdapter) {
    let TyKind::Adt(def, args) = output.kind() else {
        return (output, OutputAdapter::Plain);
    };

    if tcx.is_diagnostic_item(sym::Option, def.did())
        && let Some(inner) = args.types().next()
    {
        return (inner, OutputAdapter::Option);
    }

    if tcx.is_diagnostic_item(sym::Result, def.did())
        && let Some(ok_ty) = args.types().next()
    {
        return (ok_ty, OutputAdapter::Result);
    }

    (output, OutputAdapter::Plain)
}

pub(crate) fn render_api_path<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    def_id: DefId,
    args: ty::GenericArgsRef<'tcx>,
) -> String {
    if let Some(path) = render_trait_assoc_path(tcx, graph, def_id, args) {
        return path;
    }
    if let Some(path) = graph.public_path(def_id) {
        return render_callable_path_with_own_args(tcx, graph, def_id, args, path);
    }
    if let Some(path) = render_inherent_assoc_path(tcx, graph, def_id, args) {
        return path;
    }

    let definition_path = tcx.def_path_str(def_id);
    let rendered_definition_path =
        render_callable_path_with_own_args(tcx, graph, def_id, args, &definition_path);
    let Some(alias_path) = public_container_path(tcx, graph, def_id) else {
        return rendered_definition_path;
    };
    let Some(container_path) = container_definition_path(tcx, def_id) else {
        return rendered_definition_path;
    };
    let Some(rest) = definition_path.strip_prefix(&(container_path + "::")) else {
        return rendered_definition_path;
    };
    let alias_base = format!("{alias_path}::{rest}");
    render_callable_path_with_own_args(tcx, graph, def_id, args, &alias_base)
}

fn render_target_family_path<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    def_id: DefId,
    args: ty::GenericArgsRef<'tcx>,
) -> String {
    if let Some(path) = render_trait_assoc_family_path(tcx, graph, def_id, args) {
        return path;
    }
    render_api_family_path(tcx, graph, def_id, args)
}

fn render_api_family_path<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    def_id: DefId,
    args: ty::GenericArgsRef<'tcx>,
) -> String {
    if let Some(path) = graph.public_path(def_id) {
        return path.to_owned();
    }
    if let Some(path) = render_inherent_assoc_family_path(tcx, graph, def_id, args) {
        return path;
    }

    let definition_path = tcx.def_path_str(def_id);
    let Some(alias_path) = public_container_path(tcx, graph, def_id) else {
        return definition_path;
    };
    let Some(container_path) = container_definition_path(tcx, def_id) else {
        return definition_path;
    };
    let Some(rest) = definition_path.strip_prefix(&(container_path + "::")) else {
        return definition_path;
    };
    format!("{alias_path}::{rest}")
}

fn render_inherent_assoc_path<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    def_id: DefId,
    args: ty::GenericArgsRef<'tcx>,
) -> Option<String> {
    let assoc = tcx.opt_associated_item(def_id)?;
    if !matches!(assoc.container, AssocContainer::InherentImpl) {
        return None;
    }

    let container_id = assoc.container_id(tcx);
    let self_ty = tcx.type_of(container_id).instantiate(tcx, args);
    if !ty_is_publicly_nameable(tcx, graph, self_ty) {
        return None;
    }

    Some(format!(
        "{}::{}",
        render_public_ty_path(tcx, graph, self_ty),
        assoc.name()
    ))
    .map(|base| render_callable_path_with_own_args(tcx, graph, def_id, args, &base))
}

fn render_inherent_assoc_family_path<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    def_id: DefId,
    args: ty::GenericArgsRef<'tcx>,
) -> Option<String> {
    let assoc = tcx.opt_associated_item(def_id)?;
    if !matches!(assoc.container, AssocContainer::InherentImpl) {
        return None;
    }

    let container_id = assoc.container_id(tcx);
    let identity_args = ty::GenericArgs::identity_for_item(tcx, def_id);
    let impl_arg_count = tcx.generics_of(def_id).parent_count;
    let family_args = if impl_arg_count == 0 {
        identity_args
    } else {
        let mut family_args = identity_args.iter().collect::<Vec<_>>();
        for (index, arg) in args.iter().take(impl_arg_count).enumerate() {
            family_args[index] = arg;
        }
        tcx.mk_args(&family_args)
    };
    let self_ty = tcx.type_of(container_id).instantiate(tcx, family_args);
    if !ty_is_publicly_nameable(tcx, graph, self_ty) {
        return None;
    }

    Some(format!(
        "{}::{}",
        render_public_ty_path(tcx, graph, self_ty),
        assoc.name()
    ))
}

fn render_trait_assoc_path<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    def_id: DefId,
    args: ty::GenericArgsRef<'tcx>,
) -> Option<String> {
    let assoc = tcx.opt_associated_item(def_id)?;
    let fn_sig = crate::rapx_graph::fn_sig_with_generic_args(def_id, args, tcx);
    match assoc.container {
        AssocContainer::Trait => {
            let trait_id = assoc.container_id(tcx);
            let trait_arg_count = ty::GenericArgs::identity_for_item(tcx, trait_id).len();
            let trait_args = args.iter().take(trait_arg_count).collect::<Vec<_>>();
            let trait_args = tcx.mk_args(&trait_args);
            let self_ty = args
                .iter()
                .next()
                .and_then(|arg| arg.as_type())
                .or_else(|| fn_sig.inputs().first().map(|ty| ty.peel_refs()))?;
            Some(format!(
                "<{} as {}>::{}",
                render_public_self_ty_path(tcx, graph, self_ty),
                render_trait_path_with_args(tcx, graph, trait_id, Some(trait_args)),
                assoc.name()
            ))
        }
        AssocContainer::TraitImpl(_) => {
            let impl_id = assoc.container_id(tcx);
            let impl_arg_count = ty::GenericArgs::identity_for_item(tcx, impl_id).len();
            let impl_args = args.iter().take(impl_arg_count).collect::<Vec<_>>();
            let impl_args = tcx.mk_args(&impl_args);
            let self_ty = tcx.type_of(impl_id).instantiate(tcx, impl_args);
            let trait_ref = tcx.impl_trait_ref(impl_id).instantiate(tcx, impl_args);
            Some(format!(
                "<{} as {}>::{}",
                render_public_self_ty_path(tcx, graph, self_ty),
                render_trait_path_with_args(tcx, graph, trait_ref.def_id, Some(trait_ref.args)),
                assoc.name()
            ))
        }
        AssocContainer::InherentImpl => None,
    }
}

fn render_trait_assoc_family_path<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    def_id: DefId,
    _args: ty::GenericArgsRef<'tcx>,
) -> Option<String> {
    let assoc = tcx.opt_associated_item(def_id)?;
    let (trait_id, trait_args) = match assoc.container {
        AssocContainer::Trait => (assoc.container_id(tcx), None),
        AssocContainer::TraitImpl(_) => {
            let trait_ref = tcx.impl_trait_ref(assoc.container_id(tcx)).skip_binder();
            (trait_ref.def_id, Some(trait_ref.args))
        }
        AssocContainer::InherentImpl => return None,
    };
    let trait_path = render_trait_path_with_args(tcx, graph, trait_id, trait_args);
    Some(format!("<_ as {trait_path}>::{}", assoc.name()))
}

fn public_container_path<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    def_id: DefId,
) -> Option<String> {
    let assoc = tcx.opt_associated_item(def_id)?;
    if matches!(assoc.container, AssocContainer::Trait) {
        return None;
    }
    let container_id = assoc.container_id(tcx);
    let self_ty = tcx.type_of(container_id).instantiate_identity();
    let TyKind::Adt(def, _) = self_ty.kind() else {
        return None;
    };
    graph.public_path(def.did()).map(ToOwned::to_owned)
}

fn container_definition_path<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId) -> Option<String> {
    let assoc = tcx.opt_associated_item(def_id)?;
    if matches!(assoc.container, AssocContainer::Trait) {
        return None;
    }
    let container_id = assoc.container_id(tcx);
    let self_ty = tcx.type_of(container_id).instantiate_identity();
    let TyKind::Adt(def, _) = self_ty.kind() else {
        return None;
    };
    Some(tcx.def_path_str(def.did()))
}

fn render_public_self_ty_path<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    self_ty: Ty<'tcx>,
) -> String {
    render_public_ty_path_impl(tcx, graph, self_ty, false)
}

fn render_trait_path_with_args<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    trait_id: DefId,
    trait_args: Option<ty::GenericArgsRef<'tcx>>,
) -> String {
    let trait_path = graph
        .public_path(trait_id)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| tcx.def_path_str(trait_id));
    let Some(trait_args) = trait_args else {
        return trait_path;
    };
    let rendered_args = trait_args
        .iter()
        .skip(1)
        .map(|arg| match arg.kind() {
            ty::GenericArgKind::Type(ty) => render_public_ty_path_qualified_local(tcx, graph, ty),
            ty::GenericArgKind::Const(ct) => ct.to_string(),
            ty::GenericArgKind::Lifetime(_) => "'_".to_owned(),
        })
        .collect::<Vec<_>>();
    if rendered_args.is_empty() {
        trait_path
    } else {
        format!("{trait_path}<{}>", rendered_args.join(", "))
    }
}

fn render_public_ty_path<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    ty: Ty<'tcx>,
) -> String {
    render_public_ty_path_impl(tcx, graph, ty, false)
}

fn render_public_ty_path_qualified_local<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    ty: Ty<'tcx>,
) -> String {
    render_public_ty_path_impl(tcx, graph, ty, true)
}

fn render_public_ty_path_impl<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    ty: Ty<'tcx>,
    qualify_local: bool,
) -> String {
    match ty.kind() {
        TyKind::Adt(def, args) => {
            render_public_adt_path(tcx, graph, def.did(), args, qualify_local)
        }
        TyKind::Ref(_, inner, mutbl) => match mutbl {
            ty::Mutability::Not => {
                format!(
                    "&{}",
                    render_public_ty_path_impl(tcx, graph, *inner, qualify_local)
                )
            }
            ty::Mutability::Mut => {
                format!(
                    "&mut {}",
                    render_public_ty_path_impl(tcx, graph, *inner, qualify_local)
                )
            }
        },
        TyKind::Tuple(types) => {
            let rendered = types
                .iter()
                .map(|ty| render_public_ty_path_impl(tcx, graph, ty, qualify_local))
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

fn render_callable_path_with_own_args<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    def_id: DefId,
    args: ty::GenericArgsRef<'tcx>,
    base: &str,
) -> String {
    let generics = tcx.generics_of(def_id);
    let identity_args = ty::GenericArgs::identity_for_item(tcx, def_id);
    let rendered_args = args
        .iter()
        .zip(identity_args.iter())
        .enumerate()
        .filter(|(index, (arg, identity))| {
            *index >= generics.parent_count
                && tcx.erase_and_anonymize_regions(*arg)
                    != tcx.erase_and_anonymize_regions(*identity)
        })
        .filter_map(|(_, (arg, _))| render_generic_arg_for_path(tcx, graph, arg))
        .collect::<Vec<_>>();
    if rendered_args.is_empty() {
        base.to_owned()
    } else {
        format!("{base}::<{}>", rendered_args.join(", "))
    }
}

fn render_generic_arg_for_path<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    arg: ty::GenericArg<'tcx>,
) -> Option<String> {
    match arg.kind() {
        ty::GenericArgKind::Type(ty) => Some(render_public_ty_path_qualified_local(tcx, graph, ty)),
        ty::GenericArgKind::Const(ct) => Some(ct.to_string()),
        ty::GenericArgKind::Lifetime(_) => None,
    }
}

pub(crate) fn render_concrete_args<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    args: ty::GenericArgsRef<'tcx>,
    identity_args: ty::GenericArgsRef<'tcx>,
) -> Vec<String> {
    args.iter()
        .zip(identity_args.iter())
        .filter_map(
            |(arg, identity)| match (arg.as_type(), identity.as_type()) {
                (Some(arg_ty), Some(identity_ty))
                    if tcx.erase_and_anonymize_regions(arg_ty)
                        != tcx.erase_and_anonymize_regions(identity_ty) =>
                {
                    Some(render_public_ty_path_qualified_local(
                        tcx,
                        graph,
                        tcx.erase_and_anonymize_regions(arg_ty),
                    ))
                }
                _ => None,
            },
        )
        .collect()
}

fn render_public_adt_path<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    def_id: DefId,
    args: ty::GenericArgsRef<'tcx>,
    qualify_local: bool,
) -> String {
    if tcx.is_diagnostic_item(sym::Vec, def_id)
        && let Some(element_ty) = args.types().next()
    {
        return format!(
            "::std::vec::Vec::<{}>",
            render_public_ty_path_impl(tcx, graph, element_ty, qualify_local)
        );
    }

    let mut base = graph
        .public_path(def_id)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| tcx.def_path_str(def_id));
    if qualify_local && def_id.is_local() {
        let crate_name = tcx.crate_name(LOCAL_CRATE).to_string();
        if !base.starts_with(&crate_name) && !base.starts_with("::") {
            base = format!("{crate_name}::{base}");
        }
    }
    let rendered_args = rendered_adt_args(tcx, def_id, args)
        .iter()
        .map(|arg| match arg.kind() {
            ty::GenericArgKind::Type(ty) => {
                render_public_ty_path_impl(tcx, graph, ty, qualify_local)
            }
            ty::GenericArgKind::Const(ct) => ct.to_string(),
            ty::GenericArgKind::Lifetime(_) => "'_".to_owned(),
        })
        .collect::<Vec<_>>();
    if rendered_args.is_empty() {
        base
    } else {
        format!("{base}::<{}>", rendered_args.join(", "))
    }
}

fn rendered_adt_args<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: DefId,
    args: ty::GenericArgsRef<'tcx>,
) -> Vec<ty::GenericArg<'tcx>> {
    let mut rendered_args = args.iter().collect::<Vec<_>>();
    if should_omit_default_global_allocator(tcx, def_id, args) {
        rendered_args.truncate(1);
    }
    rendered_args
}

fn should_omit_default_global_allocator<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: DefId,
    args: ty::GenericArgsRef<'tcx>,
) -> bool {
    let path = tcx.def_path_str(def_id);
    if !matches!(
        path.as_str(),
        "alloc::boxed::Box"
            | "std::boxed::Box"
            | "alloc::rc::Rc"
            | "std::rc::Rc"
            | "alloc::sync::Arc"
            | "std::sync::Arc"
    ) {
        return false;
    }
    args.types()
        .nth(1)
        .is_some_and(|ty| type_is_global_allocator(tcx, ty))
}

fn type_is_global_allocator(tcx: TyCtxt<'_>, ty: Ty<'_>) -> bool {
    let TyKind::Adt(def, _) = ty.kind() else {
        return false;
    };
    matches!(
        tcx.def_path_str(def.did()).as_str(),
        "alloc::alloc::Global" | "std::alloc::Global"
    )
}

pub(crate) fn is_supported_callable<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    def_id: DefId,
    args: ty::GenericArgsRef<'tcx>,
    path: &str,
    api_safety: ApiSafety,
) -> bool {
    if path.contains("{impl#") || path.contains("::<impl ") || path.contains("{{closure}}") {
        return false;
    }
    if tcx.generics_of(def_id).requires_monomorphization(tcx)
        && !generic_args_are_fully_resolved(args)
    {
        return false;
    }
    if !callable_inputs_are_publicly_nameable(tcx, graph, def_id, args) {
        return false;
    }
    if !callable_self_ty_is_publicly_nameable(tcx, graph, def_id, args) {
        return false;
    }
    if is_explicit_drop_callable(tcx, def_id) {
        return false;
    }
    if let Some(assoc) = tcx.opt_associated_item(def_id)
        && matches!(assoc.container, AssocContainer::TraitImpl(_))
    {
        let trait_def_id = tcx
            .impl_trait_ref(assoc.container_id(tcx))
            .skip_binder()
            .def_id;
        if trait_def_id.krate == LOCAL_CRATE {
            if !tcx
                .effective_visibilities(())
                .is_exported(trait_def_id.expect_local())
            {
                return false;
            }
        } else {
            let trait_crate = tcx.crate_name(trait_def_id.krate);
            if !matches!(trait_crate.as_str(), "core" | "std" | "alloc") {
                return false;
            }
        }
    }
    !api_safety.is_unsafe_fn()
}

fn is_explicit_drop_callable(tcx: TyCtxt<'_>, def_id: DefId) -> bool {
    let Some(assoc) = tcx.opt_associated_item(def_id) else {
        return false;
    };
    if assoc.name() != sym::drop {
        return false;
    }

    match assoc.container {
        AssocContainer::Trait => tcx.is_lang_item(assoc.container_id(tcx), LangItem::Drop),
        AssocContainer::TraitImpl(_) => {
            let trait_ref = tcx.impl_trait_ref(assoc.container_id(tcx)).skip_binder();
            tcx.is_lang_item(trait_ref.def_id, LangItem::Drop)
        }
        AssocContainer::InherentImpl => false,
    }
}

fn generic_args_are_fully_resolved(args: ty::GenericArgsRef<'_>) -> bool {
    !args.iter().any(|arg| match arg.kind() {
        ty::GenericArgKind::Lifetime(_) => false,
        ty::GenericArgKind::Type(ty) => {
            ty.has_param()
                || ty.has_infer_types()
                || ty.has_escaping_bound_vars()
                || ty.has_placeholders()
        }
        ty::GenericArgKind::Const(ct) => ct.has_param() || ct.has_infer(),
    })
}

fn callable_inputs_are_publicly_nameable<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    def_id: DefId,
    args: ty::GenericArgsRef<'tcx>,
) -> bool {
    let fn_sig = crate::rapx_graph::fn_sig_with_generic_args(def_id, args, tcx);
    fn_sig
        .inputs()
        .iter()
        .copied()
        .all(|ty| ty_is_publicly_nameable(tcx, graph, ty))
}

fn callable_self_ty_is_publicly_nameable<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    def_id: DefId,
    args: ty::GenericArgsRef<'tcx>,
) -> bool {
    let Some(assoc) = tcx.opt_associated_item(def_id) else {
        return true;
    };
    if !matches!(
        assoc.container,
        AssocContainer::InherentImpl | AssocContainer::TraitImpl(_)
    ) {
        return true;
    }

    let container_id = assoc.container_id(tcx);
    let self_ty = tcx.type_of(container_id).instantiate(tcx, args);
    ty_is_publicly_nameable(tcx, graph, self_ty)
}

fn ty_is_publicly_nameable<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &ApiDependencyGraph<'tcx>,
    ty: Ty<'tcx>,
) -> bool {
    match ty.kind() {
        TyKind::Bool
        | TyKind::Char
        | TyKind::Int(_)
        | TyKind::Uint(_)
        | TyKind::Float(_)
        | TyKind::Str
        | TyKind::Never => true,
        TyKind::Ref(_, inner, _) | TyKind::RawPtr(inner, _) => {
            ty_is_publicly_nameable(tcx, graph, *inner)
        }
        TyKind::Slice(inner) | TyKind::Array(inner, _) => {
            ty_is_publicly_nameable(tcx, graph, *inner)
        }
        TyKind::Dynamic(predicates, ..) => {
            predicates
                .iter()
                .all(|predicate| match predicate.skip_binder() {
                    ty::ExistentialPredicate::Trait(trait_ref) => {
                        def_is_publicly_nameable(tcx, graph, trait_ref.def_id)
                    }
                    ty::ExistentialPredicate::AutoTrait(def_id) => {
                        def_is_publicly_nameable(tcx, graph, def_id)
                    }
                    ty::ExistentialPredicate::Projection(_) => true,
                })
        }
        TyKind::Tuple(types) => types
            .iter()
            .all(|inner| ty_is_publicly_nameable(tcx, graph, inner)),
        TyKind::Adt(def, args) => {
            (crate::std_adapters::is_public_std_adapter_value_ty(tcx, ty)
                || adt_is_publicly_nameable(tcx, graph, def.did()))
                && args.iter().all(|arg| match arg.kind() {
                    ty::GenericArgKind::Type(inner) => ty_is_publicly_nameable(tcx, graph, inner),
                    ty::GenericArgKind::Const(_) | ty::GenericArgKind::Lifetime(_) => true,
                })
        }
        TyKind::Param(_) => false,
        _ => false,
    }
}

fn adt_is_publicly_nameable(
    tcx: TyCtxt<'_>,
    graph: &ApiDependencyGraph<'_>,
    def_id: DefId,
) -> bool {
    def_is_publicly_nameable(tcx, graph, def_id)
}

fn def_is_publicly_nameable(
    tcx: TyCtxt<'_>,
    graph: &ApiDependencyGraph<'_>,
    def_id: DefId,
) -> bool {
    if def_id.is_local() {
        return graph.public_path(def_id).is_some()
            || def_id.as_local().is_some_and(|local_def_id| {
                tcx.effective_visibilities(()).is_exported(local_def_id)
            });
    }

    def_path_is_publicly_nameable(tcx, def_id)
}

fn def_path_is_publicly_nameable(tcx: TyCtxt<'_>, mut def_id: DefId) -> bool {
    let crate_name = tcx.crate_name(def_id.krate);
    if matches!(crate_name.as_str(), "std" | "core" | "alloc") {
        let path = tcx.def_path_str(def_id);
        if path.contains("::sys::") || path.contains("::io::stdio::") {
            return false;
        }
    }

    loop {
        if !tcx.visibility(def_id).is_public() {
            return false;
        }
        let Some(parent) = tcx.opt_parent(def_id) else {
            return true;
        };
        def_id = parent;
    }
}

pub(crate) fn classify_api_safety<'tcx>(
    def_id: DefId,
    tcx: TyCtxt<'tcx>,
    field_facts_index: &ApiFieldFactsIndex,
) -> ApiSafety {
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
    let field_facts = field_facts_index.field_facts_for(def_id);
    if field_facts.is_unsafe_related()
        && !field_facts.direct_unsafe
        && has_local_unsafe_wrapper(tcx, &field_facts.unsafe_functions)
    {
        return ApiSafety::IndirectUnsafe;
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

fn has_local_unsafe_wrapper(tcx: TyCtxt<'_>, unsafe_functions: &[String]) -> bool {
    let crate_name = tcx.crate_name(LOCAL_CRATE).to_string();
    let crate_prefix = format!("{crate_name}::");
    unsafe_functions.iter().any(|function| {
        function == &crate_name
            || function.starts_with(&crate_prefix)
            || !is_std_internal_unsafe_function(function)
    })
}

fn is_std_internal_unsafe_function(function: &str) -> bool {
    matches!(function.split("::").next(), Some("std" | "core" | "alloc"))
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
        intravisit::walk_block(self, block);
    }
}
