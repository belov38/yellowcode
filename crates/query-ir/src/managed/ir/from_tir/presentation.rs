use super::*;
use crate::schema::{FieldRole, QFieldId, StdRole};
impl<'c, 'a, 'r, 'w, 't> Builder<'c, 'a, 'r, 'w, 't> {
    pub(super) fn presentation_item(
        &mut self,
        select: &'t tir::Select,
        item: &'t tir::Item,
        col: usize,
    ) -> BuildResult<Option<NativeOwned<'a, ir::Expr>>> {
        let arg = match &item.expr.kind {
            tir::TKind::Func {
                f: query_syntax::Func::Presentation | query_syntax::Func::RefPresentation,
                args,
            } => match args.as_slice() {
                [arg] => Some(arg),
                _ => return Err(BuildProblem::Internal("ПРЕДСТАВЛЕНИЕ takes one argument")),
            },
            _ => None,
        };
        let field = match &item.expr.kind {
            tir::TKind::Field {
                level,
                source,
                col,
                derefs,
            } if arg.is_none() => self.presentation_of_field(*level, *source, *col, derefs)?,
            _ => None,
        };
        if arg.is_none() && field.is_none() {
            return Ok(None);
        }
        if self.query_depth != 1 || !self.single_part {
            return Err(BuildProblem::Later {
                what: "ПРЕДСТАВЛЕНИЕ в подзапросе или в объединении",
                stage: "Q3",
            });
        }
        if select.distinct {
            return Err(BuildProblem::Later {
                what: "ПРЕДСТАВЛЕНИЕ при РАЗЛИЧНЫЕ",
                stage: "Q3",
            });
        }
        let col = u16::try_from(col).map_err(|_| BuildProblem::Internal("too many columns"))?;
        let expr = match (arg, field) {
            (Some(arg), _) => self.expr(arg)?,
            (None, Some(field)) => field,
            (None, None) => return Ok(None),
        };
        self.ctx
            .scratch_push(&mut self.present, self.ctx.scalar(col))?;
        Ok(Some(expr))
    }
    fn presentation_of_field(
        &mut self,
        level: u16,
        source: u16,
        col: tir::Col,
        derefs: &[tir::Deref],
    ) -> BuildResult<Option<NativeOwned<'a, ir::Expr>>> {
        let ctx = self.ctx;
        let Some(src) = self
            .levels
            .len()
            .checked_sub(1 + usize::from(level))
            .and_then(|i| self.levels.get(i))
            .and_then(|ids| ids.get(usize::from(source)))
            .copied()
        else {
            return Ok(None);
        };
        if let Some((last, head)) = derefs.split_last() {
            if last.targets.is_empty()
                || !last
                    .targets
                    .iter()
                    .all(|(t, f)| self.schema.is_presentation_field(*t, *f))
            {
                return Ok(None);
            }
            let mut ty = ctx.copy(&crate::QType::default())?;
            for &(table, _) in &last.targets {
                if let Some(atom) = self.schema.table(table).reference {
                    let mut refs = ctx.vector(1)?;
                    ctx.push(&mut refs, ctx.scalar(atom))?;
                    let reference = refs.finish().map(|refs| crate::QType {
                        refs,
                        ..crate::QType::default()
                    });
                    ty = ctx.merge_type(ty.trusted_borrow(), reference.trusted_borrow())?;
                }
            }
            return Ok(Some(self.field(
                ty.trusted_borrow(),
                level,
                source,
                col,
                head,
            )?));
        }
        let (Some(ir::SrcKind::Table(table)), tir::Col::Field(field)) = (
            self.sources
                .get(src.0 as usize)
                .map(|s| &s.trusted_borrow().kind),
            col,
        ) else {
            return Ok(None);
        };
        let table = *table;
        if !self.schema.is_presentation_field(table, field) {
            return Ok(None);
        }
        let Some(field) = self
            .schema
            .table(table)
            .fields
            .iter()
            .position(|f| f.role == FieldRole::Std(StdRole::Ref))
            .and_then(|i| u16::try_from(i).ok())
            .map(QFieldId)
        else {
            return Ok(None);
        };
        let ty = match self.schema.field(table, field) {
            Some(f) => ctx.copy(&f.ty)?,
            None => ctx.copy(&crate::QType::default())?,
        };
        Ok(Some(
            ctx.expr(
                ctx.scalar((src, field))
                    .map(|(src, field)| ir::ExprKind::Col {
                        src,
                        col: ir::ColSel::Field(field),
                    }),
                ty,
            )?,
        ))
    }
}
