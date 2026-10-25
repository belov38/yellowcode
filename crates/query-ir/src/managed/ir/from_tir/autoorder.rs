use super::*;
use crate::schema::{FieldRole, QFieldId, QTableId, StdRole, TableKind};
use crate::types::{QType, RefAtom};
use mdschema::MdKind;

#[derive(Clone, Copy)]
enum AtomKey {
    Field(QFieldId),
    NotField(QFieldId),
    SelfRef,
}
pub(super) struct MainOrder<'a> {
    pub(super) keys: ScratchVec<NativeOwned<'a, ir::Expr>>,
    pub(super) hier: Option<(NativeOwned<'a, ir::Expr>, NativeOwned<'a, ir::Expr>)>,
}
impl<'c, 'a, 'r, 'w, 't> Builder<'c, 'a, 'r, 'w, 't> {
    pub(super) fn copy_expr(&self, e: &ir::Expr) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let keys = self.ctx.scratch(0)?;
        let index = Index::new();
        super::super::rebuild::Rebuild {
            ctx: self.ctx,
            keys: &keys,
            index: &index,
            own: &[],
            anchor: None,
            in_sub: false,
            replace: false,
        }
        .expr(e)
    }
    pub(super) fn role_field(&self, table: QTableId, role: StdRole) -> Option<QFieldId> {
        self.schema
            .table(table)
            .fields
            .iter()
            .position(|f| f.role == FieldRole::Std(role))
            .and_then(|i| u16::try_from(i).ok())
            .map(QFieldId)
    }
    fn atom_keys(&self, atom: RefAtom) -> BuildResult<ScratchVec<AtomKey>> {
        let ctx = self.ctx;
        let info = self.schema.ref_info(atom);
        let need = |role| {
            self.role_field(info.table, role)
                .ok_or(BuildProblem::Later {
                    what: "АВТОУПОРЯДОЧИВАНИЕ ссылки на объект без стандартного поля",
                    stage: "Q3",
                })
        };
        let mut keys = ctx.scratch(0)?;
        match info.kind {
            MdKind::Catalog => {
                if let Some(f) = self.role_field(info.table, StdRole::IsFolder) {
                    ctx.scratch_push(&mut keys, AtomKey::NotField(f))?;
                }
                ctx.scratch_push(&mut keys, AtomKey::Field(need(StdRole::Description)?))?;
            }
            MdKind::Document => {
                ctx.scratch_push(&mut keys, AtomKey::Field(need(StdRole::Date)?))?;
                ctx.scratch_push(&mut keys, AtomKey::SelfRef)?;
            }
            MdKind::Enum => ctx.scratch_push(&mut keys, AtomKey::Field(need(StdRole::Order)?))?,
            _ => {
                return Err(BuildProblem::Later {
                    what: "АВТОУПОРЯДОЧИВАНИЕ ссылки на объект этого вида",
                    stage: "Q3",
                });
            }
        }
        Ok(keys)
    }
    fn deref_field(
        &mut self,
        e: &ir::Expr,
        atom: RefAtom,
        field: QFieldId,
    ) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        use std::fmt::Write;
        let ctx = self.ctx;
        let table = self.schema.ref_info(atom).table;
        self.touch(table)?;
        struct Label {
            bytes: [u8; 64],
            len: usize,
        }
        impl std::fmt::Write for Label {
            fn write_str(&mut self, s: &str) -> std::fmt::Result {
                let end = self.len.checked_add(s.len()).ok_or(std::fmt::Error)?;
                let dst = self.bytes.get_mut(self.len..end).ok_or(std::fmt::Error)?;
                dst.copy_from_slice(s.as_bytes());
                self.len = end;
                Ok(())
            }
        }
        let mut label = Label {
            bytes: [0; 64],
            len: 0,
        };
        write!(&mut label, "\0auto:{}.{}", table.0, field.0)
            .map_err(|_| ctx.refuse(memory_budget::BudgetError::Overflow))?;
        let label = std::str::from_utf8(
            label
                .bytes
                .get(..label.len)
                .ok_or_else(|| ctx.refuse(memory_budget::BudgetError::Overflow))?,
        )
        .map_err(|_| ctx.refuse(memory_budget::BudgetError::Overflow))?;
        let name = ctx.string(label)?;
        let mut targets = ctx.vector(1)?;
        ctx.push(&mut targets, ctx.scalar((table, field)))?;
        let step = ctx
            .allocated(name.zip(targets.finish()))?
            .map(|(name, targets)| tir::Deref { name, targets });
        let mut steps = ctx.vector(1)?;
        ctx.push(&mut steps, step)?;
        let base = self.copy_expr(e)?;
        ctx.parent_depth(std::iter::once(base.depth()))?;
        let base = ctx.boxed(base)?;
        let kind = ctx
            .allocated(base.zip(steps.finish()))?
            .map(|(base, steps)| ir::ExprKind::Deref { base, steps });
        ctx.expr(kind, ctx.copy(&QType::default())?)
    }
    fn atom_key_expr(
        &mut self,
        e: &ir::Expr,
        atom: RefAtom,
        key: AtomKey,
    ) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let ctx = self.ctx;
        match key {
            AtomKey::Field(f) => self.deref_field(e, atom, f),
            AtomKey::NotField(f) => {
                let x = self.deref_field(e, atom, f)?;
                ctx.parent_depth(std::iter::once(x.depth()))?;
                ctx.expr(
                    ctx.boxed(x)?.map(ir::ExprKind::Not),
                    ctx.copy(&QType::boolean())?,
                )
            }
            AtomKey::SelfRef => self.copy_expr(e),
        }
    }
    fn number(&self, value: i64) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let ctx = self.ctx;
        ctx.expr(
            ctx.scalar(value).map(|v| {
                ir::ExprKind::Lit(crate::QValue::Number(bsl_prim::Number::from_scaled_i64(
                    v, 0,
                )))
            }),
            ctx.copy(&QType::default())?,
        )
    }
    fn null(&self) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        self.ctx.expr(
            self.ctx
                .scalar(())
                .map(|()| ir::ExprKind::Lit(crate::QValue::Null)),
            self.ctx.copy(&QType::default())?,
        )
    }
    fn is_null(&self, x: NativeOwned<'a, ir::Expr>) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let ctx = self.ctx;
        ctx.parent_depth(std::iter::once(x.depth()))?;
        ctx.expr(
            ctx.boxed(x)?
                .map(|l| ir::ExprKind::IsNull { l, not: false }),
            ctx.copy(&QType::boolean())?,
        )
    }
    fn refs_of(&mut self, e: &ir::Expr, atom: RefAtom) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let ctx = self.ctx;
        let table = self.schema.ref_info(atom).table;
        self.touch(table)?;
        let x = self.copy_expr(e)?;
        ctx.parent_depth(std::iter::once(x.depth()))?;
        ctx.expr(
            ctx.boxed(x)?.map(|l| ir::ExprKind::Refs { l, table }),
            ctx.copy(&QType::boolean())?,
        )
    }
    fn case(
        &self,
        whens: NativeOwned<'a, Vec<(ir::Expr, ir::Expr)>>,
        else_: NativeOwned<'a, ir::Expr>,
    ) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let ctx = self.ctx;
        ctx.parent_depth([whens.depth(), else_.depth()].into_iter())?;
        let else_ = ctx.boxed(else_)?;
        let kind = ctx
            .allocated(whens.zip(else_))?
            .map(|(whens, else_)| ir::ExprKind::Case {
                operand: None,
                whens,
                else_: Some(else_),
            });
        ctx.expr(kind, ctx.copy(&QType::default())?)
    }
    pub(super) fn auto_keys(
        &mut self,
        e: &ir::Expr,
    ) -> BuildResult<ScratchVec<NativeOwned<'a, ir::Expr>>> {
        let ctx = self.ctx;
        let atoms = &e.ty.refs;
        let mut out = ctx.scratch(0)?;
        if atoms.is_empty() {
            ctx.scratch_push(&mut out, self.copy_expr(e)?)?;
            return Ok(out);
        }
        let mixed = atoms.len() > 1
            || e.ty.boolean
            || e.ty.number.is_some()
            || e.ty.string.is_some()
            || e.ty.date.is_some()
            || !e.ty.others.is_empty();
        let mut per_atom = ctx.scratch(atoms.len())?;
        for atom in atoms {
            ctx.scratch_push(&mut per_atom, self.atom_keys(*atom)?)?;
        }
        let width = per_atom.iter().map(|keys| keys.len()).max().unwrap_or(0);
        if mixed {
            let mut whens = ctx.vector(
                atoms
                    .len()
                    .checked_add(1)
                    .ok_or_else(|| ctx.refuse(memory_budget::BudgetError::Overflow))?,
            )?;
            for &atom in atoms {
                let table = self.schema.ref_info(atom).table;
                let field = self
                    .role_field(table, StdRole::Ref)
                    .ok_or(BuildProblem::Internal(
                        "a reference table without a reference field",
                    ))?;
                let d = self.deref_field(e, atom, field)?;
                let mut and = ctx.vector(2)?;
                let reference = self.refs_of(e, atom)?;
                ctx.parent_depth(std::iter::once(reference.depth()))?;
                ctx.push(&mut and, reference)?;
                let isnull = self.is_null(d)?;
                ctx.parent_depth(std::iter::once(isnull.depth()))?;
                ctx.push(&mut and, isnull)?;
                let condition = ctx.expr(
                    and.finish().map(ir::ExprKind::And),
                    ctx.copy(&QType::boolean())?,
                )?;
                let result = self.number(1)?;
                ctx.parent_depth([condition.depth(), result.depth()].into_iter())?;
                ctx.push(&mut whens, ctx.allocated(condition.zip(result))?)?;
            }
            let condition = self.is_null(self.copy_expr(e)?)?;
            let result = self.number(0)?;
            ctx.parent_depth([condition.depth(), result.depth()].into_iter())?;
            ctx.push(&mut whens, ctx.allocated(condition.zip(result))?)?;
            ctx.scratch_push(&mut out, self.case(whens.finish(), self.number(2)?)?)?;
        }
        for j in 0..width {
            let mut whens = ctx.scratch(0)?;
            for (&atom, keys) in atoms.iter().zip(&per_atom) {
                if let Some(&key) = keys.get(j) {
                    let result = self.atom_key_expr(e, atom, key)?;
                    let condition = self.refs_of(e, atom)?;
                    ctx.parent_depth([condition.depth(), result.depth()].into_iter())?;
                    ctx.scratch_push(&mut whens, ctx.allocated(condition.zip(result))?)?;
                }
            }
            if !mixed && atoms.len() == 1 {
                if let Some(pair) = whens.pop() {
                    ctx.scratch_push(&mut out, self.copy_expr(&pair.trusted_borrow().1)?)?;
                }
                continue;
            }
            let else_ = if mixed && j == 0 {
                self.copy_expr(e)?
            } else {
                self.null()?
            };
            let whens = ctx.finish(&mut whens)?;
            ctx.scratch_push(&mut out, self.case(whens, else_)?)?;
        }
        Ok(out)
    }
    fn col_of(
        &self,
        src: ir::SrcId,
        table: QTableId,
        field: QFieldId,
    ) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let ctx = self.ctx;
        let ty = match self.schema.field(table, field) {
            Some(f) => ctx.copy(&f.ty)?,
            None => ctx.copy(&QType::default())?,
        };
        ctx.expr(
            ctx.scalar((src, field))
                .map(|(src, field)| ir::ExprKind::Col {
                    src,
                    col: ir::ColSel::Field(field),
                }),
            ty,
        )
    }
    pub(super) fn hierarchy_of(
        &self,
        src: ir::SrcId,
    ) -> BuildResult<Option<(NativeOwned<'a, ir::Expr>, NativeOwned<'a, ir::Expr>)>> {
        let Some(ir::SrcKind::Table(table)) = self
            .sources
            .get(src.0 as usize)
            .map(|s| &s.trusted_borrow().kind)
        else {
            return Ok(None);
        };
        let (Some(id), Some(parent)) = (
            self.role_field(*table, StdRole::Ref),
            self.role_field(*table, StdRole::Parent),
        ) else {
            return Ok(None);
        };
        Ok(Some((
            self.col_of(src, *table, id)?,
            self.col_of(src, *table, parent)?,
        )))
    }
    pub(super) fn main_order(&mut self, src: ir::SrcId) -> BuildResult<Option<MainOrder<'a>>> {
        let ctx = self.ctx;
        let Some(ir::SrcKind::Table(table)) = self
            .sources
            .get(src.0 as usize)
            .map(|s| &s.trusted_borrow().kind)
        else {
            return Ok(None);
        };
        let table = *table;
        let kind = match self.schema.table(table).kind {
            TableKind::Object(k) => k,
            _ => {
                return Err(BuildProblem::Later {
                    what: "АВТОУПОРЯДОЧИВАНИЕ таблицы этого вида",
                    stage: "Q3",
                });
            }
        };
        let mut keys = ctx.scratch(0)?;
        let mut role = |r| -> BuildResult<()> {
            if let Some(field) = self.role_field(table, r) {
                ctx.scratch_push(&mut keys, self.col_of(src, table, field)?)?;
            }
            Ok(())
        };
        match kind {
            MdKind::Catalog | MdKind::Document | MdKind::Enum => role(StdRole::Ref)?,
            MdKind::AccumulationRegister => {
                role(StdRole::Period)?;
                role(StdRole::Recorder)?;
                role(StdRole::LineNumber)?;
            }
            MdKind::InformationRegister => {
                role(StdRole::Period)?;
                for (i, field) in self.schema.table(table).fields.iter().enumerate() {
                    if field.role == FieldRole::Dimension
                        && let Ok(i) = u16::try_from(i)
                    {
                        ctx.scratch_push(&mut keys, self.col_of(src, table, QFieldId(i))?)?;
                    }
                }
            }
            _ => {
                return Err(BuildProblem::Later {
                    what: "АВТОУПОРЯДОЧИВАНИЕ таблицы этого вида",
                    stage: "Q3",
                });
            }
        }
        let hier = if kind == MdKind::Catalog {
            self.hierarchy_of(src)?
        } else {
            None
        };
        Ok(Some(MainOrder { keys, hier }))
    }
}
pub(super) fn first_src(e: &ir::Expr) -> Option<ir::SrcId> {
    match &e.kind {
        ir::ExprKind::Col { src, .. } => Some(*src),
        ir::ExprKind::Cast { x, .. } => first_src(x),
        _ => None,
    }
}
pub(super) fn last_deref_targets(e: &ir::Expr) -> &[(QTableId, QFieldId)] {
    match &e.kind {
        ir::ExprKind::Deref { steps, .. } => steps.last().map_or(&[], |step| &step.targets),
        ir::ExprKind::Cast { x, .. } => last_deref_targets(x),
        _ => &[],
    }
}
