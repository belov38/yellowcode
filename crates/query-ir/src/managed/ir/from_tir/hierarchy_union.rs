use super::*;

fn projection(part: &ir::Rel) -> Option<(&[ir::Expr], bool)> {
    match part {
        ir::Rel::Project { exprs, .. } => Some((exprs, false)),
        ir::Rel::Top { input, .. } => match input.as_ref() {
            ir::Rel::Project { exprs, .. } => Some((exprs, true)),
            _ => None,
        },
        _ => None,
    }
}

impl<'c, 'a, 'r, 'w, 't> Builder<'c, 'a, 'r, 'w, 't> {
    pub(super) fn hierarchy_union(
        &self,
        query: &tir::Query,
        root: NativeOwned<'a, ir::Rel>,
        types: &[crate::QType],
    ) -> BuildResult<(NativeOwned<'a, ir::Rel>, bool)> {
        let ctx = self.ctx;
        ctx.checkpoint()?;
        if self.query_depth != 1 || self.temp_out {
            return Ok((root, false));
        }
        let Some(shape) = ir::hierarchy_union_shape(query, self.schema) else {
            return Ok((root, false));
        };
        let ir::Rel::Union { parts, .. } = root.trusted_borrow() else {
            return Err(BuildProblem::Internal(
                "a hierarchy union without union input",
            ));
        };
        for part in parts {
            ctx.checkpoint()?;
            let Some((exprs, _)) = projection(part) else {
                return Ok((root, false));
            };
            if exprs.len() != shape.visible {
                return Ok((root, false));
            }
            let Some(base) = exprs.get(shape.column) else {
                return Ok((root, false));
            };
            let ir::ExprKind::Col { src, .. } = base.kind else {
                return Ok((root, false));
            };
            let Some(source) = self.sources.get(src.0 as usize) else {
                return Ok((root, false));
            };
            if shape
                .source(self.schema, base, &source.trusted_borrow().kind)
                .is_none()
            {
                return Ok((root, false));
            }
        }
        let (id, parent) = shape
            .slots()
            .ok_or(BuildProblem::Internal("too many columns"))?;
        let capacity = shape
            .visible
            .checked_add(2)
            .ok_or_else(|| ctx.refuse(memory_budget::BudgetError::Overflow))?;
        let depth = root.depth();
        let mut replacements = ctx.vector(parts.len())?;
        for part in parts {
            ctx.checkpoint()?;
            let (exprs, top) = projection(part).ok_or(BuildProblem::Internal(
                "a qualified hierarchy union arm without projection",
            ))?;
            let base = exprs.get(shape.column).ok_or(BuildProblem::Internal(
                "a qualified hierarchy key without source",
            ))?;
            let ir::ExprKind::Col { src, .. } = base.kind else {
                return Err(BuildProblem::Internal(
                    "a qualified hierarchy key without source",
                ));
            };
            let (reference, ancestor) = self.hierarchy_of(src)?.ok_or(BuildProblem::Internal(
                "a qualified hierarchy union source without parent",
            ))?;
            let mut output = ctx.vector(capacity)?;
            for expr in exprs {
                ctx.push(&mut output, self.copy_expr(expr)?)?;
            }
            ctx.push(&mut output, reference)?;
            ctx.push(&mut output, ancestor)?;
            let output = output.finish();
            let input_bound =
                depth
                    .checked_sub(2 + u16::from(top))
                    .ok_or(BuildProblem::Internal(
                        "a hierarchy union without projection depth",
                    ))?;
            let project_depth = ctx.parent_depth([input_bound, output.depth()].into_iter())?;
            if top {
                ctx.parent_depth(std::iter::once(project_depth))?;
            }
            ctx.push(&mut replacements, output)?;
        }
        let replacements = replacements.finish();
        let replacement = ctx.allocated(root.zip(replacements))?;
        let root = ctx
            .allocated(replacement.try_map(|(mut root, replacements)| {
                let ir::Rel::Union { parts, .. } = &mut root else {
                    return Err(memory_budget::BudgetError::Overflow);
                };
                for (part, exprs) in parts.iter_mut().zip(replacements) {
                    let project = match part {
                        ir::Rel::Top { input, .. } => input.as_mut(),
                        other => other,
                    };
                    let ir::Rel::Project { exprs: old, .. } = project else {
                        return Err(memory_budget::BudgetError::Overflow);
                    };
                    *old = exprs;
                }
                Ok(root)
            }))?
            .set_depth(depth);
        let ty = types.get(shape.column).ok_or(BuildProblem::Internal(
            "a hierarchy union key outside visible types",
        ))?;
        let ty = ctx.copy(ty)?;
        let expr = self.out(shape.column, ty)?;
        let mut keys = ctx.vector(1)?;
        ctx.push(
            &mut keys,
            expr.map(|expr| ir::SortKey {
                expr,
                desc: shape.desc,
            }),
        )?;
        Ok((ctx.sort(root, keys.finish(), Some((id, parent)))?, true))
    }
}
