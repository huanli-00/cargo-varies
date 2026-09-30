// SPDX-License-Identifier: MPL-2.0
// Derived from RAPx; see NOTICE.
use crate::rapx_graph::{TyWrapper, fn_sig_with_generic_args};
use crate::std_adapters::common_symbolic_seed_value_tys;
use log::debug;
use rustc_hir::LangItem;
use rustc_hir::def_id::DefId;
use rustc_infer::infer::DefineOpaqueTypes;
use rustc_infer::infer::{InferCtxt, TyCtxtInferExt};
use rustc_infer::traits::ObligationCause;
use rustc_middle::ty::{self, Ty, TyCtxt, TypeVisitableExt, Upcast};
use rustc_span::sym;
use rustc_trait_selection::traits::Obligation;
use rustc_trait_selection::traits::query::evaluate_obligation::InferCtxtExt as _;
use std::collections::HashSet;

const MAX_MONOS_PER_FN: usize = 32;
const MAX_OUTPUT_ONLY_MONOS_PER_FN: usize = 12;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Mono<'tcx> {
    pub(crate) value: Vec<ty::GenericArg<'tcx>>,
}

impl<'tcx> Mono<'tcx> {
    fn new(identity: &[ty::GenericArg<'tcx>]) -> Self {
        Self {
            value: identity.to_vec(),
        }
    }

    fn has_infer_types(&self) -> bool {
        self.value.iter().any(|arg| match arg.kind() {
            ty::GenericArgKind::Type(ty) => ty.has_infer_types(),
            _ => false,
        })
    }

    fn merge(&self, other: &Mono<'tcx>, tcx: TyCtxt<'tcx>) -> Option<Self> {
        assert_eq!(self.value.len(), other.value.len());
        let mut value = Vec::with_capacity(self.value.len());
        for (lhs, rhs) in self.value.iter().zip(other.value.iter()) {
            let next = if let Some(lhs_ty) = lhs.as_type() {
                let rhs_ty = rhs.expect_ty();
                if lhs_ty.is_ty_var() && !rhs_ty.is_ty_var() {
                    *rhs
                } else if lhs_ty.is_ty_var()
                    || rhs_ty.is_ty_var()
                    || tcx.erase_and_anonymize_regions(lhs_ty)
                        == tcx.erase_and_anonymize_regions(rhs_ty)
                {
                    *lhs
                } else {
                    return None;
                }
            } else {
                *lhs
            };
            value.push(next);
        }
        Some(Self { value })
    }

    fn fill_unbound_var(&self, tcx: TyCtxt<'tcx>) -> Vec<Self> {
        let mut monos = vec![self.clone()];
        for index in 0..self.value.len() {
            let Some(ty) = self.value[index].as_type() else {
                continue;
            };
            if !ty.has_infer_types() {
                continue;
            }

            let mut expanded = Vec::new();
            for mono in monos {
                for candidate in expand_infer_types(ty, tcx) {
                    let mut next = mono.clone();
                    next.value[index] = candidate.into();
                    expanded.push(next);
                    if expanded.len() >= MAX_MONOS_PER_FN {
                        break;
                    }
                }
                if expanded.len() >= MAX_MONOS_PER_FN {
                    break;
                }
            }
            monos = expanded;
        }
        monos
    }

    fn expand_shared_ref_variants(&self, tcx: TyCtxt<'tcx>) -> Vec<Self> {
        let mut variants = Vec::new();
        for (index, arg) in self.value.iter().enumerate() {
            let Some(ty) = arg.as_type() else {
                continue;
            };
            let erased = tcx.erase_and_anonymize_regions(ty);
            if !can_expand_shared_ref_candidate(erased) {
                continue;
            }

            let mut next = self.clone();
            next.value[index] = Ty::new_imm_ref(tcx, tcx.lifetimes.re_erased, erased).into();
            variants.push(next);
        }
        variants
    }
}

#[derive(Clone, Debug, Default)]
struct MonoSet<'tcx> {
    monos: Vec<Mono<'tcx>>,
    seen: HashSet<Mono<'tcx>>,
}

impl<'tcx> MonoSet<'tcx> {
    fn all(identity: &[ty::GenericArg<'tcx>]) -> Self {
        let mut set = Self::default();
        set.insert(Mono::new(identity));
        set
    }

    fn insert(&mut self, mono: Mono<'tcx>) {
        if self.seen.insert(mono.clone()) {
            self.monos.push(mono);
        }
    }

    fn rebuild_seen(&mut self) {
        self.seen = self.monos.iter().cloned().collect();
    }

    fn merge(&self, other: &Self, tcx: TyCtxt<'tcx>) -> Self {
        let mut merged = Self::default();
        for lhs in &self.monos {
            for rhs in &other.monos {
                if let Some(mono) = lhs.merge(rhs, tcx) {
                    merged.insert(mono);
                }
            }
        }
        merged
    }

    fn instantiate_unbound(&self, tcx: TyCtxt<'tcx>) -> Self {
        let mut instantiated = Self::default();
        for mono in &self.monos {
            if mono.has_infer_types() {
                for next in mono.fill_unbound_var(tcx) {
                    instantiated.insert(next);
                }
            } else {
                instantiated.insert(mono.clone());
            }
        }
        instantiated
    }

    fn erase_region_var(&mut self, tcx: TyCtxt<'tcx>) {
        for mono in &mut self.monos {
            mono.value
                .iter_mut()
                .for_each(|arg| *arg = tcx.erase_and_anonymize_regions(*arg));
        }
        self.rebuild_seen();
    }

    fn filter_by_trait_bound(mut self, fn_did: DefId, tcx: TyCtxt<'tcx>) -> Self {
        self.monos
            .retain(|mono| args_fit_trait_bounds(fn_did, &mono.value, tcx));
        self.rebuild_seen();
        self
    }

    fn filter_by_partial_trait_bound(mut self, fn_did: DefId, tcx: TyCtxt<'tcx>) -> Self {
        self.monos
            .retain(|mono| mono_may_fit_trait_bounds(fn_did, &mono.value, tcx));
        self.rebuild_seen();
        self
    }

    fn expand_shared_ref_variants(&self, tcx: TyCtxt<'tcx>) -> Self {
        let mut expanded = Self::default();
        for mono in &self.monos {
            expanded.insert(mono.clone());
            for next in mono.expand_shared_ref_variants(tcx) {
                expanded.insert(next);
            }
        }
        expanded
    }

    fn sort_by_preference(&mut self, tcx: TyCtxt<'tcx>) {
        self.monos.sort_by_key(|mono| mono_sort_key(mono, tcx));
    }

    fn truncate_preserving_common_seeds(&mut self, limit: usize, tcx: TyCtxt<'tcx>) {
        if self.monos.len() <= limit {
            return;
        }

        self.sort_by_preference(tcx);
        let mut common = self
            .monos
            .iter()
            .filter(|mono| mono_contains_common_symbolic_seed(mono, tcx))
            .cloned()
            .collect::<Vec<_>>();
        let mut others = self
            .monos
            .iter()
            .filter(|mono| !mono_contains_common_symbolic_seed(mono, tcx))
            .cloned()
            .collect::<Vec<_>>();

        if common.len() > limit {
            common.truncate(limit);
            self.monos = common;
            self.rebuild_seen();
            return;
        }

        others.truncate(limit - common.len());
        common.extend(others);
        self.monos = common;
        self.rebuild_seen();
    }
}

fn mono_sort_key<'tcx>(mono: &Mono<'tcx>, tcx: TyCtxt<'tcx>) -> (usize, usize, String) {
    let mut priority = 0;
    let mut complexity = 0;
    let mut parts = Vec::new();
    for arg in &mono.value {
        let Some(ty) = arg.as_type() else {
            continue;
        };
        let erased = tcx.erase_and_anonymize_regions(ty);
        priority += seed_candidate_priority(erased, tcx);
        complexity += ty_complexity(erased);
        parts.push(erased.to_string());
    }
    (priority, complexity, parts.join(" | "))
}

fn mono_seed_specificity<'tcx>(mono: &Mono<'tcx>) -> usize {
    mono.value
        .iter()
        .filter(|arg| {
            arg.as_type()
                .is_some_and(|ty| !matches!(ty.kind(), ty::TyKind::Param(_)))
        })
        .count()
}

fn seed_candidate_priority<'tcx>(ty: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> usize {
    if is_common_symbolic_seed_candidate(ty, tcx) {
        return 0;
    }
    if is_local_public_seed_candidate(ty) {
        return 1;
    }
    if is_preferred_seed_candidate(ty, tcx) {
        return 2;
    }
    3
}

fn is_local_public_seed_candidate(ty: Ty<'_>) -> bool {
    match ty.kind() {
        ty::TyKind::Ref(_, inner, _) => is_local_public_seed_candidate(*inner),
        ty::TyKind::Adt(def, _) => def.did().is_local(),
        _ => false,
    }
}

fn is_common_symbolic_seed_candidate<'tcx>(ty: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    let ty = tcx.erase_and_anonymize_regions(ty);
    let value_ty = ty.peel_refs();
    if let ty::TyKind::Adt(def, _) = value_ty.kind() {
        let path = tcx.def_path_str(def.did());
        if path.ends_with("::io::Cursor") || path.ends_with("::io::cursor::Cursor") {
            return true;
        }
    }
    common_symbolic_seed_value_tys(tcx)
        .into_iter()
        .map(|candidate| tcx.erase_and_anonymize_regions(candidate))
        .any(|candidate| candidate == value_ty)
}

fn mono_contains_common_symbolic_seed<'tcx>(mono: &Mono<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    mono.value.iter().any(|arg| {
        arg.as_type()
            .is_some_and(|ty| is_common_symbolic_seed_candidate(ty, tcx))
    })
}

fn is_preferred_seed_candidate<'tcx>(ty: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    match ty.kind() {
        ty::TyKind::Bool
        | ty::TyKind::Char
        | ty::TyKind::Int(_)
        | ty::TyKind::Uint(_)
        | ty::TyKind::Float(_)
        | ty::TyKind::Str => true,
        ty::TyKind::Ref(_, inner, _) => is_preferred_seed_candidate(*inner, tcx),
        ty::TyKind::Slice(inner) | ty::TyKind::Array(inner, _) => {
            is_preferred_seed_candidate(*inner, tcx)
        }
        ty::TyKind::Tuple(types) => types.iter().all(|ty| is_preferred_seed_candidate(ty, tcx)),
        ty::TyKind::Adt(def, args) => {
            if tcx.is_lang_item(def.did(), LangItem::String)
                || tcx.is_diagnostic_item(sym::Vec, def.did())
            {
                return args
                    .types()
                    .next()
                    .map(|inner| is_preferred_seed_candidate(inner, tcx))
                    .unwrap_or(true);
            }
            if tcx.def_path_str(def.did()).ends_with("::io::Cursor") {
                return args
                    .types()
                    .next()
                    .map(|inner| is_preferred_seed_candidate(inner.peel_refs(), tcx))
                    .unwrap_or(false);
            }
            false
        }
        _ => false,
    }
}

fn has_mutable_indirection(ty: Ty<'_>) -> bool {
    match ty.kind() {
        ty::TyKind::Ref(_, _, ty::Mutability::Mut) | ty::TyKind::RawPtr(_, ty::Mutability::Mut) => {
            true
        }
        ty::TyKind::Ref(_, inner, _) | ty::TyKind::RawPtr(inner, _) | ty::TyKind::Slice(inner) => {
            has_mutable_indirection(*inner)
        }
        ty::TyKind::Array(inner, _) => has_mutable_indirection(*inner),
        ty::TyKind::Tuple(types) => types.iter().any(has_mutable_indirection),
        ty::TyKind::Adt(_, args) => args
            .iter()
            .filter_map(|arg| arg.as_type())
            .any(has_mutable_indirection),
        _ => false,
    }
}

fn ty_ref_depth(ty: Ty<'_>) -> usize {
    match ty.kind() {
        ty::TyKind::Ref(_, inner, _) | ty::TyKind::RawPtr(inner, _) => 1 + ty_ref_depth(*inner),
        ty::TyKind::Slice(inner) | ty::TyKind::Array(inner, _) => ty_ref_depth(*inner),
        ty::TyKind::Tuple(types) => types.iter().map(ty_ref_depth).sum(),
        ty::TyKind::Adt(_, args) => args
            .iter()
            .filter_map(|arg| arg.as_type())
            .map(ty_ref_depth)
            .sum(),
        _ => 0,
    }
}

fn ty_complexity(ty: Ty<'_>) -> usize {
    match ty.kind() {
        ty::TyKind::Ref(_, inner, _) | ty::TyKind::RawPtr(inner, _) | ty::TyKind::Slice(inner) => {
            1 + ty_complexity(*inner)
        }
        ty::TyKind::Array(inner, _) => 1 + ty_complexity(*inner),
        ty::TyKind::Tuple(types) => 1 + types.iter().map(ty_complexity).sum::<usize>(),
        ty::TyKind::Adt(_, args) => {
            1 + args
                .iter()
                .filter_map(|arg| arg.as_type())
                .map(ty_complexity)
                .sum::<usize>()
        }
        _ => 1,
    }
}

fn can_expand_shared_ref_candidate(ty: Ty<'_>) -> bool {
    if has_mutable_indirection(ty) || ty_ref_depth(ty) != 0 {
        return false;
    }
    matches!(
        ty.kind(),
        ty::TyKind::Adt(..) | ty::TyKind::Slice(..) | ty::TyKind::Str | ty::TyKind::Array(..)
    )
}

fn expand_infer_types<'tcx>(ty: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> Vec<Ty<'tcx>> {
    if !ty.has_infer_types() {
        return vec![ty];
    }

    match ty.kind() {
        ty::TyKind::Infer(_) => unbound_generic_candidates(tcx),
        ty::TyKind::Ref(region, inner, mutability) => expand_infer_types(*inner, tcx)
            .into_iter()
            .map(|inner| Ty::new_ref(tcx, *region, inner, *mutability))
            .collect(),
        ty::TyKind::Slice(inner) => expand_infer_types(*inner, tcx)
            .into_iter()
            .map(|inner| Ty::new_slice(tcx, inner))
            .collect(),
        ty::TyKind::Array(_, _) => vec![ty],
        ty::TyKind::Tuple(types) => expand_tuple_infer_types(types.iter().collect(), tcx)
            .into_iter()
            .map(|types| Ty::new_tup(tcx, &types))
            .collect(),
        ty::TyKind::Adt(def, args) => expand_adt_infer_types(*def, args, tcx),
        _ => vec![ty],
    }
}

fn expand_tuple_infer_types<'tcx>(types: Vec<Ty<'tcx>>, tcx: TyCtxt<'tcx>) -> Vec<Vec<Ty<'tcx>>> {
    let mut tuples = vec![Vec::new()];
    for ty in types {
        let expanded = expand_infer_types(ty, tcx);
        let mut next = Vec::new();
        for prefix in tuples {
            for candidate in &expanded {
                let mut combined = prefix.clone();
                combined.push(*candidate);
                next.push(combined);
                if next.len() >= MAX_MONOS_PER_FN {
                    break;
                }
            }
            if next.len() >= MAX_MONOS_PER_FN {
                break;
            }
        }
        tuples = next;
    }
    tuples
}

fn expand_adt_infer_types<'tcx>(
    def: ty::AdtDef<'tcx>,
    args: ty::GenericArgsRef<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> Vec<Ty<'tcx>> {
    let mut arg_sets = vec![Vec::new()];
    for arg in args.iter() {
        let candidates = match arg.as_type() {
            Some(ty) => expand_infer_types(ty, tcx)
                .into_iter()
                .map(ty::GenericArg::from)
                .collect::<Vec<_>>(),
            None => vec![arg],
        };
        let mut next_sets = Vec::new();
        for prefix in arg_sets {
            for candidate in &candidates {
                let mut combined = prefix.clone();
                combined.push(*candidate);
                next_sets.push(combined);
                if next_sets.len() >= MAX_MONOS_PER_FN {
                    break;
                }
            }
            if next_sets.len() >= MAX_MONOS_PER_FN {
                break;
            }
        }
        arg_sets = next_sets;
    }

    arg_sets
        .into_iter()
        .map(|args| Ty::new_adt(tcx, def, tcx.mk_args(&args)))
        .collect()
}

fn args_fit_trait_bounds<'tcx>(
    fn_did: DefId,
    args: &[ty::GenericArg<'tcx>],
    tcx: TyCtxt<'tcx>,
) -> bool {
    trait_bounds_hold(fn_did, args, tcx, true)
}

fn mono_may_fit_trait_bounds<'tcx>(
    fn_did: DefId,
    args: &[ty::GenericArg<'tcx>],
    tcx: TyCtxt<'tcx>,
) -> bool {
    trait_bounds_hold(fn_did, args, tcx, false)
}

fn trait_bounds_hold<'tcx>(
    fn_did: DefId,
    args: &[ty::GenericArg<'tcx>],
    tcx: TyCtxt<'tcx>,
    require_concrete: bool,
) -> bool {
    let param_env = tcx.param_env(fn_did);
    tcx.predicates_of(fn_did)
        .instantiate(tcx, tcx.mk_args(args))
        .predicates
        .into_iter()
        .all(|predicate| {
            let Some(bound_trait_predicate) = predicate.as_trait_clause() else {
                return clause_may_hold(predicate, param_env, tcx, require_concrete);
            };
            let Some(trait_predicate) = bound_trait_predicate.no_bound_vars() else {
                return false;
            };
            let trait_ref = trait_predicate.trait_ref;
            let trait_def_id = trait_ref.def_id;

            if tcx.is_lang_item(trait_def_id, LangItem::Sized)
                || tcx.def_path_str(trait_def_id).ends_with("::MetaSized")
            {
                return true;
            }

            if tcx.is_lang_item(trait_def_id, LangItem::Fn)
                || tcx.is_lang_item(trait_def_id, LangItem::FnMut)
                || tcx.is_lang_item(trait_def_id, LangItem::FnOnce)
            {
                return false;
            }

            if trait_ref_requires_static_borrow_owned_by_candidate(trait_ref, tcx) {
                return false;
            }

            if trait_ref_is_concrete(trait_ref, tcx) {
                return concrete_trait_ref_implements_trait(trait_ref, param_env, tcx);
            }

            if require_concrete {
                return false;
            }

            let self_ty = tcx.erase_and_anonymize_regions(trait_ref.self_ty());
            if self_ty.has_infer_types() || self_ty.has_param() {
                return true;
            }

            let Some(target_ty) = trait_ref.args.types().nth(1) else {
                return true;
            };
            let target_ty = tcx.erase_and_anonymize_regions(target_ty);
            if target_ty.has_infer_types() || target_ty.has_param() {
                return true;
            }

            match tcx.item_name(trait_def_id).as_str() {
                "AsRef" | "Borrow" => can_satisfy_as_ref_like(self_ty, target_ty, tcx),
                "Into" | "From" => same_owned_like_ty(self_ty, target_ty, tcx),
                _ => true,
            }
        })
}

fn clause_may_hold<'tcx>(
    clause: ty::Clause<'tcx>,
    param_env: ty::ParamEnv<'tcx>,
    tcx: TyCtxt<'tcx>,
    require_concrete: bool,
) -> bool {
    let predicate: ty::Predicate<'tcx> = tcx.erase_and_anonymize_regions(clause.upcast(tcx));
    if predicate.has_param()
        || predicate.has_infer()
        || predicate.has_escaping_bound_vars()
        || predicate.has_placeholders()
    {
        return !require_concrete;
    }

    predicate_must_hold(predicate, param_env, tcx)
}

fn trait_ref_requires_static_borrow_owned_by_candidate<'tcx>(
    trait_ref: ty::TraitRef<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    let trait_name = tcx.item_name(trait_ref.def_id);
    let trait_name = trait_name.as_str();
    if !matches!(trait_name, "Into" | "From") {
        return false;
    }

    let Some(target_ty) = trait_ref.args.types().nth(1) else {
        return false;
    };

    if !type_contains_static_region(target_ty) {
        return false;
    }

    let candidate_ty = match trait_name {
        "Into" => trait_ref.self_ty(),
        "From" => target_ty,
        _ => return false,
    };

    type_contains_reference(candidate_ty)
}

fn type_contains_static_region(ty: Ty<'_>) -> bool {
    match ty.kind() {
        ty::TyKind::Ref(region, inner, _) => {
            region.is_static() || type_contains_static_region(*inner)
        }
        ty::TyKind::RawPtr(inner, _) | ty::TyKind::Slice(inner) | ty::TyKind::Array(inner, _) => {
            type_contains_static_region(*inner)
        }
        ty::TyKind::Tuple(types) => types.iter().any(type_contains_static_region),
        ty::TyKind::Adt(_, args) => args.iter().any(generic_arg_contains_static_region),
        _ => false,
    }
}

fn generic_arg_contains_static_region(arg: ty::GenericArg<'_>) -> bool {
    match arg.kind() {
        ty::GenericArgKind::Lifetime(region) => region.is_static(),
        ty::GenericArgKind::Type(ty) => type_contains_static_region(ty),
        ty::GenericArgKind::Const(_) => false,
    }
}

fn type_contains_reference(ty: Ty<'_>) -> bool {
    match ty.kind() {
        ty::TyKind::Ref(..) => true,
        ty::TyKind::RawPtr(inner, _) | ty::TyKind::Slice(inner) | ty::TyKind::Array(inner, _) => {
            type_contains_reference(*inner)
        }
        ty::TyKind::Tuple(types) => types.iter().any(type_contains_reference),
        ty::TyKind::Adt(_, args) => args
            .iter()
            .filter_map(|arg| arg.as_type())
            .any(type_contains_reference),
        _ => false,
    }
}

fn is_low_cost_checked_common_trait(trait_def_id: DefId, tcx: TyCtxt<'_>) -> bool {
    tcx.is_lang_item(trait_def_id, LangItem::Copy)
        || tcx.is_lang_item(trait_def_id, LangItem::Clone)
        || tcx.is_lang_item(trait_def_id, LangItem::PartialEq)
        || tcx.is_lang_item(trait_def_id, LangItem::PartialOrd)
        || (!trait_def_id.is_local()
            && matches!(
                tcx.def_path_str(trait_def_id).as_str(),
                path if path.ends_with("::cmp::Eq")
                    || path.ends_with("::cmp::Ord")
                    || path.ends_with("::fmt::Debug")
                    || path.ends_with("::default::Default")
                    || path.ends_with("::hash::Hash")
            ))
}

fn is_supported_external_trait_bound(trait_def_id: DefId, tcx: TyCtxt<'_>) -> bool {
    let trait_name = tcx.item_name(trait_def_id);
    let trait_name = trait_name.as_str();
    let trait_path = tcx.def_path_str(trait_def_id);
    matches!(
        trait_name,
        "AsRef" | "Borrow" | "Into" | "From" | "Read" | "Write" | "Seek" | "BufRead" | "ReadSeek"
    ) || trait_path.ends_with("::io::Read")
        || trait_path.ends_with("::io::Write")
        || trait_path.ends_with("::io::Seek")
        || trait_path.ends_with("::io::BufRead")
        || trait_path.ends_with("::ReadSeek")
}

fn trait_ref_is_concrete<'tcx>(trait_ref: ty::TraitRef<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    let erased = tcx.erase_and_anonymize_regions(trait_ref);
    if erased.has_escaping_bound_vars() || erased.has_placeholders() || erased.has_aliases() {
        return false;
    }

    !erased.args.iter().any(|arg| match arg.kind() {
        ty::GenericArgKind::Lifetime(_) => false,
        ty::GenericArgKind::Type(ty) => ty.has_infer_types() || ty.has_param(),
        ty::GenericArgKind::Const(ct) => ct.has_infer() || ct.has_param(),
    })
}

fn concrete_trait_ref_implements_trait<'tcx>(
    trait_ref: ty::TraitRef<'tcx>,
    param_env: ty::ParamEnv<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    if !trait_ref_is_concrete(trait_ref, tcx) {
        return false;
    }

    let predicate: ty::Predicate<'tcx> = tcx.erase_and_anonymize_regions(trait_ref.upcast(tcx));
    predicate_must_hold(predicate, param_env, tcx)
}

fn predicate_must_hold<'tcx>(
    predicate: ty::Predicate<'tcx>,
    param_env: ty::ParamEnv<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    let obligation = Obligation::new(tcx, ObligationCause::dummy(), param_env, predicate);

    tcx.infer_ctxt()
        .ignoring_regions()
        .build(ty::TypingMode::PostAnalysis)
        .predicate_must_hold_modulo_regions(&obligation)
}

fn same_erased_ty<'tcx>(lhs: Ty<'tcx>, rhs: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    tcx.erase_and_anonymize_regions(lhs) == tcx.erase_and_anonymize_regions(rhs)
}

fn same_owned_like_ty<'tcx>(lhs: Ty<'tcx>, rhs: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    if same_erased_ty(lhs, rhs, tcx) {
        return true;
    }

    match (lhs.kind(), rhs.kind()) {
        (ty::TyKind::Adt(lhs_def, lhs_args), ty::TyKind::Adt(rhs_def, rhs_args))
            if tcx.is_lang_item(lhs_def.did(), LangItem::String)
                && tcx.is_lang_item(rhs_def.did(), LangItem::String) =>
        {
            true
        }
        (ty::TyKind::Adt(lhs_def, lhs_args), ty::TyKind::Adt(rhs_def, rhs_args))
            if tcx.is_diagnostic_item(sym::Vec, lhs_def.did())
                && tcx.is_diagnostic_item(sym::Vec, rhs_def.did()) =>
        {
            matches!(
                (lhs_args.types().next(), rhs_args.types().next()),
                (Some(lhs_inner), Some(rhs_inner)) if same_erased_ty(lhs_inner, rhs_inner, tcx)
            )
        }
        _ => false,
    }
}

fn can_satisfy_as_ref_like<'tcx>(
    self_ty: Ty<'tcx>,
    target_ty: Ty<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    if same_erased_ty(self_ty, target_ty, tcx) {
        return true;
    }

    match self_ty.kind() {
        ty::TyKind::Ref(_, inner, _) => can_satisfy_as_ref_like(*inner, target_ty, tcx),
        ty::TyKind::Array(inner, _) => match target_ty.kind() {
            ty::TyKind::Slice(target_inner) => same_erased_ty(*inner, *target_inner, tcx),
            _ => false,
        },
        ty::TyKind::Slice(inner) => match target_ty.kind() {
            ty::TyKind::Slice(target_inner) => same_erased_ty(*inner, *target_inner, tcx),
            _ => false,
        },
        ty::TyKind::Str => matches!(target_ty.kind(), ty::TyKind::Str),
        ty::TyKind::Adt(def, args) => {
            if tcx.is_lang_item(def.did(), LangItem::String) {
                return matches!(target_ty.kind(), ty::TyKind::Str);
            }
            if tcx.is_diagnostic_item(sym::Vec, def.did())
                && let Some(inner) = args.types().next()
            {
                return matches!(target_ty.kind(), ty::TyKind::Slice(target_inner) if same_erased_ty(inner, *target_inner, tcx));
            }
            false
        }
        _ => false,
    }
}

fn is_fn_solvable<'tcx>(fn_did: DefId, tcx: TyCtxt<'tcx>) -> bool {
    let fn_sig =
        fn_sig_with_generic_args(fn_did, ty::GenericArgs::identity_for_item(tcx, fn_did), tcx);
    if fn_sig
        .inputs()
        .iter()
        .any(|input_ty| input_ty.has_param() && !supports_input_unification(*input_ty))
    {
        return false;
    }

    tcx.predicates_of(fn_did)
        .instantiate_identity(tcx)
        .predicates
        .into_iter()
        .all(|predicate| {
            let Some(trait_predicate) = predicate.as_trait_clause() else {
                return true;
            };
            let trait_def_id = trait_predicate.skip_binder().trait_ref.def_id;
            if tcx.is_lang_item(trait_def_id, LangItem::Sized)
                || tcx.def_path_str(trait_def_id).ends_with("::MetaSized")
            {
                return true;
            }

            if is_low_cost_checked_common_trait(trait_def_id, tcx) {
                return true;
            }

            !tcx.is_lang_item(trait_def_id, LangItem::Fn)
                && !tcx.is_lang_item(trait_def_id, LangItem::FnMut)
                && !tcx.is_lang_item(trait_def_id, LangItem::FnOnce)
                && (trait_def_id.is_local() || is_supported_external_trait_bound(trait_def_id, tcx))
        })
}

fn supports_input_unification(ty: Ty<'_>) -> bool {
    match ty.kind() {
        ty::TyKind::Param(_) => true,
        ty::TyKind::Ref(_, inner, _) | ty::TyKind::RawPtr(inner, _) | ty::TyKind::Slice(inner) => {
            supports_input_unification(*inner)
        }
        ty::TyKind::Array(inner, _) => supports_input_unification(*inner),
        ty::TyKind::Tuple(types) => types.iter().all(|ty| supports_input_unification(ty)),
        ty::TyKind::Adt(_, args) => args.iter().all(|arg| {
            arg.as_type()
                .map(supports_input_unification)
                .unwrap_or(true)
        }),
        _ => !ty.has_param(),
    }
}

fn unify_ty<'tcx>(
    lhs: Ty<'tcx>,
    rhs: Ty<'tcx>,
    identity: &[ty::GenericArg<'tcx>],
    infcx: &InferCtxt<'tcx>,
    cause: &ObligationCause<'tcx>,
    param_env: ty::ParamEnv<'tcx>,
) -> Option<Mono<'tcx>> {
    infcx.probe(|_| {
        let _ = infcx
            .at(cause, param_env)
            .eq(DefineOpaqueTypes::Yes, lhs, rhs)
            .ok()?;

        let value = identity
            .iter()
            .map(|arg| match arg.kind() {
                ty::GenericArgKind::Lifetime(region) => {
                    infcx.resolve_vars_if_possible(region).into()
                }
                ty::GenericArgKind::Type(ty) => infcx.resolve_vars_if_possible(ty).into(),
                ty::GenericArgKind::Const(ct) => infcx.resolve_vars_if_possible(ct).into(),
            })
            .collect();
        Some(Mono { value })
    })
}

fn get_mono_set<'tcx>(
    fn_did: DefId,
    seed_args: &[ty::GenericArg<'tcx>],
    sorted_available_tys: &[TyWrapper<'tcx>],
    tcx: TyCtxt<'tcx>,
) -> MonoSet<'tcx> {
    let infcx = tcx
        .infer_ctxt()
        .ignoring_regions()
        .build(ty::TypingMode::PostAnalysis);
    let param_env = tcx.param_env(fn_did);
    let cause = ObligationCause::dummy();
    let fresh_args = infcx
        .fresh_args_for_item(rustc_span::DUMMY_SP, fn_did)
        .iter()
        .collect::<Vec<_>>();
    let fresh_args = apply_seed_args(seed_args, &fresh_args, tcx);
    let fn_sig = fn_sig_with_generic_args(fn_did, tcx.mk_args(&fresh_args), tcx);

    let mut set = MonoSet::all(&fresh_args);
    for input_ty in fn_sig.inputs().iter() {
        if !input_ty.has_infer_types() {
            continue;
        }

        let mut candidates = MonoSet::default();
        for available_ty in sorted_available_tys {
            if let Some(mono) = unify_ty(
                *input_ty,
                available_ty.ty(),
                &fresh_args,
                &infcx,
                &cause,
                param_env,
            ) {
                candidates.insert(mono);
            }
        }
        if !candidates.monos.is_empty() {
            debug!(
                target: "varies::mono",
                "mono input candidates for `{}`: {}",
                tcx.def_path_str(fn_did),
                candidates
                    .monos
                    .iter()
                    .take(8)
                    .map(|mono| mono_input_signature(fn_did, mono, tcx))
                    .collect::<Vec<_>>()
                    .join(" || ")
            );
        }
        set = set.merge(&candidates, tcx);
        if set.monos.is_empty() {
            break;
        }
        set = set.filter_by_partial_trait_bound(fn_did, tcx);
        if !set.monos.is_empty() {
            debug!(
                target: "varies::mono",
                "mono post-bound candidates for `{}`: {}",
                tcx.def_path_str(fn_did),
                set.monos
                    .iter()
                    .take(8)
                    .map(|mono| mono_input_signature(fn_did, mono, tcx))
                    .collect::<Vec<_>>()
                    .join(" || ")
            );
        }
        if set.monos.is_empty() {
            break;
        }
        set.truncate_preserving_common_seeds(MAX_MONOS_PER_FN, tcx);
    }
    set
}

fn mono_input_signature<'tcx>(fn_did: DefId, mono: &Mono<'tcx>, tcx: TyCtxt<'tcx>) -> String {
    let args = tcx.mk_args(&mono.value);
    fn_sig_with_generic_args(fn_did, args, tcx)
        .inputs()
        .iter()
        .map(|ty| tcx.erase_and_anonymize_regions(*ty).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn mono_limit_for_fn<'tcx>(fn_did: DefId, tcx: TyCtxt<'tcx>) -> usize {
    let fn_sig =
        fn_sig_with_generic_args(fn_did, ty::GenericArgs::identity_for_item(tcx, fn_did), tcx);
    if fn_sig.inputs().iter().all(|input| !input.has_param()) {
        MAX_OUTPUT_ONLY_MONOS_PER_FN
    } else {
        MAX_MONOS_PER_FN
    }
}

fn apply_seed_args<'tcx>(
    seed_args: &[ty::GenericArg<'tcx>],
    fresh_args: &[ty::GenericArg<'tcx>],
    tcx: TyCtxt<'tcx>,
) -> Vec<ty::GenericArg<'tcx>> {
    assert_eq!(seed_args.len(), fresh_args.len());
    seed_args
        .iter()
        .zip(fresh_args.iter())
        .map(
            |(seed_arg, fresh_arg)| match (seed_arg.kind(), fresh_arg.kind()) {
                (ty::GenericArgKind::Type(seed_ty), ty::GenericArgKind::Type(_))
                    if !matches!(seed_ty.kind(), ty::TyKind::Param(_)) =>
                {
                    tcx.erase_and_anonymize_regions(seed_ty).into()
                }
                _ => *fresh_arg,
            },
        )
        .collect()
}

fn unbound_generic_candidates<'tcx>(tcx: TyCtxt<'tcx>) -> Vec<Ty<'tcx>> {
    let mut candidates = vec![
        tcx.types.bool,
        tcx.types.char,
        tcx.types.u8,
        tcx.types.i8,
        tcx.types.i32,
        tcx.types.u32,
        tcx.types.f32,
        Ty::new_imm_ref(
            tcx,
            tcx.lifetimes.re_erased,
            Ty::new_slice(tcx, tcx.types.u8),
        ),
    ];

    if let Some(vec_def_id) = tcx.get_diagnostic_item(sym::Vec) {
        let vec_def = tcx.adt_def(vec_def_id);
        let vec_u8 = Ty::new_adt(tcx, vec_def, tcx.mk_args(&[tcx.types.u8.into()]));
        candidates.push(vec_u8);
    }
    if let Some(string_def_id) = tcx.lang_items().string() {
        let string_def = tcx.adt_def(string_def_id);
        candidates.push(Ty::new_adt(tcx, string_def, tcx.mk_args(&[])));
    }
    candidates.extend(common_symbolic_seed_value_tys(tcx));

    candidates
}

pub(crate) fn resolve_mono_apis<'tcx>(
    fn_did: DefId,
    available_tys: &HashSet<TyWrapper<'tcx>>,
    tcx: TyCtxt<'tcx>,
) -> Vec<Mono<'tcx>> {
    resolve_mono_apis_with_seeds(
        fn_did,
        std::slice::from_ref(&Mono {
            value: ty::GenericArgs::identity_for_item(tcx, fn_did).to_vec(),
        }),
        available_tys,
        tcx,
    )
}

pub(crate) fn resolve_mono_apis_with_seeds<'tcx>(
    fn_did: DefId,
    seeds: &[Mono<'tcx>],
    available_tys: &HashSet<TyWrapper<'tcx>>,
    tcx: TyCtxt<'tcx>,
) -> Vec<Mono<'tcx>> {
    if !is_fn_solvable(fn_did, tcx) {
        return Vec::new();
    }

    let mut sorted_available_tys = available_tys.iter().copied().collect::<Vec<_>>();
    sorted_available_tys.sort_by_key(|available_ty| {
        let ty = tcx.erase_and_anonymize_regions(available_ty.ty());
        (
            seed_candidate_priority(ty, tcx),
            usize::from(has_mutable_indirection(ty)),
            ty_ref_depth(ty),
            ty_complexity(ty),
            ty.to_string(),
        )
    });

    let mut ordered_seeds = seeds.to_vec();
    ordered_seeds.sort_by_key(|seed| std::cmp::Reverse(mono_seed_specificity(seed)));

    let mut resolved = MonoSet::default();
    let mono_limit = mono_limit_for_fn(fn_did, tcx);
    for seed in &ordered_seeds {
        let seeded = get_mono_set(fn_did, &seed.value, &sorted_available_tys, tcx)
            .instantiate_unbound(tcx)
            .expand_shared_ref_variants(tcx)
            .filter_by_trait_bound(fn_did, tcx);
        let mut seeded = seeded;
        seeded.truncate_preserving_common_seeds(mono_limit, tcx);
        for mono in seeded.monos {
            resolved.insert(mono);
        }
    }
    resolved.truncate_preserving_common_seeds(MAX_MONOS_PER_FN, tcx);
    resolved.erase_region_var(tcx);
    resolved
        .monos
        .retain(|mono| mono_is_fully_resolved(mono, tcx));
    resolved.monos
}

fn mono_is_fully_resolved<'tcx>(mono: &Mono<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    !mono.value.iter().any(|arg| match arg.kind() {
        ty::GenericArgKind::Lifetime(_) => false,
        ty::GenericArgKind::Type(ty) => {
            let ty = tcx.erase_and_anonymize_regions(ty);
            ty.has_param()
                || ty.has_infer_types()
                || ty.has_escaping_bound_vars()
                || ty.has_placeholders()
        }
        ty::GenericArgKind::Const(ct) => {
            let ct = tcx.erase_and_anonymize_regions(ct);
            ct.has_param() || ct.has_infer()
        }
    })
}
