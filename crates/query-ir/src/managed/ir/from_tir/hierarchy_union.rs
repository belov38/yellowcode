use super::*;

type SourceHierarchy<'a> = (NativeOwned<'a, ir::Rel>, Option<(u16, u16)>);

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

    pub(super) fn order_union(
        &mut self,
        query: &'t tir::Query,
        first: &'t tir::Select,
        mut root: NativeOwned<'a, ir::Rel>,
        types: &[crate::QType],
    ) -> BuildResult<NativeOwned<'a, ir::Rel>> {
        use query_syntax::ast::OrderDir;
        let ctx = self.ctx;
        ctx.checkpoint()?;
        let hier_count = query
            .order
            .iter()
            .filter(|k| matches!(k.dir, OrderDir::Hierarchy | OrderDir::HierarchyDesc))
            .count();
        if hier_count > 1 {
            return Err(BuildProblem::Later {
                what: "ИЕРАРХИЯ с несколькими ключами или группировкой",
                stage: "Q3",
            });
        }
        let mut order_items = ctx.scratch::<(usize, OrderDir)>(query.order.len())?;
        if !query.order.is_empty() {
            for k in &query.order {
                let col = k.col.ok_or(BuildProblem::Internal(
                    "an order key of a union outside the select list",
                ))?;
                if first
                    .items
                    .get(col)
                    .is_some_and(|item| item.nested.is_some())
                {
                    return Err(BuildProblem::Later {
                        what: "сортировка по вложенной таблице",
                        stage: "Q3",
                    });
                }
                if u16::try_from(col)
                    .is_ok_and(|c| self.present.iter().any(|p| *p.trusted_borrow() == c))
                {
                    return Err(BuildProblem::Later {
                        what: "сортировка по представлению",
                        stage: "Q3",
                    });
                }
                ctx.scratch_push(&mut order_items, (col, k.dir))?;
            }
        } else if query.autoorder {
            for (col, ty) in types.iter().enumerate() {
                if first
                    .items
                    .get(col)
                    .is_some_and(|item| item.nested.is_some())
                    || u16::try_from(col)
                        .is_ok_and(|c| self.present.iter().any(|p| *p.trusted_borrow() == c))
                    || !ir::union_autoorder_col_orderable(ty)
                {
                    continue;
                }
                ctx.scratch_push(&mut order_items, (col, OrderDir::Asc))?;
            }
            if order_items.is_empty() {
                return Ok(root);
            }
        }
        let hier_col = order_items.iter().find_map(|(col, dir)| {
            matches!(dir, OrderDir::Hierarchy | OrderDir::HierarchyDesc).then_some(*col)
        });
        let mut hier_targets = ctx.scratch::<(crate::QTableId, crate::QFieldId)>(0)?;
        if let Some(h_col) = hier_col
            && let Some(ty) = types.get(h_col)
        {
            for &atom in &ty.refs {
                if let Some(table) = self.schema.hierarchical_table(atom)
                    && let Some(parent_field) =
                        self.role_field(table, crate::schema::StdRole::Parent)
                    && !hier_targets.contains(&(table, parent_field))
                {
                    ctx.scratch_push(&mut hier_targets, (table, parent_field))?;
                }
            }
        }
        let mut source_hier = None;
        if let Some(h_col) = hier_col
            && hier_targets.is_empty()
            && query.unions.iter().all(|all| *all)
        {
            let (updated, pair) = self.attach_union_source_hierarchy(root, h_col, types.len())?;
            root = updated;
            source_hier = pair;
        }
        let needs_auto_refs = query.autoorder
            && order_items
                .iter()
                .any(|(col, _)| types.get(*col).is_some_and(|ty| !ty.refs.is_empty()));
        if hier_targets.is_empty() && !needs_auto_refs {
            let mut keys = ctx.vector(order_items.len())?;
            for &(col, dir) in &order_items {
                let desc = matches!(dir, OrderDir::Desc | OrderDir::HierarchyDesc);
                let ty = match types.get(col) {
                    Some(ty) => ctx.copy(ty)?,
                    None => ctx.copy(&crate::QType::default())?,
                };
                let expr = self.out(col, ty)?;
                ctx.parent_depth(std::iter::once(expr.depth()))?;
                ctx.push(&mut keys, expr.map(|expr| ir::SortKey { expr, desc }))?;
            }
            return ctx.sort(root, keys.finish(), source_hier);
        }
        let extra_hier_cols = if source_hier.is_some() { 2usize } else { 0 };
        let union_width = types
            .len()
            .checked_add(extra_hier_cols)
            .ok_or_else(|| ctx.refuse(memory_budget::BudgetError::Overflow))?;
        let union_src = ir::SrcId(
            u32::try_from(self.sources.len())
                .map_err(|_| BuildProblem::Internal("too many sources"))?,
        );
        let mut subq_cols = ctx.vector(union_width)?;
        for ty in types {
            ctx.push(&mut subq_cols, ctx.copy(ty)?)?;
        }
        for _ in 0..extra_hier_cols {
            ctx.push(&mut subq_cols, ctx.copy(&crate::QType::default())?)?;
        }
        let source_entry = subq_cols.finish().map(|columns| ir::SourceS {
            id: union_src,
            all_fields: false,
            kind: ir::SrcKind::Subquery { columns },
        });
        ctx.scratch_push(&mut self.sources, source_entry)?;
        let subq = ctx.subquery(union_src, root)?;
        let mut project_exprs = ctx.vector(union_width)?;
        for i in 0..union_width {
            let col_idx =
                u16::try_from(i).map_err(|_| BuildProblem::Internal("too many columns"))?;
            let ty = match types.get(i) {
                Some(ty) => ctx.copy(ty)?,
                None => ctx.copy(&crate::QType::default())?,
            };
            let expr = ctx.expr(
                ctx.scalar((union_src, col_idx))
                    .map(|(src, col)| ir::ExprKind::Col {
                        src,
                        col: ir::ColSel::Column(col),
                    }),
                ty,
            )?;
            ctx.parent_depth(std::iter::once(expr.depth()))?;
            ctx.push(&mut project_exprs, expr)?;
        }
        let mut hier = source_hier;
        if !hier_targets.is_empty()
            && let Some(h_col) = hier_col
        {
            for &(table, _) in &hier_targets {
                self.touch(table)?;
            }
            let h_col_u16 =
                u16::try_from(h_col).map_err(|_| BuildProblem::Internal("too many columns"))?;
            let base_ty = match types.get(h_col) {
                Some(ty) => ctx.copy(ty)?,
                None => ctx.copy(&crate::QType::default())?,
            };
            let base = ctx.expr(
                ctx.scalar((union_src, h_col_u16))
                    .map(|(src, col)| ir::ExprKind::Col {
                        src,
                        col: ir::ColSel::Column(col),
                    }),
                base_ty,
            )?;
            let name = ctx.string("\u{0}hier")?;
            let mut targets_vec = ctx.vector(hier_targets.len())?;
            for &(table, field) in &hier_targets {
                ctx.push(&mut targets_vec, ctx.scalar((table, field)))?;
            }
            let step = ctx
                .allocated(name.zip(targets_vec.finish()))?
                .map(|(name, targets)| tir::Deref { name, targets });
            let mut steps = ctx.vector(1)?;
            ctx.push(&mut steps, step)?;
            ctx.parent_depth(std::iter::once(base.depth()))?;
            let base = ctx.boxed(base)?;
            let kind = ctx
                .allocated(base.zip(steps.finish()))?
                .map(|(base, steps)| ir::ExprKind::Deref { base, steps });
            let parent_expr = ctx.expr(kind, ctx.copy(&crate::QType::default())?)?;
            let parent_slot = u16::try_from(project_exprs.items().len())
                .map_err(|_| BuildProblem::Internal("too many columns"))?;
            ctx.parent_depth(std::iter::once(parent_expr.depth()))?;
            ctx.push(&mut project_exprs, parent_expr)?;
            hier = Some((h_col_u16, parent_slot));
        }
        let mut sort_keys = ctx.vector(order_items.len())?;
        for &(col, dir) in &order_items {
            ctx.checkpoint()?;
            let desc = matches!(dir, OrderDir::Desc | OrderDir::HierarchyDesc);
            let has_refs = types.get(col).is_some_and(|ty| !ty.refs.is_empty());
            if query.autoorder && has_refs {
                let col_u16 =
                    u16::try_from(col).map_err(|_| BuildProblem::Internal("too many columns"))?;
                let ty = match types.get(col) {
                    Some(ty) => ctx.copy(ty)?,
                    None => ctx.copy(&crate::QType::default())?,
                };
                let base = ctx.expr(
                    ctx.scalar((union_src, col_u16))
                        .map(|(src, col)| ir::ExprKind::Col {
                            src,
                            col: ir::ColSel::Column(col),
                        }),
                    ty,
                )?;
                let mut automatic = self.auto_keys(base.trusted_borrow())?;
                automatic.reverse();
                while let Some(key) = automatic.pop() {
                    let key_ty = ctx.copy(&key.trusted_borrow().ty)?;
                    let slot = project_exprs.items().len();
                    ctx.parent_depth(std::iter::once(key.depth()))?;
                    ctx.push(&mut project_exprs, key)?;
                    let out_expr = self.out(slot, key_ty)?;
                    ctx.parent_depth(std::iter::once(out_expr.depth()))?;
                    ctx.push(
                        &mut sort_keys,
                        out_expr.map(|expr| ir::SortKey { expr, desc }),
                    )?;
                }
            } else {
                let ty = match types.get(col) {
                    Some(ty) => ctx.copy(ty)?,
                    None => ctx.copy(&crate::QType::default())?,
                };
                let out_expr = self.out(col, ty)?;
                ctx.parent_depth(std::iter::once(out_expr.depth()))?;
                ctx.push(
                    &mut sort_keys,
                    out_expr.map(|expr| ir::SortKey { expr, desc }),
                )?;
            }
        }
        let proj = ctx.project(subq, project_exprs.finish())?;
        ctx.sort(proj, sort_keys.finish(), hier)
    }

    fn attach_union_source_hierarchy(
        &self,
        root: NativeOwned<'a, ir::Rel>,
        col: usize,
        visible: usize,
    ) -> BuildResult<SourceHierarchy<'a>> {
        let ctx = self.ctx;
        ctx.checkpoint()?;
        let ir::Rel::Union { parts, .. } = root.trusted_borrow() else {
            return Ok((root, None));
        };
        let mut any_hier = false;
        for part in parts {
            ctx.checkpoint()?;
            let Some((exprs, _)) = projection(part) else {
                return Ok((root, None));
            };
            if exprs.len() != visible {
                return Ok((root, None));
            }
            let Some(base) = exprs.get(col) else {
                return Ok((root, None));
            };
            if let Some(src) = autoorder::first_src(base)
                && self.hierarchy_of(src)?.is_some()
            {
                any_hier = true;
            }
        }
        if !any_hier {
            return Ok((root, None));
        }
        let id_slot =
            u16::try_from(visible).map_err(|_| BuildProblem::Internal("too many columns"))?;
        let parent_slot = visible
            .checked_add(1)
            .and_then(|n| u16::try_from(n).ok())
            .ok_or(BuildProblem::Internal("too many columns"))?;
        let capacity = visible
            .checked_add(2)
            .ok_or_else(|| ctx.refuse(memory_budget::BudgetError::Overflow))?;
        let depth = root.depth();
        let mut replacements = ctx.vector(parts.len())?;
        for part in parts {
            ctx.checkpoint()?;
            let (exprs, top) = projection(part).ok_or(BuildProblem::Internal(
                "a qualified hierarchy union arm without projection",
            ))?;
            let pair = match exprs.get(col).and_then(autoorder::first_src) {
                Some(src) => self.hierarchy_of(src)?,
                None => None,
            };
            let (reference, ancestor) = match pair {
                Some(pair) => pair,
                None => {
                    let null_a = ctx.expr(
                        ctx.scalar(())
                            .map(|()| ir::ExprKind::Lit(crate::QValue::Null)),
                        ctx.copy(&crate::QType::default())?,
                    )?;
                    let null_b = ctx.expr(
                        ctx.scalar(())
                            .map(|()| ir::ExprKind::Lit(crate::QValue::Null)),
                        ctx.copy(&crate::QType::default())?,
                    )?;
                    (null_a, null_b)
                }
            };
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
        Ok((root, Some((id_slot, parent_slot))))
    }
}
