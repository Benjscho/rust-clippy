use super::EXPLICIT_ITER_LOOP_DEREF;
use clippy_utils::diagnostics::span_lint_and_sugg;
use clippy_utils::msrvs::{self, Msrv};
use clippy_utils::source::snippet_with_applicability;
use clippy_utils::ty::{
    implements_trait, implements_trait_with_env, is_copy, make_normalized_projection,
    make_normalized_projection_with_regions, normalize_with_regions,
};
use rustc_errors::Applicability;
use rustc_hir::{Expr, Mutability};
use rustc_lint::LateContext;
use rustc_middle::ty::adjustment::{Adjust, Adjustment, AutoBorrow, AutoBorrowMutability};
use rustc_middle::ty::{self, EarlyBinder, Ty, TypeAndMut};
use rustc_span::sym;

pub(super) fn check(cx: &LateContext<'_>, self_arg: &Expr<'_>, call_expr: &Expr<'_>, msrv: &Msrv) {
    let Some((adjust, ty)) = is_ref_iterable(cx, self_arg, call_expr) else {
        return;
    };
    if let ty::Array(_, count) = *ty.peel_refs().kind() {
        if !ty.is_ref() {
            if !msrv.meets(msrvs::ARRAY_INTO_ITERATOR) {
                return;
            }
        } else if count
            .try_eval_target_usize(cx.tcx, cx.param_env)
            .map_or(true, |x| x > 32)
            && !msrv.meets(msrvs::ARRAY_IMPL_ANY_LEN)
        {
            return;
        }
    }

    let mut applicability = Applicability::MachineApplicable;
    let object = snippet_with_applicability(cx, self_arg.span, "_", &mut applicability);
    span_lint_and_sugg(
        cx,
        EXPLICIT_ITER_LOOP_DEREF,
        call_expr.span,
        "it is more concise to loop over references to containers instead of using explicit \
         iteration methods",
        "to write this more concisely, try",
        format!("{}{object}", adjust.display()),
        applicability,
    );
}

#[derive(Clone, Copy)]
enum AdjustKind {
    Deref,
    Reborrow,
    ReborrowMut,
}
impl AdjustKind {
    fn reborrow(mutbl: Mutability) -> Self {
        match mutbl {
            Mutability::Not => Self::Reborrow,
            Mutability::Mut => Self::ReborrowMut,
        }
    }

    fn auto_reborrow(mutbl: AutoBorrowMutability) -> Self {
        match mutbl {
            AutoBorrowMutability::Not => Self::Reborrow,
            AutoBorrowMutability::Mut { .. } => Self::ReborrowMut,
        }
    }

    fn display(self) -> &'static str {
        match self {
            Self::Deref => "*",
            Self::Reborrow => "&*",
            Self::ReborrowMut => "&mut *",
        }
    }
}

/// Checks if an `iter` or `iter_mut` call returns `IntoIterator::IntoIter`. Returns how the
/// argument needs to be adjusted.
fn is_ref_iterable<'tcx>(
    cx: &LateContext<'tcx>,
    self_arg: &Expr<'_>,
    call_expr: &Expr<'_>,
) -> Option<(AdjustKind, Ty<'tcx>)> {
    let typeck = cx.typeck_results();
    if let Some(trait_id) = cx.tcx.get_diagnostic_item(sym::IntoIterator)
        && let Some(fn_id) = typeck.type_dependent_def_id(call_expr.hir_id)
        && let sig = cx.tcx.liberate_late_bound_regions(fn_id, cx.tcx.fn_sig(fn_id).skip_binder())
        && let &[req_self_ty, req_res_ty] = &**sig.inputs_and_output
        && let param_env = cx.tcx.param_env(fn_id)
        && implements_trait_with_env(cx.tcx, param_env, req_self_ty, trait_id, &[])
        && let Some(into_iter_ty) =
            make_normalized_projection_with_regions(cx.tcx, param_env, trait_id, sym!(IntoIter), [req_self_ty])
        && let req_res_ty = normalize_with_regions(cx.tcx, param_env, req_res_ty)
        && into_iter_ty == req_res_ty
    {
        let adjustments = typeck.expr_adjustments(self_arg);
        let self_ty = typeck.expr_ty(self_arg);

        let res_ty = cx.tcx.erase_regions(EarlyBinder::bind(req_res_ty)
            .instantiate(cx.tcx, typeck.node_args(call_expr.hir_id)));
        let mutbl = if let ty::Ref(_, _, mutbl) = *req_self_ty.kind() {
            Some(mutbl)
        } else {
            None
        };

        if !adjustments.is_empty() && let ty::Ref(region, ty, Mutability::Mut) = *self_ty.kind()
                && let Some(mutbl) = mutbl
            {
            // Attempt to reborrow the mutable reference
            let self_ty = if mutbl.is_mut() {
                self_ty
            } else {
                Ty::new_ref(cx.tcx,region, TypeAndMut { ty, mutbl })
            };
            if implements_trait(cx, self_ty, trait_id, &[])
                && let Some(ty) =
                    make_normalized_projection(cx.tcx, cx.param_env, trait_id, sym!(IntoIter), [self_ty])
                && ty == res_ty
            {
                return Some((AdjustKind::reborrow(mutbl), self_ty));
            }

        }

        match *adjustments {
            [
                Adjustment { kind: Adjust::Deref(_), ..},
                Adjustment {
                    kind: Adjust::Borrow(AutoBorrow::Ref(_, mutbl)),
                    target,
                },
                ..
            ] => {
                if target != self_ty
                    && implements_trait(cx, target, trait_id, &[])
                    && let Some(ty) =
                        make_normalized_projection(cx.tcx, cx.param_env, trait_id, sym!(IntoIter), [target])
                    && ty == res_ty
                {
                    Some((AdjustKind::auto_reborrow(mutbl), target))
                } else {
                    None
                }
            }
            [Adjustment { kind: Adjust::Deref(_), target }, ..] => {
                if is_copy(cx, target)
                    && implements_trait(cx, target, trait_id, &[])
                    && let Some(ty) =
                        make_normalized_projection(cx.tcx, cx.param_env, trait_id, sym!(IntoIter), [target])
                    && ty == res_ty
                {
                    Some((AdjustKind::Deref, target))
                } else {
                    None
                }
            }
            _ => None,
        }
    } else {
        None
    }
}
