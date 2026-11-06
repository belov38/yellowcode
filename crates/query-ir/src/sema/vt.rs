use super::expr::{Clause, ExprCtx, Hint};
use super::pos::Frame;
use super::scope::{Columns, ExtraCol, FieldScope, Level, SourceCols, VtCheck, periodicity_word};
use super::{Analyzer, R};
use crate::managed as mem;
use crate::params::ParamConst;
use crate::schema::{
    FieldRole, Periodicity, QFieldId, QTableId, StdRole, TableKind, VtKind, VtParamKind,
};
use crate::tir::{
    DrCrArgDependency, DrCrArgRead, DrCrArgScope, DrCrNested, DrCrSource, DrCrWildcard,
    DrCrWildcardTarget, TExpr,
};
use crate::types::{DateQ, Other, QType};
use crate::value::QValue;
use crate::{AnalyzeMode, CompileError};
use mdschema::MdKind;
use query_syntax::ast::{self, ExprKind, InRhs, Lit};
use query_syntax::{Kw, MsgArg, MsgId, QueryError};

pub(crate) struct VtSource<'scope> {
    pub periodicity: Periodicity,
    pub complement: Option<crate::schema::Complement>,
    pub records: Option<crate::tir::RecordsOptions>,
    pub accounting: Option<crate::tir::AccountingArguments>,
    pub accounting_dependencies:
        mem::NativeOwned<'scope, Option<crate::tir::AccountingDependencies>>,
    pub drcr: mem::NativeOwned<'scope, Option<DrCrSource>>,
    pub args: mem::NativeOwned<'scope, Vec<Option<TExpr>>>,
    pub breakdown: mem::NativeOwned<'scope, Vec<QFieldId>>,
    pub check: Option<VtCheck>,
}

struct CapturedDrCrExpr<'ast, 'scope> {
    argument: u16,
    expression: &'ast ast::Expr,
    scope: DrCrArgScope,
    owned: Option<mem::NativeOwned<'scope, TExpr>>,
}

struct DrCrCapture<'ast, 'scope> {
    captured: mem::ScratchVec<CapturedDrCrExpr<'ast, 'scope>>,
    output: mem::NativeVec<'scope, DrCrArgDependency>,
    ordinary: bool,
    complete: bool,
}

trait DrCrSink<'scope> {
    fn put(
        &mut self,
        memory: Option<&'scope mem::CompileArena>,
        value: mem::NativeOwned<'scope, DrCrArgDependency>,
    ) -> R<()>;
}
impl<'scope> DrCrSink<'scope> for mem::NativeVec<'scope, DrCrArgDependency> {
    fn put(
        &mut self,
        _: Option<&'scope mem::CompileArena>,
        value: mem::NativeOwned<'scope, DrCrArgDependency>,
    ) -> R<()> {
        self.push_owned(value)
    }
}
impl<'scope> DrCrSink<'scope> for mem::ScratchVec<mem::NativeOwned<'scope, DrCrArgDependency>> {
    fn put(
        &mut self,
        memory: Option<&'scope mem::CompileArena>,
        value: mem::NativeOwned<'scope, DrCrArgDependency>,
    ) -> R<()> {
        self.push(memory, value)
    }
}

impl<'ast, 'scope> DrCrCapture<'ast, 'scope> {
    fn new(memory: Option<&'scope mem::CompileArena>) -> R<Self> {
        Ok(Self {
            captured: mem::ScratchVec::new(memory, 0)?,
            output: mem::NativeVec::new(memory, 0)?,
            ordinary: false,
            complete: true,
        })
    }
    fn keep(
        &mut self,
        memory: Option<&'scope mem::CompileArena>,
        argument: usize,
        expression: &'ast ast::Expr,
        scope: DrCrArgScope,
        owned: mem::NativeOwned<'scope, TExpr>,
    ) -> R<()> {
        let Ok(argument) = u16::try_from(argument) else {
            return mem::allocation_error(memory, memory_budget::BudgetError::Overflow);
        };
        self.captured.push(
            memory,
            CapturedDrCrExpr {
                argument,
                expression,
                scope,
                owned: Some(owned),
            },
        )
    }
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

fn drcr_inert(e: &ast::Expr) -> bool {
    match &e.kind {
        ExprKind::Lit(_)
        | ExprKind::Path(_)
        | ExprKind::TypeOf { .. }
        | ExprKind::Value { .. }
        | ExprKind::Param(_) => true,
        ExprKind::Paren(e) => drcr_inert(e),
        _ => false,
    }
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
    fn in_drcr_capture_b<T>(&mut self, f: impl FnOnce(&mut Self) -> R<T>) -> R<T> {
        let restricted = self.params.shape_only();
        let saved_params = std::mem::replace(&mut self.params, restricted);
        let saved_defer = std::mem::replace(&mut self.defer_accum_balance_failure, false);
        let result = f(self);
        self.defer_accum_balance_failure = saved_defer;
        self.params = saved_params;
        result
    }

    fn drcr_level(&self, id: QTableId, scope: DrCrArgScope) -> R<Level<'scope>> {
        match scope {
            DrCrArgScope::NoSources => Ok(Level::default()),
            DrCrArgScope::Condition => {
                Level::of_vt_condition(self.memory, id, FieldScope::Condition)
            }
            DrCrArgScope::Nested(table) => Level::of_table(self.memory, table),
            DrCrArgScope::Account(role) => {
                let (ru, en) = match role {
                    StdRole::AccountDr => ("СчетДт", "AccountDr"),
                    StdRole::AccountCr => ("СчетКт", "AccountCr"),
                    StdRole::Account => ("Счет", "Account"),
                    StdRole::BalancedAccount => ("КорСчет", "BalancedAccount"),
                    _ => return Err(self.unsupported("invalid DrCr account scope", 0)),
                };
                let field = self
                    .schema
                    .table(id)
                    .fields
                    .iter()
                    .find(|f| f.role == FieldRole::Std(role))
                    .or_else(|| {
                        matches!(role, StdRole::Account | StdRole::BalancedAccount)
                            .then(|| {
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
                            .flatten()
                    })
                    .ok_or_else(|| self.unsupported("missing DrCr account scope", 0))?;
                let ty = mem::ScopeType::copy(self.memory, &field.ty)?.with_null();
                let mut cols = Columns::new(self.memory)?;
                cols.push(self.memory, ru, &ty)?;
                cols.push(self.memory, en, &ty)?;
                Level::of_columns(self.memory, cols)
            }
        }
    }

    fn drcr_nested_target(&self, e: &TExpr) -> R<Option<QTableId>> {
        if let crate::tir::TKind::Field { derefs, .. } | crate::tir::TKind::Cast { derefs, .. } =
            &e.kind
            && let Some(last) = derefs.last()
        {
            let mut found = None;
            for (table, field) in &last.targets {
                let Some(target) = self.schema.field(*table, *field).and_then(|f| f.nested) else {
                    continue;
                };
                if found.is_some_and(|old| old != target) {
                    return Err(self.unsupported(
                        "a DrCr nested prefix has ambiguous table targets",
                        e.span.start,
                    ));
                }
                found = Some(target);
            }
            return Ok(found);
        }
        Ok(self.nested_table(e))
    }

    fn drcr_capture<'ast>(
        &mut self,
        id: QTableId,
        args: Option<&'ast ast::VtArgs>,
        capture: &mut DrCrCapture<'ast, 'scope>,
    ) -> R<mem::NativeOwned<'scope, DrCrSource>> {
        let mut supplied = mem::NativeVec::new(self.memory, args.map_or(0, |a| a.args.len()))?;
        let mut output =
            std::mem::replace(&mut capture.output, mem::NativeVec::new(self.memory, 0)?);
        if let Some(args) = args {
            for (i, arg) in args.args.iter().enumerate() {
                supplied.push_copy(u8::from(arg.is_some()))?;
                let Some(arg) = arg else { continue };
                let argument = u16::try_from(i).map_err(|_| {
                    self.unsupported("DrCr argument index out of range", arg.expr.span.start)
                })?;
                let scope = match self.schema.table(id).params.get(i) {
                    Some(p) if p.kind == VtParamKind::Condition => DrCrArgScope::Condition,
                    Some(p) if p.kind == VtParamKind::AccountCondition => {
                        account_condition(Some(&p.name_en))
                            .map_or(DrCrArgScope::NoSources, |(role, _, _)| {
                                DrCrArgScope::Account(role)
                            })
                    }
                    _ => DrCrArgScope::NoSources,
                };
                let mut ordinal = 0;
                if let ExprKind::Dcs(list) = &arg.expr.kind {
                    self.drcr_list(
                        id,
                        argument,
                        scope,
                        list,
                        capture,
                        &mut output,
                        &mut ordinal,
                    )?;
                } else {
                    self.drcr_site(
                        id,
                        argument,
                        scope,
                        &arg.expr,
                        None,
                        false,
                        false,
                        &[],
                        capture,
                        &mut output,
                        &mut ordinal,
                    )?;
                }
                for list in &arg.dcs {
                    self.drcr_list(
                        id,
                        argument,
                        scope,
                        list,
                        capture,
                        &mut output,
                        &mut ordinal,
                    )?;
                }
            }
        }
        if capture.captured.iter().any(|x| x.owned.is_some()) {
            return Err(self.unsupported("unconsumed DrCr semantic owner", 0));
        }
        mem::allocation_result(self.memory, supplied.finish().zip(output.finish())).map(|o| {
            o.map(|(supplied, dependencies)| {
                let mut source = DrCrSource {
                    supplied,
                    dependencies,
                    date_slots: 0,
                };
                if let Some(args) = args {
                    for (i, arg) in args.args.iter().take(2).enumerate() {
                        let Some(arg) = arg else { continue };
                        if arg.dir.is_none()
                            && arg.dcs.is_empty()
                            && matches!(
                                &arg.expr.kind,
                                ExprKind::DateTime { .. } | ExprKind::Param(_)
                            )
                            && source.date_argument(i).is_some()
                        {
                            source.date_slots |= 1 << i;
                        }
                    }
                }
                source
            })
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one paid traversal keeps scope and transfer explicit"
    )]
    fn drcr_list<'ast>(
        &mut self,
        id: QTableId,
        argument: u16,
        scope: DrCrArgScope,
        list: &'ast ast::DcsList,
        capture: &mut DrCrCapture<'ast, 'scope>,
        output: &mut impl DrCrSink<'scope>,
        ordinal: &mut u32,
    ) -> R<()> {
        for item in &list.items {
            self.drcr_site(
                id,
                argument,
                scope,
                &item.expr,
                item.nested.as_ref(),
                item.star.is_some(),
                item.alias.is_some()
                    || matches!(item.expr.kind, ExprKind::Paren(_) | ExprKind::Path(_)),
                &item.dcs,
                capture,
                output,
                ordinal,
            )?;
            if item.nested.is_none() {
                for tail in &item.dcs {
                    self.drcr_list(id, argument, scope, tail, capture, output, ordinal)?;
                }
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one paid producer keeps source scope and owner transfer explicit"
    )]
    fn drcr_site<'ast>(
        &mut self,
        id: QTableId,
        argument: u16,
        scope: DrCrArgScope,
        e: &'ast ast::Expr,
        nested: Option<&'ast ast::DcsList>,
        star: bool,
        field: bool,
        tails: &'ast [ast::DcsList],
        capture: &mut DrCrCapture<'ast, 'scope>,
        output: &mut impl DrCrSink<'scope>,
        ordinal: &mut u32,
    ) -> R<()> {
        if let ExprKind::Dcs(list) = &e.kind {
            return self.drcr_list(id, argument, scope, list, capture, output, ordinal);
        }
        let captured = capture
            .captured
            .iter_mut()
            .find(|x| std::ptr::eq(x.expression, e));
        let base = if let Some(site) = captured {
            if site.argument != argument || site.scope != scope {
                return Err(self.unsupported("inconsistent DrCr capture scope", e.span.start));
            }
            site.owned
                .take()
                .ok_or_else(|| self.unsupported("duplicate DrCr capture site", e.span.start))?
        } else {
            if !star
                && nested.is_none()
                && matches!(scope, DrCrArgScope::NoSources)
                && drcr_inert(e)
            {
                if capture.ordinary {
                    capture.complete = false;
                }
                return Ok(());
            }
            let level = if star && matches!(scope, DrCrArgScope::Condition) {
                Level::of_table(self.memory, id)?
            } else {
                self.drcr_level(id, scope)?
            };
            self.in_vt_scope(level, |a| {
                if matches!(scope, DrCrArgScope::Nested(_)) && !field && !star && nested.is_none() {
                    a.condition_owned(e, Clause::VtArg, Kw::Where)
                        .map(mem::ExprValue::copied)
                } else {
                    a.expr_value(e, ExprCtx::new(Clause::VtArg, e.span.start))
                }
            })?
            .into_native_owned()?
        };
        let current = *ordinal;
        *ordinal = ordinal.checked_add(1).ok_or_else(|| {
            self.unsupported("DrCr dependency ordinal out of range", e.span.start)
        })?;
        let mut attached = mem::ScratchVec::new(self.memory, 0)?;
        let read = if let Some(children) = nested {
            let table = self
                .in_vt_scope(self.drcr_level(id, scope)?, |a| {
                    a.drcr_nested_target(base.trusted_borrow())
                })?
                .ok_or_else(|| {
                    self.unsupported("a nested table list needs a tabular section", e.span.start)
                })?;
            let mut dependencies = mem::NativeVec::new(self.memory, 0)?;
            for tail in tails {
                self.drcr_list(id, argument, scope, tail, capture, &mut attached, ordinal)?;
            }
            self.drcr_list(
                id,
                argument,
                DrCrArgScope::Nested(table),
                children,
                capture,
                &mut dependencies,
                ordinal,
            )?;
            let owner = mem::allocation_result(self.memory, base.zip(dependencies.finish()))?.map(
                |(base, dependencies)| DrCrNested {
                    base,
                    table,
                    dependencies,
                    span: e.span,
                },
            );
            use mem::ManagedClone;
            let depth =
                mem::allocation_result(self.memory, owner.trusted_borrow().graph_depth(128))?;
            self.max_depth = self.max_depth.max(u32::from(depth));
            owner.set_depth(depth).map(DrCrArgRead::Nested)
        } else if star {
            let mut targets = mem::NativeVec::new(self.memory, 0)?;
            let mut stack = mem::ScratchVec::new(self.memory, 0)?;
            let path = mem::NativeVec::new(self.memory, 0)?.finish();
            let section = self.in_vt_scope(self.drcr_level(id, scope)?, |a| {
                a.drcr_nested_target(base.trusted_borrow())
            })?;
            if let Some(table) = section {
                self.drcr_expand(
                    table,
                    path.trusted_borrow(),
                    e.span,
                    &mut stack,
                    &mut targets,
                )?;
            } else {
                let ty = &base.trusted_borrow().ty;
                if ty.refs.is_empty()
                    || ty.boolean
                    || ty.number.is_some()
                    || ty.string.is_some()
                    || ty.date.is_some()
                    || !ty.others.is_empty()
                {
                    return Err(self.unsupported(
                        "a DrCr wildcard prefix has no reference or nested-table target",
                        e.span.start,
                    ));
                }
                for atom in &ty.refs {
                    self.drcr_expand(
                        self.schema.ref_info(*atom).table,
                        path.trusted_borrow(),
                        e.span,
                        &mut stack,
                        &mut targets,
                    )?;
                }
            }
            mem::allocation_result(self.memory, base.zip(targets.finish()))?.map(
                |(base, targets)| {
                    DrCrArgRead::Wildcard(DrCrWildcard {
                        base,
                        targets,
                        span: e.span,
                    })
                },
            )
        } else {
            base.map(DrCrArgRead::Expression)
        };
        output.put(
            self.memory,
            read.map(|read| DrCrArgDependency {
                argument,
                ordinal: current,
                scope,
                read,
            }),
        )?;
        attached.reverse();
        while let Some(owner) = attached.pop() {
            output.put(self.memory, owner)?;
        }
        Ok(())
    }

    fn drcr_expand(
        &self,
        table: QTableId,
        path: &[crate::tir::Deref],
        span: query_syntax::Span,
        stack: &mut mem::ScratchVec<QTableId>,
        out: &mut mem::NativeVec<'scope, DrCrWildcardTarget>,
    ) -> R<()> {
        if stack.contains(&table) {
            return Err(self.unsupported(
                "a DrCr wildcard expansion has cyclic nested targets",
                span.start,
            ));
        }
        stack.push(self.memory, table)?;
        let mut fields = mem::NativeVec::new(self.memory, 0)?;
        let mut owned_path = mem::NativeVec::new(self.memory, path.len())?;
        for step in path {
            owned_path.push_owned(mem::owned_copy(self.memory, step)?)?;
        }
        for field in &self.schema.table(table).star {
            fields.push_copy(*field)?;
        }
        out.push_owned(
            mem::allocation_result(self.memory, owned_path.finish().zip(fields.finish()))?.map(
                |(path, fields)| DrCrWildcardTarget {
                    table,
                    path,
                    fields,
                    all_fields: true,
                },
            ),
        )?;
        for field in &self.schema.table(table).star {
            let Some(qf) = self.schema.field(table, *field) else {
                continue;
            };
            if let Some(child) = qf.nested {
                let mut next = mem::NativeVec::new(self.memory, path.len() + 1)?;
                for step in path {
                    next.push_owned(mem::owned_copy(self.memory, step)?)?;
                }
                let name = mem::owned_copy(self.memory, &qf.name_en)?;
                let mut targets = mem::NativeVec::new(self.memory, 1)?;
                targets.push_copy((table, *field))?;
                next.push_owned(
                    mem::allocation_result(self.memory, name.zip(targets.finish()))?
                        .map(|(name, targets)| crate::tir::Deref { name, targets }),
                )?;
                self.drcr_expand(child, next.finish().trusted_borrow(), span, stack, out)?;
            }
        }
        stack.pop();
        Ok(())
    }

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
        let accounting_periods = if crate::lower::is_accounting_balance(self.schema, id) {
            1
        } else if crate::lower::is_accounting_turnovers(self.schema, id) {
            2
        } else {
            0
        };
        let mut accounting =
            (accounting_periods != 0).then(crate::tir::AccountingArguments::default);
        let mut accounting_capture = if accounting.is_some()
            || matches!(
                self.schema.table(id).kind,
                TableKind::Virtual(VtKind::ScheduleData | VtKind::ActualActionPeriod)
            ) {
            let mut capture = DrCrCapture::new(self.memory)?;
            capture.ordinary = true;
            Some(capture)
        } else {
            None
        };
        let mut records = (self.schema.table(id).kind
            == TableKind::Virtual(VtKind::RecordsWithExtDimensions))
        .then(crate::tir::RecordsOptions::default);
        let mut drcr = if self.schema.table(id).kind == TableKind::Virtual(VtKind::DrCrTurnovers) {
            Some(DrCrCapture::new(self.memory)?)
        } else {
            None
        };
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
                        Some((VtParamKind::Value, _))
                            if matches!(
                                self.schema.table(id).kind,
                                TableKind::Virtual(
                                    VtKind::TaskByPerformer | VtKind::FilterCriterion
                                )
                            ) =>
                        {
                            if self.schema.table(id).kind
                                == TableKind::Virtual(VtKind::FilterCriterion)
                                && self.opts.mode == AnalyzeMode::Execute
                                && let ExprKind::Param(p) = &arg.expr.kind
                                && let Some(shape) = self.params.shape(&p.name.text)?
                                && !matches!(&*shape, crate::ParamShape::Value(_))
                            {
                                return Err(self.view_parameter_error(path, i));
                            }
                            let t = self.in_vt_scope(Level::default(), |a| {
                                a.expr_value(
                                    &arg.expr,
                                    ExprCtx::new(Clause::VtArg, arg.expr.span.start),
                                )
                            })?;
                            if self.schema.table(id).kind
                                == TableKind::Virtual(VtKind::FilterCriterion)
                                && let Some(meta) =
                                    self.schema.meta(id).and_then(|m| m.criterion.as_ref())
                                && t.ty != QType::default()
                                && t.ty != meta.ty
                            {
                                return Err(self.view_parameter_error(path, i));
                            }
                            t.into_native_owned()?.into_some()
                        }
                        Some((VtParamKind::Order, _)) if records.is_some() => {
                            let options = records
                                .as_mut()
                                .ok_or_else(|| self.view_parameter_error(path, i))?;
                            options.order_desc = matches!(arg.dir, Some((ast::OrderDir::Desc, _)));
                            if matches!(
                                arg.expr.kind,
                                ExprKind::Param(_)
                                    | ExprKind::Lit(Lit::String(_))
                                    | ExprKind::Dcs(_)
                                    | ExprKind::Tuple(_)
                            ) || !arg.dcs.is_empty()
                            {
                                options
                                    .deferred
                                    .get_or_insert(crate::tir::RecordsDeferred::OrderShape);
                            }
                            self.vt_scalar(id, arg)?
                        }
                        Some((VtParamKind::Top, _)) if records.is_some() => {
                            let options = records
                                .as_mut()
                                .ok_or_else(|| self.view_parameter_error(path, i))?;
                            let literal = match &arg.expr.kind {
                                ExprKind::Lit(Lit::Number(n))
                                    if self
                                        .text
                                        .get(
                                            arg.expr.span.start as usize
                                                ..arg.expr.span.end as usize,
                                        )
                                        .is_some_and(|token| {
                                            token.bytes().all(|b| b.is_ascii_digit())
                                        })
                                        && arg.dir.is_none()
                                        && arg.dcs.is_empty() =>
                                {
                                    Some(n.digits.parse::<u64>())
                                }
                                _ => None,
                            };
                            match literal {
                                Some(Ok(n)) => {
                                    options.top = Some(n);
                                    mem::NativeOwned::none(self.memory)
                                }
                                other => {
                                    options.deferred.get_or_insert(if other.is_some() {
                                        crate::tir::RecordsDeferred::TopRange
                                    } else {
                                        crate::tir::RecordsDeferred::TopShape
                                    });
                                    self.vt_scalar(id, arg)?
                                }
                            }
                        }
                        Some((VtParamKind::Period, _)) => self.vt_period(
                            path,
                            i,
                            arg,
                            drcr.as_mut().or(accounting_capture.as_mut()),
                        )?,
                        Some((VtParamKind::Condition, _)) => {
                            let scope = self.condition_scope(id);
                            self.vt_condition(
                                id,
                                i,
                                arg,
                                scope,
                                drcr.as_mut().or(accounting_capture.as_mut()),
                            )?
                        }
                        Some((VtParamKind::AccountCondition, _)) => self.vt_account_condition(
                            id,
                            i,
                            arg,
                            drcr.as_mut().or(accounting_capture.as_mut()),
                        )?,
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
                if i < 2
                    && let Some(options) = records.as_mut()
                    && typed.trusted_borrow().as_ref().is_some_and(|e| {
                        !matches!(
                            e.kind,
                            crate::tir::TKind::Const(_) | crate::tir::TKind::Param(_)
                        )
                    })
                {
                    options
                        .deferred
                        .get_or_insert(crate::tir::RecordsDeferred::BoundExpression);
                }
                if i == 3
                    && let Some(options) = records.as_mut()
                    && typed.trusted_borrow().as_ref().is_some_and(|e| {
                        matches!(
                            e.kind,
                            crate::tir::TKind::Param(_)
                                | crate::tir::TKind::Const(QValue::String(_))
                        )
                    })
                {
                    options
                        .deferred
                        .get_or_insert(crate::tir::RecordsDeferred::OrderShape);
                }
                if let (Some(marker), Some(arg)) = (accounting.as_mut(), arg) {
                    if i < accounting_periods
                        && arg.dir.is_none()
                        && arg.dcs.is_empty()
                        && matches!(
                            &arg.expr.kind,
                            ExprKind::DateTime { .. } | ExprKind::Param(_)
                        )
                        && typed
                            .trusted_borrow()
                            .as_ref()
                            .is_some_and(crate::tir::AccountingArguments::plain_date)
                    {
                        marker.date_slots |= 1 << i;
                    } else {
                        marker.deferred = true;
                    }
                }
                typed_args.push_owned(typed)?;
            }
        }
        if let Some(a) = args {
            self.check_period_order(id, path, a)?;
            if records.is_some()
                || matches!(
                    self.schema.table(id).kind,
                    TableKind::Virtual(VtKind::ActualActionPeriod | VtKind::ScheduleData)
                )
            {
                use query_syntax::lexer::{Punct, TokKind, Tokens};
                if let Some(token) = Tokens::new(a.span.text(self.text))
                    .find(|t| t.kind == TokKind::Punct(Punct::LBrace))
                {
                    if let Some(options) = records
                        .as_mut()
                        .filter(|_| self.opts.mode == AnalyzeMode::Static)
                    {
                        options.deferred = Some(crate::tir::RecordsDeferred::Composition);
                    } else {
                        return Err(self.unsupported(
                            "a DCS block in an expression",
                            a.span.start + token.span.start,
                        ));
                    }
                }
            }
        }
        let accounting_dependencies = match accounting_capture {
            None => mem::NativeOwned::none(self.memory),
            Some(mut capture) => {
                let source = self.in_drcr_capture_b(|a| a.drcr_capture(id, args, &mut capture))?;
                let complete = capture.complete;
                source
                    .map(|source| crate::tir::AccountingDependencies {
                        supplied: source.supplied,
                        dependencies: source.dependencies,
                        complete,
                    })
                    .into_some()
            }
        };
        let drcr = match drcr {
            None => mem::NativeOwned::none(self.memory),
            Some(mut capture) => self
                .in_drcr_capture_b(|a| a.drcr_capture(id, args, &mut capture))?
                .into_some(),
        };
        let typed_args = typed_args.finish();
        if self.schema.table(id).kind == TableKind::Virtual(VtKind::FilterCriterion)
            && !typed_args
                .trusted_borrow()
                .first()
                .is_some_and(Option::is_some)
        {
            return Err(self.view_parameter_error(path, 0));
        }
        if self.schema.table(id).kind == TableKind::Virtual(VtKind::Base) {
            self.base_main_dimensions(id, path, args)?;
        }
        let mut out = VtSource {
            periodicity,
            complement,
            args: typed_args,
            records,
            accounting,
            accounting_dependencies,
            drcr,
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

    fn vt_scalar(
        &mut self,
        id: QTableId,
        arg: &ast::VtArg,
    ) -> R<mem::NativeOwned<'scope, Option<TExpr>>> {
        let mut first = None;
        for &(e, _) in &arg_exprs_flagged(self.memory, arg)? {
            let t = self.in_vt_scope(
                Level::of_vt_condition(self.memory, id, FieldScope::All)?,
                |a| a.expr_value(e, ExprCtx::new(Clause::VtArg, e.span.start)),
            )?;
            if first.is_none() {
                first = Some(t);
            }
        }
        match first {
            Some(value) => Ok(value.into_native_owned()?.into_some()),
            None => Ok(mem::NativeOwned::none(self.memory)),
        }
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

    fn vt_period<'ast>(
        &mut self,
        path: &ast::Path,
        i: usize,
        arg: &'ast ast::VtArg,
        mut capture: Option<&mut DrCrCapture<'ast, 'scope>>,
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
            if let Some(capture) = capture.as_mut() {
                if capture.ordinary && first.is_none() {
                    first = Some(mem::ExprValue::copied(mem::owned_copy(self.memory, &*t)?));
                }
                capture.keep(
                    self.memory,
                    i,
                    e,
                    DrCrArgScope::NoSources,
                    t.into_native_owned()?,
                )?;
            } else if first.is_none() {
                first = Some(t);
            }
        }
        match first {
            Some(value) => Ok(value.into_native_owned()?.into_some()),
            None => Ok(mem::NativeOwned::none(self.memory)),
        }
    }

    fn vt_condition<'ast>(
        &mut self,
        id: QTableId,
        i: usize,
        arg: &'ast ast::VtArg,
        scope: FieldScope,
        mut capture: Option<&mut DrCrCapture<'ast, 'scope>>,
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
            if let Some(capture) = capture.as_mut() {
                if capture.ordinary && first.is_none() {
                    first = Some(mem::ExprValue::copied(mem::owned_copy(self.memory, &*t)?));
                }
                capture.keep(
                    self.memory,
                    i,
                    e,
                    DrCrArgScope::Condition,
                    t.into_native_owned()?,
                )?;
            } else if first.is_none() {
                first = Some(t);
            }
        }
        match first {
            Some(value) => Ok(value.into_native_owned()?.into_some()),
            None => Ok(mem::NativeOwned::none(self.memory)),
        }
    }

    fn vt_account_condition<'ast>(
        &mut self,
        id: QTableId,
        i: usize,
        arg: &'ast ast::VtArg,
        mut capture: Option<&mut DrCrCapture<'ast, 'scope>>,
    ) -> R<mem::NativeOwned<'scope, Option<TExpr>>> {
        let (role, ru, en) = account_condition(
            self.schema
                .table(id)
                .params
                .get(i)
                .map(|p| p.name_en.as_str()),
        )
        .unwrap_or((StdRole::Account, "Счет", "Account"));
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
            if let Some(capture) = capture.as_mut() {
                if capture.ordinary && first.is_none() {
                    first = Some(mem::ExprValue::copied(mem::owned_copy(self.memory, &*t)?));
                }
                capture.keep(
                    self.memory,
                    i,
                    e,
                    DrCrArgScope::Account(role),
                    t.into_native_owned()?,
                )?;
            } else if first.is_none() {
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

    pub(crate) fn vt_checks(&mut self, levels: &[Level]) -> R<()> {
        for level in levels {
            for src in &level.sources {
                let Some(check) = &src.vt_check else { continue };
                if !src.dim_res_used {
                    if self.defer_accum_balance_failure
                        && let super::scope::SourceCols::Table { id, .. } = &src.cols
                        && self.schema.table(self.schema.root(*id)).kind
                            == TableKind::Object(MdKind::AccumulationRegister)
                    {
                        self.pending_view_failure = Some(*id);
                    }
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

pub(crate) fn account_condition(
    name: Option<&str>,
) -> Option<(StdRole, &'static str, &'static str)> {
    Some(match name? {
        "Account condition" => (StdRole::Account, "Счет", "Account"),
        "Balanced account condition" => (StdRole::BalancedAccount, "КорСчет", "BalancedAccount"),
        "AccountDr condition" => (StdRole::AccountDr, "СчетДт", "AccountDr"),
        "Account Cr condition" => (StdRole::AccountCr, "СчетКт", "AccountCr"),
        _ => return None,
    })
}
