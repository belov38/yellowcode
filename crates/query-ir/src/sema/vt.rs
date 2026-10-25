use super::expr::{Clause, ExprCtx, Hint};
use super::pos::Frame;
use super::scope::{Columns, ExtraCol, FieldScope, Level, SourceCols, VtCheck, periodicity_word};
use super::{Analyzer, R};
use crate::managed as mem;
use crate::params::ParamConst;
use crate::schema::{
    FieldRole, Periodicity, QFieldId, QTableId, StdRole, TableKind, VtKind, VtParamKind,
};
use crate::tir::TExpr;
use crate::types::{DateQ, Other, QType};
use crate::value::QValue;
use crate::{AnalyzeMode, CompileError};
use mdschema::MdKind;
use query_syntax::ast::{self, ExprKind, InRhs, Lit};
use query_syntax::{Kw, MsgArg, MsgId, QueryError};

pub(crate) struct VtSource<'scope> {
    pub periodicity: Periodicity,
    pub complement: Option<crate::schema::Complement>,
    pub args: mem::NativeOwned<'scope, Vec<Option<TExpr>>>,
    pub breakdown: mem::NativeOwned<'scope, Vec<QFieldId>>,
    pub check: Option<VtCheck>,
}

fn arg_exprs_flagged<'a>(
    memory: Option<&mem::CompileArena>,
    arg: &'a ast::VtArg,
) -> R<mem::ScratchVec<(&'a ast::Expr, bool)>> {
    fn take<'a>(
        memory: Option<&mem::CompileArena>,
        out: &mut mem::ScratchVec<(&'a ast::Expr, bool)>,
        list: &'a ast::DcsList,
    ) -> R<()> {
        out.extend(
            memory,
            list.items.iter().filter(|i| i.star.is_none()).map(|i| {
                (
                    &i.expr,
                    i.alias.is_some()
                        || matches!(i.expr.kind, ExprKind::Paren(_) | ExprKind::Path(_)),
                )
            }),
        )?;
        for item in &list.items {
            for group in &item.dcs {
                take(memory, out, group)?;
            }
        }
        Ok(())
    }
    let mut out = mem::ScratchVec::new(memory, 0)?;
    match &arg.expr.kind {
        ExprKind::Dcs(list) => take(memory, &mut out, list)?,
        _ => out.push(memory, (&arg.expr, false))?,
    }
    for list in &arg.dcs {
        take(memory, &mut out, list)?;
    }
    Ok(out)
}

fn each_param<'e>(
    memory: Option<&mem::CompileArena>,
    e: &'e ast::Expr,
    f: &mut impl FnMut(&'e ast::Param) -> R<()>,
) -> R<()> {
    let mut stack = mem::ScratchVec::new(memory, 1)?;
    stack.push(memory, e)?;
    while let Some(e) = stack.pop() {
        match &e.kind {
            ExprKind::Param(p) => f(p)?,
            ExprKind::Lit(_)
            | ExprKind::Path(_)
            | ExprKind::TypeOf { .. }
            | ExprKind::Value { .. } => {}
            ExprKind::Chain { first, rest } => {
                stack.push(memory, first)?;
                stack.extend(memory, rest.iter().map(|o| &o.expr))?;
            }
            ExprKind::Unary { expr, .. }
            | ExprKind::Cast { expr, .. }
            | ExprKind::Deref { expr, .. } => stack.push(memory, expr)?,
            ExprKind::Paren(x) => stack.push(memory, x)?,
            ExprKind::Compare { l, r, .. } => {
                stack.push(memory, l)?;
                stack.push(memory, r)?;
            }
            ExprKind::In { l, rhs, .. } => {
                stack.push(memory, l)?;
                if let InRhs::List(xs) = rhs {
                    stack.extend(memory, xs)?;
                }
            }
            ExprKind::Between { l, lo, hi, .. } => {
                stack.push(memory, l)?;
                stack.push(memory, lo)?;
                stack.push(memory, hi)?;
            }
            ExprKind::IsNull { l, .. } | ExprKind::Refs { l, .. } => stack.push(memory, l)?,
            ExprKind::Like { l, pattern, .. } => {
                stack.push(memory, l)?;
                stack.push(memory, pattern)?;
            }
            ExprKind::Tuple(xs) | ExprKind::DateTime { args: xs, .. } => {
                stack.extend(memory, xs)?
            }
            ExprKind::Case {
                operand,
                whens,
                else_,
                ..
            } => {
                stack.extend(memory, operand.as_deref())?;
                for w in whens {
                    stack.push(memory, &w.cond)?;
                    stack.push(memory, &w.then)?;
                }
                stack.extend(memory, else_.as_deref())?;
            }
            ExprKind::Func { args, .. } => {
                if let ast::FuncArgs::List(xs) = args {
                    stack.extend(memory, xs)?;
                }
            }
            ExprKind::Dcs(list) => stack.extend(memory, list.items.iter().map(|i| &i.expr))?,
        }
    }
    Ok(())
}

fn period_type_ok(t: &QType) -> bool {
    if t.atom_count() == 0 {
        return !t.null;
    }
    !t.boolean
        && t.number.is_none()
        && t.string.is_none()
        && t.refs.is_empty()
        && t.others
            .iter()
            .all(|o| matches!(o, Other::PointInTime | Other::Boundary))
}

impl<'scope> Level<'scope> {
    fn of_vt_condition(
        memory: Option<&'scope crate::managed::CompileArena>,
        id: QTableId,
        scope: FieldScope,
    ) -> R<Level<'scope>> {
        let mut l = Level::of_table(memory, id)?;
        if let Some(SourceCols::Table { scope: s, .. }) = l.sources.first_mut().map(|s| &mut s.cols)
        {
            *s = scope;
        }
        Ok(l)
    }
}

impl<'scope> Analyzer<'scope> {
    pub(crate) fn vt_source(
        &mut self,
        id: QTableId,
        path: &ast::Path,
        args: Option<&ast::VtArgs>,
    ) -> R<VtSource<'scope>> {
        let mut params = mem::ScratchVec::new(self.memory, self.schema.table(id).params.len())?;
        params.extend(
            self.memory,
            self.schema
                .table(id)
                .params
                .iter()
                .map(|p| (p.kind, p.name_en == "View points")),
        )?;
        let mut typed_args = mem::NativeVec::new(self.memory, args.map_or(0, |a| a.args.len()))?;
        let mut periodicity = Periodicity::Absent;
        let mut complement = None;
        let mut breakdown = mem::NativeVec::new(self.memory, 0)?.finish();
        if let Some(a) = args {
            if a.args.len() > params.len() {
                return Err(self.quoted_path_error(
                    MsgId::SELECT_ERROR_WRONGPARAMETER,
                    path,
                    path.span().start,
                ));
            }
            for (i, arg) in a.args.iter().enumerate() {
                let typed = if let Some(arg) = arg {
                    match params.get(i) {
                        Some((VtParamKind::Period, _)) => self.vt_period(path, i, arg)?,
                        Some((VtParamKind::Condition, _)) => {
                            let scope = self.condition_scope(id);
                            self.vt_condition(id, arg, scope)?
                        }
                        Some((VtParamKind::AccountCondition, _)) => {
                            self.vt_account_condition(id, i, arg)?
                        }
                        Some((VtParamKind::Periodicity, _)) => {
                            if matches!(arg.expr.kind, ExprKind::Param(_)) {
                                return Err(self.periodicity_param_error(
                                    path,
                                    i,
                                    arg.expr.span.start,
                                ));
                            }
                            periodicity = periodicity_of(arg);
                            self.vt_params(arg)?;
                            mem::NativeOwned::none(self.memory)
                        }
                        Some((VtParamKind::ComplementMethod, _)) => {
                            complement = complement_of(arg);
                            self.vt_params(arg)?;
                            mem::NativeOwned::none(self.memory)
                        }
                        Some((VtParamKind::DimensionList, true)) => {
                            breakdown = self.vt_breakdown(id, arg)?;
                            mem::NativeOwned::none(self.memory)
                        }
                        _ => {
                            self.vt_params(arg)?;
                            mem::NativeOwned::none(self.memory)
                        }
                    }
                } else {
                    mem::NativeOwned::none(self.memory)
                };
                typed_args.push_owned(typed)?;
            }
        }
        if let Some(a) = args {
            self.check_period_order(id, path, a)?;
        }
        let mut out = VtSource {
            periodicity,
            complement,
            args: typed_args.finish(),
            breakdown,
            check: None,
        };
        if self.needs_dimension_or_resource(id) {
            out.check = Some(VtCheck {
                at: path.span().start,
                path: {
                    let mut text = mem::NameKey::raw(self.memory, "")?;
                    for (index, part) in path.parts.iter().enumerate() {
                        if index != 0 {
                            text.push_str(self.memory, ".")?;
                        }
                        text.push_str(self.memory, &part.text)?;
                    }
                    text
                },
            });
        }
        Ok(out)
    }

    fn condition_scope(&self, id: QTableId) -> FieldScope {
        let aggregated = matches!(
            self.schema.table(id).kind,
            TableKind::Virtual(
                VtKind::Balance
                    | VtKind::Turnovers
                    | VtKind::BalanceAndTurnovers
                    | VtKind::DrCrTurnovers
            )
        );
        let register = matches!(
            self.schema.table(self.schema.root(id)).kind,
            TableKind::Object(MdKind::AccumulationRegister | MdKind::AccountingRegister)
        );
        if aggregated && register {
            FieldScope::Condition
        } else {
            FieldScope::All
        }
    }

    fn needs_dimension_or_resource(&self, id: QTableId) -> bool {
        if self.schema.table(id).kind != TableKind::Virtual(VtKind::Balance) {
            return false;
        }
        matches!(
            self.schema.table(self.schema.root(id)).kind,
            TableKind::Object(MdKind::AccumulationRegister | MdKind::AccountingRegister)
        )
    }

    fn in_vt_scope<T>(&mut self, level: Level<'scope>, f: impl FnOnce(&mut Self) -> R<T>) -> R<T> {
        let mut levels = mem::ScratchVec::new(self.memory, 1)?;
        levels.push(self.memory, level)?;
        self.frames.push(self.memory, Frame::Barrier)?;
        let saved_levels = std::mem::replace(&mut self.levels, levels);
        let saved_columns = self.columns.take();
        let r = f(self);
        self.frames.pop();
        self.columns = saved_columns;
        self.levels = saved_levels;
        r
    }

    fn vt_period(
        &mut self,
        path: &ast::Path,
        i: usize,
        arg: &ast::VtArg,
    ) -> R<mem::NativeOwned<'scope, Option<TExpr>>> {
        let mut first = None;
        for &(e, _) in &arg_exprs_flagged(self.memory, arg)? {
            let required = QType::date(DateQ::DateTime);
            let hint = Hint::Required(&required);
            let t = self.in_vt_scope(Level::default(), |a| {
                a.operand_value(e, ExprCtx::new(Clause::VtArg, e.span.start), hint)
            })?;
            if !period_type_ok(&t.ty) {
                return Err(self.view_parameter_error(path, i));
            }
            if first.is_none() {
                first = Some(t);
            }
        }
        match first {
            Some(value) => Ok(value.into_native_owned()?.into_some()),
            None => Ok(mem::NativeOwned::none(self.memory)),
        }
    }

    fn vt_condition(
        &mut self,
        id: QTableId,
        arg: &ast::VtArg,
        scope: FieldScope,
    ) -> R<mem::NativeOwned<'scope, Option<TExpr>>> {
        let mut first = None;
        for &(e, field) in &arg_exprs_flagged(self.memory, arg)? {
            let t = self.in_vt_scope(Level::of_vt_condition(self.memory, id, scope)?, |a| {
                if field {
                    a.expr_value(e, ExprCtx::new(Clause::VtArg, e.span.start))
                } else {
                    a.condition_owned(e, Clause::VtArg, Kw::Where)
                        .map(mem::ExprValue::copied)
                }
            })?;
            if first.is_none() {
                first = Some(t);
            }
        }
        match first {
            Some(value) => Ok(value.into_native_owned()?.into_some()),
            None => Ok(mem::NativeOwned::none(self.memory)),
        }
    }

    fn vt_account_condition(
        &mut self,
        id: QTableId,
        i: usize,
        arg: &ast::VtArg,
    ) -> R<mem::NativeOwned<'scope, Option<TExpr>>> {
        let (role, ru, en) = match self
            .schema
            .table(id)
            .params
            .get(i)
            .map(|p| p.name_en.as_str())
        {
            Some("Balanced account condition") => {
                (StdRole::BalancedAccount, "КорСчет", "BalancedAccount")
            }
            Some("AccountDr condition") => (StdRole::AccountDr, "СчетДт", "AccountDr"),
            Some("Account Cr condition") => (StdRole::AccountCr, "СчетКт", "AccountCr"),
            _ => (StdRole::Account, "Счет", "Account"),
        };
        let ty = self
            .schema
            .table(id)
            .fields
            .iter()
            .find(|f| {
                matches!(
                    f.role,
                    FieldRole::Std(
                        StdRole::Account
                            | StdRole::AccountDr
                            | StdRole::AccountCr
                            | StdRole::BalancedAccount
                    )
                ) && f.role == FieldRole::Std(role)
            })
            .or_else(|| {
                self.schema.table(id).fields.iter().find(|f| {
                    matches!(
                        f.role,
                        FieldRole::Std(
                            StdRole::Account
                                | StdRole::AccountDr
                                | StdRole::AccountCr
                                | StdRole::BalancedAccount
                        )
                    )
                })
            })
            .map(|f| mem::ScopeType::copy(self.memory, &f.ty))
            .transpose()?
            .unwrap_or_else(|| mem::ScopeType::Legacy(QType::undefined()));
        let mut first = None;
        for &(e, field) in &arg_exprs_flagged(self.memory, arg)? {
            let mut cols = Columns::new(self.memory)?;
            let ty = crate::managed::ScopeType::copy(self.memory, &ty)?.with_null();
            cols.push(self.memory, ru, &ty)?;
            cols.push(self.memory, en, &ty)?;
            let t = self.in_vt_scope(Level::of_columns(self.memory, cols)?, |a| {
                if field {
                    a.expr_value(e, ExprCtx::new(Clause::VtArg, e.span.start))
                } else {
                    a.condition_owned(e, Clause::VtArg, Kw::Where)
                        .map(mem::ExprValue::copied)
                }
            })?;
            if first.is_none() {
                first = Some(t);
            }
        }
        match first {
            Some(value) => Ok(value.into_native_owned()?.into_some()),
            None => Ok(mem::NativeOwned::none(self.memory)),
        }
    }

    fn vt_params(&mut self, arg: &ast::VtArg) -> R<()> {
        for &(e, _) in &arg_exprs_flagged(self.memory, arg)? {
            each_param(self.memory, e, &mut |p| {
                if self.opts.mode == AnalyzeMode::Execute
                    && self.params.shape(&p.name.text)?.is_none()
                {
                    return Err(self.quoted_text_error(
                        MsgId::SELECT_ERROR_PARAMETERVALUENOTFOUND,
                        &p.name.text,
                        p.span.start,
                    ));
                }
                self.record_param_use(
                    &p.name.text,
                    mem::ScratchType::new(self.memory, QType::undefined()),
                )
            })?;
        }
        Ok(())
    }

    fn vt_breakdown(
        &mut self,
        id: QTableId,
        arg: &ast::VtArg,
    ) -> R<mem::NativeOwned<'scope, Vec<QFieldId>>> {
        self.vt_params(arg)?;
        if self.opts.mode != AnalyzeMode::Execute {
            return Ok(mem::NativeVec::new(self.memory, 0)?.finish());
        }
        let ExprKind::Param(p) = &arg.expr.kind else {
            return Ok(mem::NativeVec::new(self.memory, 0)?.finish());
        };
        let constant = self.params.constant(&p.name.text)?;
        let values = match constant.as_deref() {
            Some(ParamConst::List(vs)) => vs.as_slice(),
            Some(ParamConst::Value(v)) => std::slice::from_ref(v),
            None => return Ok(mem::NativeVec::new(self.memory, 0)?.finish()),
        };
        let mut out = mem::NativeVec::new(self.memory, 0)?;
        for v in values {
            let QValue::String(s) = v else { continue };
            let mut text = mem::NameKey::raw(self.memory, "")?;
            for ch in char::decode_utf16(s.0.iter().copied()) {
                let mut encoded = [0; 4];
                text.push_str(
                    self.memory,
                    ch.unwrap_or(char::REPLACEMENT_CHARACTER)
                        .encode_utf8(&mut encoded),
                )?;
            }
            let key = mem::NameKey::folded(self.memory, &text)?;
            let Some(f) = self.schema.find_field_folded(id, &key) else {
                continue;
            };
            let breakable = self.schema.field(id, f).is_some_and(|qf| {
                matches!(
                    qf.role,
                    FieldRole::Dimension | FieldRole::Std(StdRole::Recorder)
                )
            });
            if breakable && !out.items().contains(&f) {
                out.push_copy(f)?;
            }
        }
        drop(constant);
        self.note_value_dep(&p.name.text)?;
        Ok(out.finish())
    }

    pub(crate) fn breakdown_columns(
        &self,
        id: QTableId,
        fields: &[QFieldId],
    ) -> R<mem::ScratchVec<ExtraCol>> {
        let mut out = mem::ScratchVec::new(self.memory, 0)?;
        for field in fields {
            if let Some(qf) = self.schema.field(id, *field) {
                out.push(
                    self.memory,
                    ExtraCol::breakdown(self.memory, &qf.name_ru, &qf.name_en, &qf.ty)?,
                )?;
            }
        }
        Ok(out)
    }

    fn periodicity_param_error(&self, path: &ast::Path, index: usize, offset: u32) -> CompileError {
        if let Some(memory) = self.memory {
            let mut digits = [0; usize::BITS as usize];
            let Some(number) = decimal_digits(index, &mut digits) else {
                return memory.resource_error(memory_budget::BudgetError::Overflow);
            };
            let parts = path
                .parts
                .iter()
                .enumerate()
                .flat_map(|(k, part)| [if k == 0 { "" } else { "." }, &*part.text])
                .chain([", ", number]);
            return memory.quoted_parts_error(
                MsgId::SELECT_ERROR_WRONGPARAMETER,
                self.map.at(self.text, offset),
                parts,
                "",
            );
        }
        self.err(MsgId::SELECT_ERROR_WRONGPARAMETER, offset)
            .with_quoted(MsgArg::text(&format!("{}, {index}", path.dotted())))
    }

    fn check_period_order(&mut self, id: QTableId, path: &ast::Path, a: &ast::VtArgs) -> R<()> {
        if self.opts.mode != AnalyzeMode::Execute
            || self.schema.table(id).kind != TableKind::Virtual(VtKind::BalanceAndTurnovers)
        {
            return Ok(());
        }
        let mut dates = [None, None];
        for (slot, arg) in dates.iter_mut().zip(a.args.iter()) {
            let Some(arg) = arg else { return Ok(()) };
            let ExprKind::Param(p) = &arg.expr.kind else {
                return Ok(());
            };
            let constant = self.params.constant(&p.name.text)?;
            let Some(ParamConst::Value(v)) = constant.as_deref() else {
                return Ok(());
            };
            *slot = point_date(v);
        }
        let [Some(start), Some(end)] = dates else {
            return Ok(());
        };
        if start.ticks() <= end.ticks() {
            return Ok(());
        }
        for arg in a.args.iter().take(2).flatten() {
            if let ExprKind::Param(p) = &arg.expr.kind {
                self.note_value_dep(&p.name.text)?;
            }
        }
        Err(self.view_period_error(path))
    }

    fn view_period_error(&self, path: &ast::Path) -> CompileError {
        if let Some(memory) = self.memory {
            return memory.quoted_parts_cause_error(
                MsgId::SELECT_ERROR_ERRORPROCESSINGVIEW,
                Some(self.map.at(self.text, path.span().start)),
                path.parts.iter().map(|part| &*part.text),
                ".",
                MsgId::INVALID_PERIOD,
                None,
            );
        }
        self.err(MsgId::SELECT_ERROR_ERRORPROCESSINGVIEW, path.span().start)
            .with_quoted(MsgArg::text(&path.dotted()))
            .with_cause(QueryError::new(MsgId::INVALID_PERIOD, None))
    }

    pub(crate) fn view_parameter_error(&self, path: &ast::Path, index: usize) -> CompileError {
        if let Some(memory) = self.memory {
            let Some(number) = index.checked_add(1) else {
                return memory.resource_error(memory_budget::BudgetError::Overflow);
            };
            let mut digits = [0; usize::BITS as usize];
            let Some(number) = decimal_digits(number, &mut digits) else {
                return memory.resource_error(memory_budget::BudgetError::Overflow);
            };
            return memory.quoted_parts_cause_error(
                MsgId::SELECT_ERROR_ERRORPROCESSINGVIEW,
                Some(self.map.at(self.text, path.span().start)),
                path.parts.iter().map(|part| &*part.text),
                ".",
                MsgId::PARAMETER_N_TYPE_MISMATCH,
                Some(number),
            );
        }
        self.err(MsgId::SELECT_ERROR_ERRORPROCESSINGVIEW, path.span().start)
            .with_quoted(MsgArg::text(&path.dotted()))
            .with_cause(
                QueryError::new(MsgId::PARAMETER_N_TYPE_MISMATCH, None)
                    .with_subst(MsgArg::text(&(index + 1).to_string())),
            )
    }

    pub(crate) fn view_dimensions_error(&self, path: &str, offset: u32) -> CompileError {
        if let Some(memory) = self.memory {
            return memory.quoted_parts_cause_error(
                MsgId::SELECT_ERROR_ERRORPROCESSINGVIEW,
                Some(self.map.at(self.text, offset)),
                [path],
                "",
                MsgId::ATLEAST_ONE_DIMENSION_OR_RESOURCE_NEEDED,
                None,
            );
        }
        self.err(MsgId::SELECT_ERROR_ERRORPROCESSINGVIEW, offset)
            .with_quoted(MsgArg::text(path))
            .with_cause(QueryError::new(
                MsgId::ATLEAST_ONE_DIMENSION_OR_RESOURCE_NEEDED,
                None,
            ))
    }

    pub(crate) fn vt_checks(&self, levels: &[Level]) -> R<()> {
        for level in levels {
            for src in &level.sources {
                let Some(check) = &src.vt_check else { continue };
                if !src.dim_res_used {
                    return Err(self.view_dimensions_error(&check.path, check.at));
                }
            }
        }
        Ok(())
    }
}

fn decimal_digits(mut value: usize, buffer: &mut [u8; usize::BITS as usize]) -> Option<&str> {
    let mut start = buffer.len();
    loop {
        start = start.checked_sub(1)?;
        *buffer.get_mut(start)? = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    std::str::from_utf8(buffer.get(start..)?).ok()
}

fn point_date(v: &QValue) -> Option<bsl_prim::BslDate> {
    match v {
        QValue::Date(d) if *d != bsl_prim::BslDate::EMPTY => Some(*d),
        QValue::PointInTime(m) if m.date != bsl_prim::BslDate::EMPTY => Some(m.date),
        QValue::Boundary(b) => point_date(&b.point),
        _ => None,
    }
}

fn complement_of(arg: &ast::VtArg) -> Option<crate::schema::Complement> {
    match &arg.expr.kind {
        ExprKind::Path(p) => match p.parts.as_slice() {
            [w] => crate::schema::Complement::from_word(&w.text),
            _ => None,
        },
        ExprKind::Lit(Lit::String(s)) => crate::schema::Complement::from_word(s),
        _ => None,
    }
}

fn periodicity_of(arg: &ast::VtArg) -> Periodicity {
    match &arg.expr.kind {
        ExprKind::Path(p) => match p.parts.as_slice() {
            [w] => periodicity_word(&w.text),
            _ => Periodicity::Absent,
        },
        ExprKind::Lit(Lit::String(s)) => periodicity_word(s),
        _ => Periodicity::Absent,
    }
}
