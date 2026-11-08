use super::*;
use crate::managed::NativeVec;

impl<'c, 'a, 'r, 'w, 't> Builder<'c, 'a, 'r, 'w, 't> {
    pub(super) fn union_autoorder_part(
        &mut self,
        part: NativeOwned<'a, ir::Rel>,
        sort: &mut NativeVec<'a, ir::SortKey>,
        reference_desc: bool,
        scalar_desc: Option<bool>,
    ) -> BuildResult<NativeOwned<'a, ir::Rel>> {
        let ir::Rel::Project { exprs, .. } = part.trusted_borrow() else {
            return Err(BuildProblem::Internal(
                "an autoordered union arm without a projection",
            ));
        };
        if exprs.len() != 1 + usize::from(scalar_desc.is_some()) {
            return Err(BuildProblem::Internal(if scalar_desc.is_some() {
                "an autoordered union arm without its visible keys"
            } else {
                "an autoordered union arm without one raw reference"
            }));
        }
        let base = exprs.first().ok_or(BuildProblem::Internal(
            "an autoordered union arm without one raw reference",
        ))?;
        let automatic = self.auto_keys(base)?;
        self.replace_union_autoorder_part(part, sort, automatic, reference_desc, scalar_desc)
    }

    pub(super) fn replace_union_autoorder_part(
        &mut self,
        part: NativeOwned<'a, ir::Rel>,
        sort: &mut NativeVec<'a, ir::SortKey>,
        mut automatic: ScratchVec<NativeOwned<'a, ir::Expr>>,
        reference_desc: bool,
        scalar_desc: Option<bool>,
    ) -> BuildResult<NativeOwned<'a, ir::Rel>> {
        let ctx = self.ctx;
        let ir::Rel::Project { exprs, .. } = part.trusted_borrow() else {
            return Err(BuildProblem::Internal(
                "an autoordered union arm without a projection",
            ));
        };
        let visible = 1 + usize::from(scalar_desc.is_some());
        if exprs.len() != visible {
            return Err(BuildProblem::Internal(if scalar_desc.is_some() {
                "an autoordered union arm without its visible keys"
            } else {
                "an autoordered union arm without one raw reference"
            }));
        }
        let first = sort.items().is_empty();
        if !first
            && sort
                .items()
                .len()
                .checked_sub(usize::from(scalar_desc.is_some()))
                != Some(automatic.len())
        {
            return Err(BuildProblem::Internal(
                "nonuniform automatic union key layout",
            ));
        }
        let capacity = automatic
            .len()
            .checked_add(visible)
            .ok_or_else(|| ctx.refuse(memory_budget::BudgetError::Overflow))?;
        let mut output = ctx.vector(capacity)?;
        for expr in exprs {
            ctx.push(&mut output, self.copy_expr(expr)?)?;
        }
        automatic.reverse();
        while let Some(key) = automatic.pop() {
            if first {
                let ty = ctx.copy(&key.trusted_borrow().ty)?;
                let expr = self.out(output.items().len(), ty)?;
                ctx.parent_depth(std::iter::once(expr.depth()))?;
                ctx.push(
                    sort,
                    expr.map(|expr| ir::SortKey {
                        expr,
                        desc: reference_desc,
                    }),
                )?;
            }
            ctx.push(&mut output, key)?;
        }
        if first && let Some(desc) = scalar_desc {
            let scalar = exprs.get(1).ok_or(BuildProblem::Internal(
                "an autoordered union arm without its visible keys",
            ))?;
            let expr = self.out(1, ctx.copy(&scalar.ty)?)?;
            ctx.parent_depth(std::iter::once(expr.depth()))?;
            ctx.push(sort, expr.map(|expr| ir::SortKey { expr, desc }))?;
        }
        let output = output.finish();
        let input_depth = part.depth().checked_sub(1).ok_or(BuildProblem::Internal(
            "an autoordered union arm without project depth",
        ))?;
        let depth = ctx.parent_depth([input_depth, output.depth()].into_iter())?;
        let replacement = ctx.allocated(part.zip(output))?;
        Ok(ctx
            .allocated(replacement.try_map(|(part, exprs)| match part {
                ir::Rel::Project { input, .. } => Ok(ir::Rel::Project { input, exprs }),
                _ => Err(memory_budget::BudgetError::Overflow),
            }))?
            .set_depth(depth))
    }
}
