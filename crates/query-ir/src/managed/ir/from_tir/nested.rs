use super::*;
use crate::schema::{FieldRole, QFieldId, QTableId, StdRole};

impl<'c, 'a, 'r, 'w, 't> Builder<'c, 'a, 'r, 'w, 't> {
    pub(super) fn nested_item(
        &mut self,
        item: &'t tir::Item,
        col: usize,
    ) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let ctx = self.ctx;
        let col = u16::try_from(col).map_err(|_| BuildProblem::Internal("too many columns"))?;
        if matches!(item.expr.kind, tir::TKind::Const(_)) {
            ctx.scratch_push(
                &mut self.nested,
                ctx.scalar(()).map(|()| ir::NestedS { col, rows: None }),
            )?;
            return ctx.expr(
                ctx.scalar(())
                    .map(|()| ir::ExprKind::Lit(crate::QValue::Null)),
                ctx.copy(&item.expr.ty)?,
            );
        }
        if item.unknown {
            return Err(BuildProblem::Internal(
                "a nested table of an unknown type in Execute",
            ));
        }
        let (owner_ref, tab) = self.owner_of(&item.expr)?;
        let src = self.section_source(tab)?;
        let owner = self.section_col(src, tab, StdRole::Ref)?;
        let number = self.section_col(src, tab, StdRole::LineNumber)?;
        let mut outer = ctx.scratch(0)?;
        for level in &self.levels {
            for &id in level {
                ctx.scratch_push(&mut outer, id)?;
            }
        }
        let outer_aggs = std::mem::replace(&mut self.aggs, ctx.scratch(0)?);
        let outer_index = std::mem::take(&mut self.agg_index);
        let mut ids = ctx.scratch(1)?;
        ctx.scratch_push(&mut ids, src)?;
        ctx.scratch_push(&mut self.levels, ids)?;
        let result = (|| {
            let items = item.nested.as_deref().unwrap_or_default();
            let mut output = ctx.vector(items.len())?;
            for item in items {
                if item.nested.is_some() {
                    return Err(BuildProblem::Later {
                        what: "вложенная таблица во вложенной",
                        stage: "Q3",
                    });
                }
                let e = self.expr(&item.expr)?;
                ctx.parent_depth(std::iter::once(e.depth()))?;
                ctx.push(&mut output, e)?;
            }
            Ok(output.finish())
        })();
        self.levels.pop();
        let had_aggs = !self.aggs.is_empty();
        self.aggs = outer_aggs;
        self.agg_index = outer_index;
        let exprs = result?;
        if had_aggs {
            return Err(BuildProblem::Later {
                what: "агрегат во вложенной таблице",
                stage: "Q3",
            });
        }
        let mut reads_owner = false;
        for e in exprs.trusted_borrow() {
            ir::visit_expr_in_place(e, true, &mut |x| {
                if let ir::ExprKind::Col { src, .. } = &x.kind {
                    reads_owner |= outer.contains(src);
                }
            });
        }
        if reads_owner {
            return Err(BuildProblem::Later {
                what: "поле владельца во вложенной таблице",
                stage: "Q3",
            });
        }
        let depth = ctx.parent_depth(std::iter::empty())?;
        let scan = ctx
            .scalar((src, tab))
            .map(|(src, table)| ir::Rel::Scan { src, table })
            .set_depth(depth);
        let parts = ctx.allocated(scan.zip(owner))?;
        let parts = ctx.allocated(parts.zip(number))?;
        let rows = ctx
            .allocated(parts.zip(exprs))?
            .map(|(((scan, owner), number), exprs)| ir::NestedRows {
                scan,
                owner,
                number,
                exprs,
            });
        ctx.parent_depth(std::iter::once(rows.depth()))?;
        ctx.scratch_push(
            &mut self.nested,
            rows.map(|rows| ir::NestedS {
                col,
                rows: Some(rows),
            }),
        )?;
        Ok(owner_ref)
    }
    fn owner_of(
        &mut self,
        e: &'t tir::TExpr,
    ) -> BuildResult<(NativeOwned<'a, ir::Expr>, QTableId)> {
        let ctx = self.ctx;
        match &e.kind {
            tir::TKind::Field {
                level,
                source,
                col,
                derefs,
            } => {
                let src = self
                    .levels
                    .len()
                    .checked_sub(1 + usize::from(*level))
                    .and_then(|i| self.levels.get(i))
                    .and_then(|ids| ids.get(usize::from(*source)))
                    .copied()
                    .ok_or(BuildProblem::Internal(
                        "a nested table of a source that is not visible",
                    ))?;
                let col = match col {
                    tir::Col::Field(f) => ir::ColSel::Field(*f),
                    tir::Col::Column(i) => ir::ColSel::Column(*i),
                };
                let base = ctx.expr(
                    ctx.scalar((src, col))
                        .map(|(src, col)| ir::ExprKind::Col { src, col }),
                    self.column_type(src, col)?,
                )?;
                match derefs.split_last() {
                    None => {
                        let (ir::ColSel::Field(field), Some(ir::SrcKind::Table(table))) = (
                            col,
                            self.sources
                                .get(src.0 as usize)
                                .map(|s| &s.trusted_borrow().kind),
                        ) else {
                            return Err(BuildProblem::Later {
                                what: "вложенная таблица не из таблицы",
                                stage: "Q3",
                            });
                        };
                        let table = *table;
                        let tab = self
                            .schema
                            .field(table, field)
                            .and_then(|f| f.nested)
                            .ok_or(BuildProblem::Internal(
                                "a nested table of a field that is not a section",
                            ))?;
                        Ok((self.section_col(src, table, StdRole::Ref)?, tab))
                    }
                    Some((last, init)) => {
                        let tab = self.step_section(last)?;
                        Ok((self.owner_deref(base, init)?, tab))
                    }
                }
            }
            tir::TKind::Cast {
                e: x,
                to,
                derefs,
                num,
            } => {
                let (last, init) = derefs.split_last().ok_or(BuildProblem::Internal(
                    "a nested table of a cast without a dereference",
                ))?;
                let tab = self.step_section(last)?;
                for step in init {
                    for &(table, _) in &step.targets {
                        self.touch(table)?;
                    }
                }
                let x = self.expr(x)?;
                ctx.parent_depth(std::iter::once(x.depth()))?;
                let x = ctx.boxed(x)?;
                let kind = ctx
                    .allocated(x.zip(ctx.copy(to)?))?
                    .map(|(x, to)| ir::ExprKind::Cast { x, to, num: *num });
                let cast = ctx.expr(kind, ctx.copy(to)?)?;
                Ok((self.owner_deref(cast, init)?, tab))
            }
            _ => Err(BuildProblem::Later {
                what: "вложенная таблица из выражения",
                stage: "Q3",
            }),
        }
    }
    fn owner_deref(
        &mut self,
        base: NativeOwned<'a, ir::Expr>,
        steps: &[tir::Deref],
    ) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        if steps.is_empty() {
            return Ok(base);
        }
        let ctx = self.ctx;
        for step in steps {
            for &(table, _) in &step.targets {
                self.touch(table)?;
            }
        }
        ctx.parent_depth(std::iter::once(base.depth()))?;
        let base = ctx.boxed(base)?;
        let mut copied = ctx.vector(steps.len())?;
        for step in steps {
            ctx.push(&mut copied, ctx.copy(step)?)?;
        }
        let kind = ctx
            .allocated(base.zip(copied.finish()))?
            .map(|(base, steps)| ir::ExprKind::Deref { base, steps });
        ctx.expr(kind, ctx.copy(&crate::QType::default())?)
    }
    fn step_section(&self, step: &tir::Deref) -> BuildResult<QTableId> {
        let mut found = None;
        for &(table, field) in &step.targets {
            if let Some(tab) = self.schema.field(table, field).and_then(|f| f.nested) {
                if found.is_some_and(|old| old != tab) {
                    return Err(BuildProblem::Later {
                        what: "вложенная таблица составного ссылочного типа",
                        stage: "Q3",
                    });
                }
                found = Some(tab);
            }
        }
        found.ok_or(BuildProblem::Internal(
            "a nested table step without a section",
        ))
    }
    fn section_source(&mut self, table: QTableId) -> BuildResult<ir::SrcId> {
        let ctx = self.ctx;
        let id = ir::SrcId(
            u32::try_from(self.sources.len())
                .map_err(|_| BuildProblem::Internal("too many sources"))?,
        );
        ctx.scratch_push(
            &mut self.sources,
            ctx.scalar((id, table)).map(|(id, table)| ir::SourceS {
                id,
                kind: ir::SrcKind::Table(table),
            }),
        )?;
        self.touch(table)?;
        Ok(id)
    }
    fn section_col(
        &self,
        src: ir::SrcId,
        table: QTableId,
        role: StdRole,
    ) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let field = self
            .schema
            .table(table)
            .fields
            .iter()
            .position(|f| f.role == FieldRole::Std(role))
            .and_then(|i| u16::try_from(i).ok())
            .map(QFieldId)
            .ok_or(BuildProblem::Internal(
                "a table without a standard field of a nested table",
            ))?;
        let ty = self.schema.field(table, field).map(|f| &f.ty);
        let ty = match ty {
            Some(ty) => self.ctx.copy(ty)?,
            None => self.ctx.copy(&crate::QType::default())?,
        };
        self.ctx.expr(
            self.ctx
                .scalar((src, field))
                .map(|(src, f)| ir::ExprKind::Col {
                    src,
                    col: ir::ColSel::Field(f),
                }),
            ty,
        )
    }
}
