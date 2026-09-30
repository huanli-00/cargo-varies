use rustc_infer::infer::TyCtxtInferExt;
use rustc_middle::traits::ObligationCause;
use rustc_middle::ty::{self, Ty, TyCtxt, TyKind};
use rustc_trait_selection::infer::InferCtxtExt as _;
use rustc_trait_selection::traits::ObligationCtxt;

pub(crate) fn can_supply_to_input<'tcx>(
    tcx: TyCtxt<'tcx>,
    output: Ty<'tcx>,
    input: Ty<'tcx>,
) -> bool {
    match input.kind() {
        TyKind::Ref(region, _, mutability) => {
            if region.is_static() {
                return false;
            }
            let source = Ty::new_ref(tcx, tcx.lifetimes.re_erased, output, *mutability);
            borrowed_source_satisfies_input(tcx, source, input)
        }
        TyKind::RawPtr(_, mutability) => {
            let source = Ty::new_ptr(tcx, output, *mutability);
            supplied_ty_satisfies_input(tcx, source, input)
        }
        _ => supplied_ty_satisfies_input(tcx, output, input),
    }
}

pub(crate) fn input_requires_constructor_scan(input: Ty<'_>) -> bool {
    type_contains_unsizing_target(input)
}

fn borrowed_source_satisfies_input<'tcx>(
    tcx: TyCtxt<'tcx>,
    output: Ty<'tcx>,
    input: Ty<'tcx>,
) -> bool {
    if tcx.erase_and_anonymize_regions(output) == tcx.erase_and_anonymize_regions(input) {
        return true;
    }
    if !type_contains_unsizing_target(input) {
        return false;
    }

    let Some(coerce_unsized_did) = tcx.lang_items().coerce_unsized_trait() else {
        return false;
    };

    tcx.infer_ctxt()
        .build(ty::TypingMode::PostAnalysis)
        .type_implements_trait(coerce_unsized_did, [output, input], ty::ParamEnv::empty())
        .must_apply_modulo_regions()
}

fn supplied_ty_satisfies_input<'tcx>(tcx: TyCtxt<'tcx>, output: Ty<'tcx>, input: Ty<'tcx>) -> bool {
    if output == input {
        return true;
    }
    if type_is_subtype_of(tcx, output, input) {
        return true;
    }
    if !type_contains_unsizing_target(input) {
        return false;
    }

    let Some(coerce_unsized_did) = tcx.lang_items().coerce_unsized_trait() else {
        return false;
    };

    tcx.infer_ctxt()
        .build(ty::TypingMode::PostAnalysis)
        .type_implements_trait(coerce_unsized_did, [output, input], ty::ParamEnv::empty())
        .must_apply_modulo_regions()
}

fn type_is_subtype_of<'tcx>(tcx: TyCtxt<'tcx>, output: Ty<'tcx>, input: Ty<'tcx>) -> bool {
    let infcx = tcx.infer_ctxt().build(ty::TypingMode::PostAnalysis);
    let ocx = ObligationCtxt::new(&infcx);
    let cause = ObligationCause::dummy();
    if ocx
        .sub(&cause, ty::ParamEnv::empty(), output, input)
        .is_err()
    {
        return false;
    }
    ocx.evaluate_obligations_error_on_ambiguity().is_empty()
}

pub(crate) fn type_contains_unsizing_target(ty: Ty<'_>) -> bool {
    match ty.kind() {
        TyKind::Dynamic(..) | TyKind::Slice(_) | TyKind::Str => true,
        TyKind::Ref(_, inner, _) | TyKind::RawPtr(inner, _) => {
            type_contains_unsizing_target(*inner)
        }
        TyKind::Adt(_, args) => args.types().any(type_contains_unsizing_target),
        TyKind::Tuple(types) => types.iter().any(type_contains_unsizing_target),
        TyKind::Array(inner, _) => type_contains_unsizing_target(*inner),
        _ => false,
    }
}
