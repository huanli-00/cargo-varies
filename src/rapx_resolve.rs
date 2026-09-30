// SPDX-License-Identifier: MPL-2.0
// Derived from RAPx; see NOTICE.
use crate::rapx_graph::{ApiDependencyGraph, TyWrapper, fn_sig_with_generic_args};
use crate::rapx_mono::{self, Mono};
use crate::std_adapters::{common_symbolic_seed_value_tys, kani_seed_ty_for_input};
use crate::type_relations::{can_supply_to_input, type_contains_unsizing_target};
use log::debug;
use rustc_hir::def_id::DefId;
use rustc_hir::intravisit::Visitor;
use rustc_infer::infer::TyCtxtInferExt;
use rustc_middle::ty::{self, Ty, TyCtxt, TyKind, TypeVisitableExt, Upcast};
use rustc_span::sym;
use rustc_trait_selection::traits::Obligation;
use rustc_trait_selection::traits::query::evaluate_obligation::InferCtxtExt as _;
use std::collections::{HashMap, HashSet};
use std::env;

const MAX_TY_COMPLEXITY: usize = 6;
const MAX_RESOLVE_ITERATIONS: usize = 10;
const MAX_IMPL_SEED_CANDIDATES: usize = 16;
const MAX_IMPL_SEED_VARIANTS: usize = 32;

#[derive(Clone)]
struct TypeCandidates<'tcx> {
    tcx: TyCtxt<'tcx>,
    candidates: HashSet<TyWrapper<'tcx>>,
}

impl<'tcx> TypeCandidates<'tcx> {
    fn new(tcx: TyCtxt<'tcx>) -> Self {
        Self {
            tcx,
            candidates: HashSet::new(),
        }
    }

    fn candidates(&self) -> &HashSet<TyWrapper<'tcx>> {
        &self.candidates
    }

    fn insert(&mut self, ty: Ty<'tcx>) -> bool {
        if ty_complexity(ty) > MAX_TY_COMPLEXITY {
            return false;
        }
        self.candidates.insert(ty.into())
    }

    fn insert_all(&mut self, ty: Ty<'tcx>) -> bool {
        let complexity = ty_complexity(ty);
        if complexity > MAX_TY_COMPLEXITY {
            return false;
        }

        let mut changed = self.insert(ty);
        if complexity < MAX_TY_COMPLEXITY {
            changed |= self.insert(Ty::new_ref(
                self.tcx,
                self.tcx.lifetimes.re_erased,
                ty,
                ty::Mutability::Not,
            ));
            changed |= self.insert(Ty::new_ref(
                self.tcx,
                self.tcx.lifetimes.re_erased,
                ty,
                ty::Mutability::Mut,
            ));
        }
        if should_extend_with_slice_variants(ty) && complexity + 2 <= MAX_TY_COMPLEXITY {
            let slice = Ty::new_slice(self.tcx, ty);
            changed |= self.insert(Ty::new_ref(
                self.tcx,
                self.tcx.lifetimes.re_erased,
                slice,
                ty::Mutability::Not,
            ));
            changed |= self.insert(Ty::new_ref(
                self.tcx,
                self.tcx.lifetimes.re_erased,
                slice,
                ty::Mutability::Mut,
            ));
        }

        changed
    }

    fn add_prelude_tys(&mut self) {
        let tcx = self.tcx;
        let mut tys = vec![
            tcx.types.bool,
            tcx.types.char,
            tcx.types.f32,
            tcx.types.f64,
            tcx.types.i8,
            tcx.types.i16,
            tcx.types.i32,
            tcx.types.i64,
            tcx.types.isize,
            tcx.types.u8,
            tcx.types.u16,
            tcx.types.u32,
            tcx.types.u64,
            tcx.types.usize,
            Ty::new_imm_ref(tcx, tcx.lifetimes.re_erased, tcx.types.str_),
        ];

        if let Some(vec_def_id) = tcx.get_diagnostic_item(sym::Vec) {
            let vec_def = tcx.adt_def(vec_def_id);
            tys.push(Ty::new_adt(
                tcx,
                vec_def,
                tcx.mk_args(&[tcx.types.u8.into()]),
            ));
        }
        if let Some(string_def_id) = tcx.lang_items().string() {
            tys.push(Ty::new_adt(
                tcx,
                tcx.adt_def(string_def_id),
                tcx.mk_args(&[]),
            ));
        }
        let common_seed_tys = common_symbolic_seed_value_tys(tcx);
        debug!(
            target: "varies::resolve",
            "common symbolic seed tys: {}",
            common_seed_tys
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        tys.extend(common_seed_tys);
        for ty in tys {
            self.insert_all(ty);
        }
    }

    fn add_public_local_tys(&mut self) {
        let tcx = self.tcx;
        let mut visitor = PublicTypeSeedVisitor {
            tcx,
            candidates: self,
        };
        tcx.hir_visit_all_item_likes_in_crate(&mut visitor);
    }
}

fn should_extend_with_slice_variants(ty: Ty<'_>) -> bool {
    !matches!(
        ty.kind(),
        TyKind::Ref(..) | TyKind::RawPtr(..) | TyKind::Slice(_) | TyKind::Str | TyKind::Dynamic(..)
    )
}

struct PublicTypeSeedVisitor<'tcx, 'a> {
    tcx: TyCtxt<'tcx>,
    candidates: &'a mut TypeCandidates<'tcx>,
}

impl<'tcx> Visitor<'tcx> for PublicTypeSeedVisitor<'tcx, '_> {
    fn visit_item(&mut self, item: &'tcx rustc_hir::Item<'tcx>) -> Self::Result {
        if matches!(
            item.kind,
            rustc_hir::ItemKind::Struct(..) | rustc_hir::ItemKind::Enum(..)
        ) {
            let def_id = item.owner_id.to_def_id();
            let local_def_id = def_id.expect_local();
            let generics = self.tcx.generics_of(def_id);
            if self
                .tcx
                .effective_visibilities(())
                .is_exported(local_def_id)
                && !generics.requires_monomorphization(self.tcx)
            {
                let ty = self.tcx.type_of(def_id).instantiate_identity();
                if !ty.has_param()
                    && !ty.has_infer_types()
                    && is_symbolically_reconstructible_ty(ty, self.tcx)
                {
                    self.candidates.insert_all(ty);
                }
            }
        }
        rustc_hir::intravisit::walk_item(self, item);
    }
}

pub(crate) fn partition_generic_apis<'tcx>(
    all_apis: &HashSet<DefId>,
    tcx: TyCtxt<'tcx>,
) -> (HashSet<DefId>, HashSet<DefId>) {
    let mut non_generic = HashSet::new();
    let mut generic = HashSet::new();
    for api in all_apis {
        if tcx.generics_of(*api).requires_monomorphization(tcx) {
            generic.insert(*api);
        } else {
            non_generic.insert(*api);
        }
    }
    (non_generic, generic)
}

fn collect_output_ty_if_inputs_resolvable<'tcx>(
    fn_did: DefId,
    args: ty::GenericArgsRef<'tcx>,
    reachable_tys: &HashSet<TyWrapper<'tcx>>,
    new_tys: &mut HashSet<Ty<'tcx>>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    let fn_sig = fn_sig_with_generic_args(fn_did, args, tcx);
    if !inputs_are_resolvable_from_candidates(fn_sig.inputs(), reachable_tys, tcx) {
        return false;
    }

    let output_ty = fn_sig.output();
    if !output_ty.is_unit() {
        new_tys.insert(output_ty);
    }
    true
}

pub(crate) fn resolve_generic_apis<'tcx>(graph: &mut ApiDependencyGraph<'tcx>) {
    let generic_map = resolve_reachable_api_monos(graph);
    for (fn_did, monos) in generic_map {
        for mono in monos {
            graph.add_api(fn_did, &mono.value);
        }
    }
}

// Grow the reachable type frontier until bounded mono expansion stops discovering
// new output types for generic APIs.
fn resolve_reachable_api_monos<'tcx>(
    graph: &ApiDependencyGraph<'tcx>,
) -> HashMap<DefId, Vec<Mono<'tcx>>> {
    let tcx = graph.tcx();
    let mut type_candidates = TypeCandidates::new(tcx);
    type_candidates.add_prelude_tys();
    type_candidates.add_public_local_tys();

    let (mut non_generic_apis, generic_apis) = partition_generic_apis(graph.all_apis(), tcx);
    let mut generic_map: HashMap<DefId, Vec<Mono<'tcx>>> = HashMap::new();

    let max_iterations = env_usize("VARIES_MAX_RESOLVE_ITERATIONS", MAX_RESOLVE_ITERATIONS);
    for iteration in 0..max_iterations {
        let reachable_tys = type_candidates.candidates();
        let mut new_tys = HashSet::new();
        let mut trait_impl_candidates_cache: HashMap<DefId, Vec<ty::TraitRef<'tcx>>> =
            HashMap::new();
        debug!(
            target: "varies::resolve",
            "generic resolve iteration {}: {} reachable tys, {} generic apis",
            iteration + 1,
            reachable_tys.len(),
            generic_apis.len()
        );

        non_generic_apis.retain(|fn_did| {
            !collect_output_ty_if_inputs_resolvable(
                *fn_did,
                ty::GenericArgs::identity_for_item(tcx, *fn_did),
                reachable_tys,
                &mut new_tys,
                tcx,
            )
        });

        for (api_offset, fn_did) in generic_apis.iter().enumerate() {
            if api_offset % 50 == 0 {
                debug!(
                    target: "varies::resolve",
                    "generic resolve iteration {} visiting {}/{}: `{}`",
                    iteration + 1,
                    api_offset + 1,
                    generic_apis.len(),
                    tcx.def_path_str(*fn_did)
                );
            }
            let seeds = resolve_fn_seed_monos(
                *fn_did,
                reachable_tys,
                tcx,
                &mut trait_impl_candidates_cache,
            );
            let monos = if seeds.len() == 1
                && seeds[0].value == ty::GenericArgs::identity_for_item(tcx, *fn_did).to_vec()
            {
                rapx_mono::resolve_mono_apis(*fn_did, reachable_tys, tcx)
            } else {
                rapx_mono::resolve_mono_apis_with_seeds(*fn_did, &seeds, reachable_tys, tcx)
            };
            if !monos.is_empty() {
                debug!(
                    target: "varies::resolve",
                    "generic api `{}` resolved {} mono candidates: {}",
                    tcx.def_path_str(*fn_did),
                    monos.len(),
                    monos
                        .iter()
                        .map(|mono| {
                            let args = tcx.mk_args(&mono.value);
                            fn_sig_with_generic_args(*fn_did, args, tcx)
                                .inputs()
                                .iter()
                                .map(|ty| tcx.erase_and_anonymize_regions(*ty).to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .collect::<Vec<_>>()
                        .join(" || ")
                );
            }
            for mono in monos {
                let output_ty =
                    fn_sig_with_generic_args(*fn_did, tcx.mk_args(&mono.value), tcx).output();
                let entry = generic_map.entry(*fn_did).or_default();
                if entry.contains(&mono) {
                    continue;
                }
                entry.push(mono);
                if !output_ty.is_unit() {
                    new_tys.insert(output_ty);
                }
            }
        }

        let mut changed = false;
        for ty in new_tys {
            changed |= type_candidates.insert_all(ty);
        }
        if !changed {
            break;
        }
    }

    generic_map
}

fn env_usize(name: &str, default: usize) -> usize {
    match env::var(name) {
        Ok(value) => value
            .parse::<usize>()
            .unwrap_or_else(|error| panic!("{name} must be usize, got `{value}`: {error}")),
        Err(_) => default,
    }
}

fn is_symbolically_reconstructible_ty<'tcx>(ty: Ty<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    kani_seed_ty_for_input(tcx, ty).is_some()
}

fn inputs_are_resolvable_from_candidates<'tcx>(
    inputs: &[Ty<'tcx>],
    reachable_tys: &HashSet<TyWrapper<'tcx>>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    inputs.iter().all(|input_ty| {
        if type_contains_unsizing_target(*input_ty) {
            is_symbolically_reconstructible_ty(*input_ty, tcx)
                || reachable_tys.contains(&TyWrapper::from(*input_ty))
                || reachable_tys.iter().any(|candidate| {
                    unsized_input_is_resolvable_from_candidate(candidate.ty(), *input_ty, tcx)
                })
        } else {
            is_symbolically_reconstructible_ty(*input_ty, tcx)
                || reachable_tys.contains(&TyWrapper::from(*input_ty))
                || reachable_tys
                    .iter()
                    .any(|candidate| can_supply_to_input(tcx, candidate.ty(), *input_ty))
        }
    })
}

fn unsized_input_is_resolvable_from_candidate<'tcx>(
    output: Ty<'tcx>,
    input: Ty<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    let output = tcx.erase_and_anonymize_regions(output);
    let input = tcx.erase_and_anonymize_regions(input);
    if output == input {
        return true;
    }

    let inner = match input.kind() {
        TyKind::Ref(_, inner, _) | TyKind::RawPtr(inner, _) => *inner,
        _ => return false,
    };
    let inner = tcx.erase_and_anonymize_regions(inner);

    match inner.kind() {
        TyKind::Slice(input_elem) => matches_vec_or_slice_like(output, *input_elem, tcx),
        TyKind::Str => is_string_like_output(output, tcx),
        TyKind::Dynamic(predicates, ..) => {
            concrete_output_satisfies_dyn_predicates(output, predicates, tcx)
        }
        _ => false,
    }
}

fn matches_vec_or_slice_like<'tcx>(
    output: Ty<'tcx>,
    input_elem: Ty<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    let input_elem = tcx.erase_and_anonymize_regions(input_elem);
    match output.kind() {
        TyKind::Slice(output_elem) | TyKind::Array(output_elem, _) => {
            tcx.erase_and_anonymize_regions(*output_elem) == input_elem
        }
        TyKind::Adt(def, args) if tcx.is_diagnostic_item(sym::Vec, def.did()) => args
            .types()
            .next()
            .is_some_and(|output_elem| tcx.erase_and_anonymize_regions(output_elem) == input_elem),
        _ => false,
    }
}

fn is_string_like_output(output: Ty<'_>, tcx: TyCtxt<'_>) -> bool {
    match output.kind() {
        TyKind::Str => true,
        TyKind::Adt(def, _) => tcx.lang_items().string() == Some(def.did()),
        _ => false,
    }
}

fn concrete_output_satisfies_dyn_predicates<'tcx>(
    output: Ty<'tcx>,
    predicates: &'tcx ty::List<ty::PolyExistentialPredicate<'tcx>>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    if matches!(output.kind(), TyKind::Ref(..) | TyKind::RawPtr(..)) {
        return false;
    }

    let principal_ok = predicates.principal().is_none_or(|principal| {
        let trait_ref =
            tcx.instantiate_bound_regions_with_erased(principal.with_self_ty(tcx, output));
        concrete_trait_ref_implements_trait(trait_ref, tcx)
    });
    if !principal_ok {
        return false;
    }

    predicates
        .iter()
        .all(|predicate| match predicate.skip_binder() {
            ty::ExistentialPredicate::AutoTrait(def_id) => {
                concrete_trait_ref_implements_trait(ty::TraitRef::new(tcx, def_id, [output]), tcx)
            }
            ty::ExistentialPredicate::Trait(_) => true,
            ty::ExistentialPredicate::Projection(_) => {
                concrete_clause_holds(predicate.with_self_ty(tcx, output), tcx)
            }
        })
}

fn ty_is_safe_impl_seed_candidate<'tcx>(
    ty: Ty<'tcx>,
    reachable_tys: &HashSet<TyWrapper<'tcx>>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    let ty = tcx.erase_and_anonymize_regions(ty);
    if ty.has_param() || ty.has_infer_types() {
        return false;
    }
    if is_symbolically_reconstructible_ty(ty, tcx) || reachable_tys.contains(&TyWrapper::from(ty)) {
        return true;
    }

    match ty.kind() {
        TyKind::Ref(_, inner, _) | TyKind::RawPtr(inner, _) => {
            let inner = tcx.erase_and_anonymize_regions(*inner);
            is_symbolically_reconstructible_ty(inner, tcx)
                || reachable_tys.contains(&TyWrapper::from(inner))
        }
        _ => false,
    }
}

fn resolve_fn_seed_monos<'tcx>(
    fn_did: DefId,
    reachable_tys: &HashSet<TyWrapper<'tcx>>,
    tcx: TyCtxt<'tcx>,
    trait_impl_candidates_cache: &mut HashMap<DefId, Vec<ty::TraitRef<'tcx>>>,
) -> Vec<Mono<'tcx>> {
    let mut seeds = vec![Mono {
        value: ty::GenericArgs::identity_for_item(tcx, fn_did).to_vec(),
    }];
    let mut seen_seeds = seeds.iter().cloned().collect::<HashSet<_>>();
    if let Some(self_seeds) =
        resolve_trait_self_seeds(fn_did, reachable_tys, tcx, trait_impl_candidates_cache)
    {
        for seed in self_seeds {
            if seen_seeds.insert(seed.clone()) {
                seeds.push(seed);
            }
        }
    }
    for seed in resolve_trait_bound_seeds(fn_did, reachable_tys, tcx, trait_impl_candidates_cache) {
        if seen_seeds.insert(seed.clone()) {
            seeds.push(seed);
        }
    }
    seeds
}

// For provided trait methods, seed the generic `Self` slot from concrete impls
// so mono resolution starts from receiver types that can actually occur.
fn resolve_trait_self_seeds<'tcx>(
    fn_did: DefId,
    reachable_tys: &HashSet<TyWrapper<'tcx>>,
    tcx: TyCtxt<'tcx>,
    trait_impl_candidates_cache: &mut HashMap<DefId, Vec<ty::TraitRef<'tcx>>>,
) -> Option<Vec<Mono<'tcx>>> {
    let assoc = tcx.opt_associated_item(fn_did)?;
    if !matches!(assoc.container, ty::AssocContainer::Trait) {
        return None;
    }

    let self_slot = ty::GenericArgs::identity_for_item(tcx, fn_did)
        .iter()
        .enumerate()
        .filter(|(_, arg)| matches!(arg.kind(), ty::GenericArgKind::Type(ty) if matches!(ty.kind(), TyKind::Param(_))))
        .map(|(index, _)| index)
        .next()?;

    let trait_id = assoc.container_id(tcx);
    let mut seeds = Vec::new();
    let mut seen_seeds = HashSet::new();
    for trait_ref in impl_trait_ref_candidates_for_trait(
        trait_id,
        reachable_tys,
        tcx,
        trait_impl_candidates_cache,
    ) {
        let self_ty = tcx.erase_and_anonymize_regions(trait_ref.self_ty());
        if self_ty.has_param() || self_ty.has_infer_types() {
            continue;
        }
        let mut args = ty::GenericArgs::identity_for_item(tcx, fn_did)
            .iter()
            .collect::<Vec<_>>();
        args[self_slot] = self_ty.into();
        let seed = Mono { value: args };
        if seen_seeds.insert(seed.clone()) {
            seeds.push(seed);
        }
    }

    Some(seeds)
}

fn resolve_trait_bound_seeds<'tcx>(
    fn_did: DefId,
    reachable_tys: &HashSet<TyWrapper<'tcx>>,
    tcx: TyCtxt<'tcx>,
    trait_impl_candidates_cache: &mut HashMap<DefId, Vec<ty::TraitRef<'tcx>>>,
) -> Vec<Mono<'tcx>> {
    let identity = ty::GenericArgs::identity_for_item(tcx, fn_did)
        .iter()
        .collect::<Vec<_>>();
    let mut seeds = Vec::new();
    let mut seen_seeds = HashSet::new();

    for predicate in tcx
        .predicates_of(fn_did)
        .instantiate_identity(tcx)
        .predicates
    {
        let Some(trait_predicate) = predicate.as_trait_clause() else {
            continue;
        };
        let trait_ref = trait_predicate.skip_binder().trait_ref;
        if !trait_ref.def_id.is_local() && !is_supported_external_trait_seed(trait_ref.def_id, tcx)
        {
            continue;
        }

        for impl_trait_ref in impl_trait_ref_candidates_for_trait(
            trait_ref.def_id,
            reachable_tys,
            tcx,
            trait_impl_candidates_cache,
        ) {
            let Some(seed) =
                seed_mono_from_impl_trait_ref(&identity, trait_ref, impl_trait_ref, tcx)
            else {
                continue;
            };
            if seen_seeds.insert(seed.clone()) {
                seeds.push(seed);
            }
        }
    }

    seeds
}

fn impl_trait_ref_candidates_for_trait<'tcx>(
    trait_def_id: DefId,
    reachable_tys: &HashSet<TyWrapper<'tcx>>,
    tcx: TyCtxt<'tcx>,
    cache: &mut HashMap<DefId, Vec<ty::TraitRef<'tcx>>>,
) -> Vec<ty::TraitRef<'tcx>> {
    if let Some(cached) = cache.get(&trait_def_id) {
        return cached.clone();
    }

    let mut candidates = Vec::new();
    let mut seen_candidates = HashSet::new();
    for impl_def_id in tcx.all_impls(trait_def_id) {
        for trait_ref in instantiate_impl_trait_ref_candidates(impl_def_id, reachable_tys, tcx) {
            if seen_candidates.insert(trait_ref) {
                candidates.push(trait_ref);
            }
        }
    }
    cache.insert(trait_def_id, candidates.clone());
    candidates
}

fn is_supported_external_trait_seed(trait_def_id: DefId, tcx: TyCtxt<'_>) -> bool {
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

// Build a bounded set of instantiated impl trait refs by substituting
// reachable concrete types for the impl's type parameters.
fn instantiate_impl_trait_ref_candidates<'tcx>(
    impl_def_id: DefId,
    reachable_tys: &HashSet<TyWrapper<'tcx>>,
    tcx: TyCtxt<'tcx>,
) -> Vec<ty::TraitRef<'tcx>> {
    let impl_trait_ref = tcx.impl_trait_ref(impl_def_id);

    let identity = ty::GenericArgs::identity_for_item(tcx, impl_def_id)
        .iter()
        .collect::<Vec<_>>();
    if !identity.iter().any(|arg| matches!(arg.kind(), ty::GenericArgKind::Type(ty) if matches!(ty.kind(), TyKind::Param(_)))) {
        return vec![impl_trait_ref.instantiate_identity()];
    }

    let mut candidate_tys = reachable_tys
        .iter()
        .map(|wrapper| tcx.erase_and_anonymize_regions(wrapper.ty()))
        .filter(|ty| !ty.has_param() && !ty.has_infer_types())
        .collect::<Vec<_>>();
    let mut common_seed_tys = common_symbolic_seed_value_tys(tcx)
        .into_iter()
        .map(|ty| tcx.erase_and_anonymize_regions(ty))
        .collect::<Vec<_>>();
    candidate_tys.extend(common_seed_tys.iter().copied());
    candidate_tys.sort_by_key(|ty| {
        (
            usize::from(!is_symbolically_reconstructible_ty(*ty, tcx)),
            ty_complexity(*ty),
            ty.to_string(),
        )
    });
    candidate_tys.dedup();
    common_seed_tys.sort_by_key(|ty| (ty_complexity(*ty), ty.to_string()));
    common_seed_tys.dedup();

    let mut common = candidate_tys
        .iter()
        .copied()
        .filter(|ty| common_seed_tys.contains(ty))
        .collect::<Vec<_>>();
    let mut preferred = candidate_tys
        .iter()
        .copied()
        .filter(|ty| !common_seed_tys.contains(ty) && is_symbolically_reconstructible_ty(*ty, tcx))
        .collect::<Vec<_>>();
    let mut others = candidate_tys
        .iter()
        .copied()
        .filter(|ty| !common_seed_tys.contains(ty) && !is_symbolically_reconstructible_ty(*ty, tcx))
        .collect::<Vec<_>>();
    if common.len() > MAX_IMPL_SEED_CANDIDATES {
        common.truncate(MAX_IMPL_SEED_CANDIDATES);
        preferred.clear();
        others.clear();
    } else {
        let preferred_limit = MAX_IMPL_SEED_CANDIDATES.saturating_sub(common.len());
        if preferred.len() > preferred_limit {
            preferred.truncate(preferred_limit);
            others.clear();
        } else {
            others.truncate(preferred_limit.saturating_sub(preferred.len()));
        }
    }
    let mut candidate_tys = common;
    candidate_tys.extend(preferred);
    candidate_tys.extend(others);

    let mut arg_sets = vec![identity.clone()];
    for (index, arg) in identity.iter().enumerate() {
        let ty::GenericArgKind::Type(ty) = arg.kind() else {
            if matches!(arg.kind(), ty::GenericArgKind::Const(_)) {
                return Vec::new();
            }
            continue;
        };
        if !matches!(ty.kind(), TyKind::Param(_)) {
            continue;
        }

        let mut next = Vec::new();
        for prefix in arg_sets {
            for candidate_ty in &candidate_tys {
                let mut args = prefix.clone();
                args[index] = (*candidate_ty).into();
                next.push(args);
                if next.len() >= MAX_IMPL_SEED_VARIANTS {
                    break;
                }
            }
            if next.len() >= MAX_IMPL_SEED_VARIANTS {
                break;
            }
        }
        arg_sets = next;
        if arg_sets.is_empty() {
            return Vec::new();
        }
    }

    let mut variants = Vec::new();
    for args in arg_sets {
        let impl_args = tcx.mk_args(&args);
        let instantiated = impl_trait_ref.instantiate(tcx, impl_args);
        let self_ty = tcx.erase_and_anonymize_regions(instantiated.self_ty());
        if !ty_is_safe_impl_seed_candidate(self_ty, reachable_tys, tcx) {
            continue;
        }
        if !impl_where_clauses_hold(impl_def_id, impl_args, instantiated, tcx) {
            continue;
        }
        if !variants.contains(&instantiated) {
            variants.push(instantiated);
        }
    }
    variants
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
    tcx: TyCtxt<'tcx>,
) -> bool {
    if !trait_ref_is_concrete(trait_ref, tcx) {
        return false;
    }

    concrete_clause_holds(trait_ref.upcast(tcx), tcx)
}

fn impl_where_clauses_hold<'tcx>(
    impl_def_id: DefId,
    args: ty::GenericArgsRef<'tcx>,
    instantiated_trait_ref: ty::TraitRef<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    tcx.predicates_of(impl_def_id)
        .instantiate(tcx, args)
        .predicates
        .into_iter()
        .all(|clause| match clause.kind().skip_binder() {
            ty::ClauseKind::Trait(trait_predicate)
                if trait_predicate.trait_ref.def_id == instantiated_trait_ref.def_id =>
            {
                true
            }
            ty::ClauseKind::Trait(trait_predicate)
                if tcx.trait_is_auto(trait_predicate.trait_ref.def_id)
                    || (!trait_predicate.trait_ref.def_id.is_local()
                        && !is_supported_external_trait_seed(
                            trait_predicate.trait_ref.def_id,
                            tcx,
                        )) =>
            {
                true
            }
            _ => concrete_clause_holds(clause, tcx),
        })
}

fn concrete_clause_holds<'tcx>(clause: ty::Clause<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
    let predicate: ty::Predicate<'tcx> = tcx.erase_and_anonymize_regions(clause.upcast(tcx));
    let obligation = Obligation::new(
        tcx,
        rustc_infer::traits::ObligationCause::dummy(),
        ty::ParamEnv::empty(),
        predicate,
    );

    tcx.infer_ctxt()
        .ignoring_regions()
        .build(ty::TypingMode::PostAnalysis)
        .predicate_must_hold_modulo_regions(&obligation)
}

fn seed_mono_from_impl_trait_ref<'tcx>(
    identity: &[ty::GenericArg<'tcx>],
    predicate_trait_ref: ty::TraitRef<'tcx>,
    impl_trait_ref: ty::TraitRef<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> Option<Mono<'tcx>> {
    if predicate_trait_ref.def_id != impl_trait_ref.def_id
        || predicate_trait_ref.args.len() != impl_trait_ref.args.len()
    {
        return None;
    }

    let mut value = identity.to_vec();
    let mut changed = false;
    for (predicate_arg, impl_arg) in predicate_trait_ref
        .args
        .iter()
        .zip(impl_trait_ref.args.iter())
    {
        match (predicate_arg.kind(), impl_arg.kind()) {
            (ty::GenericArgKind::Lifetime(_), ty::GenericArgKind::Lifetime(_)) => {}
            (ty::GenericArgKind::Type(predicate_ty), ty::GenericArgKind::Type(impl_ty)) => {
                let predicate_ty = tcx.erase_and_anonymize_regions(predicate_ty);
                let impl_ty = tcx.erase_and_anonymize_regions(impl_ty);
                if let Some(slot) = identity.iter().position(|arg| {
                    arg.as_type().is_some_and(|candidate_ty| {
                        tcx.erase_and_anonymize_regions(candidate_ty) == predicate_ty
                    })
                }) {
                    if impl_ty.has_param() || impl_ty.has_infer_types() {
                        return None;
                    }
                    value[slot] = impl_ty.into();
                    changed = true;
                } else if predicate_ty != impl_ty {
                    return None;
                }
            }
            (ty::GenericArgKind::Const(predicate_ct), ty::GenericArgKind::Const(impl_ct)) => {
                let predicate_ct = tcx.erase_and_anonymize_regions(predicate_ct);
                let impl_ct = tcx.erase_and_anonymize_regions(impl_ct);
                if predicate_ct != impl_ct {
                    return None;
                }
            }
            _ => return None,
        }
    }

    if !changed {
        return None;
    }

    let seed = Mono { value };
    if seed.value.iter().any(|arg| {
        arg.as_type().is_some_and(|ty| {
            let ty = tcx.erase_and_anonymize_regions(ty);
            ty.has_param() || ty.has_infer_types()
        })
    }) {
        return None;
    }
    Some(seed)
}

fn ty_complexity<'tcx>(ty: Ty<'tcx>) -> usize {
    match ty.kind() {
        TyKind::Ref(_, inner, _) | TyKind::Array(inner, _) | TyKind::Slice(inner) => {
            ty_complexity(*inner) + 1
        }
        TyKind::Tuple(tys) => tys.iter().fold(0, |max, ty| max.max(ty_complexity(ty))) + 1,
        TyKind::Adt(_, args) => {
            args.iter().fold(0, |max, arg| {
                max.max(arg.as_type().map(ty_complexity).unwrap_or(0))
            }) + 1
        }
        _ => 1,
    }
}
