use super::pos::{Frame, elem_at, root_at};
use super::{Analyzer, R, boolean_of};
use crate::fold::{self, FoldError, Folded};
use crate::managed as mem;
use crate::params::{ParamConst, ParamShape};
use crate::schema::ValuePath;
use crate::tir::{TExpr, TKind};
use crate::types::rules::{self, Arg, ArgError, ArithError};
use crate::types::{DateQ, NumQ, Other, QType, StrQ};
use crate::value::QValue;
use crate::{AnalyzeMode, CompileError};
use query_syntax::ast::{self, BinOp, CastType, ExprKind, InRhs, Lit, UnOp};
use query_syntax::{Func, Kw, MsgArg, MsgId, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Clause {
    Select,
    Where,
    Having,
    On,
    GroupBy,
    OrderBy,
    Totals,
    VtArg,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ExprCtx {
    pub clause: Clause,
    pub in_agg: bool,
    pub clause_at: u32,
    pub and_root: Option<Span>,
}

impl ExprCtx {
    pub(crate) const SELECT: ExprCtx = ExprCtx::new(Clause::Select, 0);

    pub(crate) const fn new(clause: Clause, clause_at: u32) -> ExprCtx {
        ExprCtx {
            clause,
            in_agg: false,
            clause_at,
            and_root: None,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Hint<'a> {
    Plain,
    Cmp(&'a QType, u32, bool),
    In(&'a QType),
    Hierarchy(&'a QType),
    Arith,
    Like,
    Required(&'a QType),
}

const NUM0: NumQ = NumQ {
    digits: 0,
    scale: 0,
    nonneg: false,
};

pub(crate) fn is_type_ref(e: &TExpr) -> bool {
    matches!(e.kind, TKind::Value(ValuePath::TypeRef(_)))
}

pub(crate) fn unlimited_view() -> QType {
    QType {
        string: Some(StrQ {
            len: 0,
            fixed: false,
        }),
        ..QType::default()
    }
}

fn sys_enum_mismatch(a: &QType, b: &QType) -> bool {
    let only_sys_enum = |t: &QType| match t.others.as_slice() {
        [o @ Other::SysEnum(_)] if t.atom_count() == 1 => Some(*o),
        _ => None,
    };
    let clash = |x: &QType, y: &QType| {
        only_sys_enum(x).is_some_and(|o| y.atom_count() > 0 && !y.others.contains(&o))
    };
    clash(a, b) || clash(b, a)
}

fn number0() -> QType {
    QType::num(NUM0)
}

fn unqualified(t: QType) -> QType {
    let mut t = t.without_null();
    if t.number.is_some() {
        t.number = Some(NUM0);
    }
    if t.string.is_some() {
        t.string = Some(StrQ {
            len: 0,
            fixed: false,
        });
    }
    t
}

fn only_refs(scope: Option<&mem::CompileArena>, t: &QType) -> R<QType> {
    Ok(QType {
        refs: mem::copy(scope, &t.refs)?,
        ..QType::default()
    })
}

fn as_param(e: &ast::Expr) -> Option<&ast::Param> {
    let mut e = e;
    loop {
        match &e.kind {
            ExprKind::Paren(inner) => e = inner,
            ExprKind::Param(p) => return Some(p),
            _ => return None,
        }
    }
}

fn is_const(e: &TExpr) -> bool {
    match &e.kind {
        TKind::Const(_) | TKind::Param(_) | TKind::Unit(_) => true,
        TKind::Neg(x) => is_const(x),
        _ => false,
    }
}

fn const_value(e: &TExpr) -> Option<&QValue> {
    match &e.kind {
        TKind::Const(v) => Some(v),
        _ => None,
    }
}

fn const_int(e: &TExpr) -> Option<i64> {
    match const_value(e)? {
        QValue::Number(n) => Some(n.trunc_to_i64_saturating()),
        _ => None,
    }
}

pub(super) fn bool_like(t: &QType) -> bool {
    t.only_bool() || t.atom_count() == 0
}

fn is_number_like(t: &QType) -> bool {
    t.only_number() || t.atom_count() == 0
}

fn is_string_like(t: &QType) -> bool {
    t.string.is_some() || t.atom_count() == 0
}

fn unit_index(f: Func) -> Option<usize> {
    match f {
        Func::BeginOfPeriod | Func::EndOfPeriod | Func::DateAdd => Some(1),
        Func::DateDiff => Some(2),
        _ => None,
    }
}

fn is_aggregate(f: Func) -> bool {
    matches!(
        f,
        Func::Sum | Func::Avg | Func::Min | Func::Max | Func::Count
    )
}

fn op_text(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::And => "И",
        BinOp::Or => "ИЛИ",
    }
}

impl<'scope> Analyzer<'scope> {
    pub(crate) fn wrong_param(&self, arg: mem::Argument<'_>, at: u32) -> CompileError {
        self.quoted_argument_error(MsgId::SELECT_ERROR_WRONGPARAMETER, arg, at)
    }

    pub(crate) fn wrong_param_path(&self, path: &ast::Path) -> CompileError {
        self.quoted_path_error(MsgId::SELECT_ERROR_WRONGPARAMETER, path, path.span().start)
    }

    pub(crate) fn wrong_param_uppercase(&self, text: &str, at: u32) -> CompileError {
        if let Some(memory) = self.memory {
            return memory.quoted_chars_error(
                MsgId::SELECT_ERROR_WRONGPARAMETER,
                self.map.at(self.text, at),
                text.chars().flat_map(char::to_uppercase),
            );
        }
        self.wrong_param(mem::Argument::Text(&text.to_uppercase()), at)
    }

    pub(crate) fn wrong_compare(&self, at: u32) -> CompileError {
        self.message_error(MsgId::SELECT_ERROR_WRONGCOMPAREPARAMETERS, at)
    }

    pub(crate) fn owned_expr(&mut self, e: &ast::Expr) -> R<mem::OwnedExpr<'scope>> {
        self.expr_owned(e, ExprCtx::SELECT)
    }
    pub(crate) fn expr_owned(&mut self, e: &ast::Expr, ctx: ExprCtx) -> R<mem::OwnedExpr<'scope>> {
        self.expr_value(e, ctx).map(mem::ExprValue::into_owned)
    }
    fn scalar(
        &self,
        kind: mem::ScalarKind<'scope>,
        ty: mem::ScratchType<'scope>,
        span: Span,
    ) -> R<mem::ExprValue<'scope>> {
        mem::ExprValue::compose(self.memory, kind, ty, span)
    }
    fn scalar_type(&self, ty: QType) -> mem::ScratchType<'scope> {
        mem::ScratchType::new(self.memory, ty)
    }

    fn check_unary(&self, op: UnOp, operand: &ast::Expr, inner: &TExpr) -> R<()> {
        match op {
            UnOp::Not if !bool_like(&inner.ty) => {
                Err(self.wrong_param(mem::Argument::Kw(Kw::Not), root_at(operand)))
            }
            UnOp::Neg | UnOp::Plus if !is_number_like(&inner.ty) => Err(self.wrong_param(
                mem::Argument::Text(if op == UnOp::Neg { "-" } else { "+" }),
                operand.span.start,
            )),
            _ => Ok(()),
        }
    }

    pub(crate) fn expr_value(&mut self, e: &ast::Expr, ctx: ExprCtx) -> R<mem::ExprValue<'scope>> {
        if let Some(arena) = self.memory {
            arena.checkpoint()?;
        }
        let span = e.span;
        match &e.kind {
            ExprKind::Path(p) => self.path_expr_owned(p),
            ExprKind::Paren(inner) => self.expr_value(inner, ctx),
            ExprKind::Lit(l) => self.literal(l, span),
            ExprKind::Param(p) => self.param_expr(p, Hint::Plain),
            ExprKind::Compare {
                op,
                op_span,
                l: l_ast,
                r: r_ast,
            } => {
                self.frames
                    .push(self.memory, Frame::Compare { op: op_span.start })?;
                let lr = self.pair(l_ast, r_ast, ctx, op_span.start);
                self.frames.pop();
                let (l, r) = lr?;
                if !self.comparable(&l) || !self.comparable(&r) || sys_enum_mismatch(&l.ty, &r.ty) {
                    return Err(self.wrong_compare(op_span.start));
                }
                let ty = self.scalar_type(boolean_of(&[&l, &r]));
                self.scalar(
                    mem::ScalarKind::Compare(*op, l.boxed(self.memory)?, r.boxed(self.memory)?),
                    ty,
                    span,
                )
            }
            ExprKind::Chain { first, rest } => self.chain(e, first, rest, ctx),
            ExprKind::Unary { op, expr, .. } => self.unary(*op, expr, ctx, span),
            ExprKind::IsNull { l, not, .. } => {
                let l = self.expr_value(l, ctx)?;
                self.scalar(
                    mem::ScalarKind::IsNull {
                        l: l.boxed(self.memory)?,
                        not: *not,
                    },
                    self.scalar_type(QType::boolean()),
                    span,
                )
            }
            ExprKind::In {
                l,
                not,
                hierarchy,
                kw,
                rhs,
            } => self.in_pred(l, *not, *hierarchy, kw.start, rhs, ctx, span),
            ExprKind::Between {
                l, not, kw, lo, hi, ..
            } => {
                if let ExprKind::Unary {
                    op: UnOp::Not,
                    expr: x,
                    ..
                } = &l.kind
                {
                    self.expr_value(x, ctx)?;
                    return Err(self.wrong_param(mem::Argument::Kw(Kw::Not), root_at(x)));
                }
                let (lt, lo, hi) = if as_param(l).is_some() {
                    let lo = self.operand_value(
                        lo,
                        ctx,
                        Hint::Cmp(&QType::undefined(), kw.start, true),
                    )?;
                    let hi = self.operand_value(
                        hi,
                        ctx,
                        Hint::Cmp(&QType::undefined(), kw.start, true),
                    )?;
                    let other = if lo.ty.is_undefined() { &hi.ty } else { &lo.ty };
                    let lt = self.operand_value(l, ctx, Hint::Cmp(other, kw.start, true))?;
                    (lt, lo, hi)
                } else {
                    let lt = self.expr_value(l, ctx)?;
                    let lo = self.operand_value(lo, ctx, Hint::Cmp(&lt.ty, kw.start, true))?;
                    let hi = self.operand_value(hi, ctx, Hint::Cmp(&lt.ty, kw.start, true))?;
                    (lt, lo, hi)
                };
                if ![&lt, &lo, &hi].iter().all(|x| self.comparable(x)) {
                    return Err(self.wrong_compare(kw.start));
                }
                let ty = self.scalar_type(boolean_of(&[&lt, &lo, &hi]));
                self.scalar(
                    mem::ScalarKind::Between {
                        l: lt.boxed(self.memory)?,
                        not: *not,
                        lo: lo.boxed(self.memory)?,
                        hi: hi.boxed(self.memory)?,
                    },
                    ty,
                    span,
                )
            }
            ExprKind::Refs { l, kw, table } => {
                let lt = self.expr_value(l, ctx)?;
                let parts = &table.parts;
                let found = self
                    .schema
                    .find_table_managed(self.memory, parts)?
                    .and_then(|id| self.schema.table(id).reference.map(|r| (id, r)));
                let Some((id, atom)) = found else {
                    return Err(self.quoted_path_error(
                        MsgId::SELECT_ERROR_TABLENOTFOUND,
                        table,
                        kw.start,
                    ));
                };
                if lt.ty.atom_count() > 0 && !lt.ty.refs.contains(&atom) {
                    return Err(self.quoted_argument_error(
                        MsgId::SELECT_ERROR_INCOMPATIBLETYPES,
                        mem::Argument::Kw(Kw::Refs),
                        kw.start,
                    ));
                }
                let ty = self.scalar_type(boolean_of(&[&lt]));
                self.scalar(
                    mem::ScalarKind::Refs {
                        l: lt.boxed(self.memory)?,
                        table: id,
                    },
                    ty,
                    span,
                )
            }
            ExprKind::Like {
                l,
                not,
                kw: _,
                pattern,
                escape,
            } => {
                let lt = self.operand_value(l, ctx, Hint::Like)?;
                if !is_string_like(&lt.ty) {
                    return Err(self.wrong_like(root_at(l)));
                }
                let pt = self.operand_value(pattern, ctx, Hint::Like)?;
                if !is_string_like(&pt.ty) {
                    return Err(self.wrong_like(root_at(pattern)));
                }
                let ty = self.scalar_type(boolean_of(&[&lt, &pt]));
                self.scalar(
                    mem::ScalarKind::Like {
                        l: lt.boxed(self.memory)?,
                        not: *not,
                        pattern: pt.boxed(self.memory)?,
                        escape: escape.as_ref().and_then(|e| e.ch.chars().next()),
                    },
                    ty,
                    span,
                )
            }
            ExprKind::Tuple(_) => Err(self.unsupported("a tuple outside of В", span.start)),
            ExprKind::Case {
                operand,
                whens,
                else_,
                ..
            } => self.case(operand.as_deref(), whens, else_.as_deref(), ctx, span),
            ExprKind::Deref { expr, deref } => {
                let value = self.operand_value(expr, ctx, Hint::Plain)?;
                let target = mem::type_copy(self.memory, &value.ty)?;
                let nested = self.nested_table(&value);
                let (derefs, ty) =
                    self.derefs_owned(mem::type_copy(self.memory, &value.ty)?, nested, deref, 0)?;
                self.scalar(
                    mem::ScalarKind::Cast {
                        e: value.boxed(self.memory)?,
                        to: target,
                        derefs,
                        num: None,
                    },
                    ty,
                    span,
                )
            }
            ExprKind::Cast {
                kw,
                expr,
                ty,
                deref,
            } => self.cast(kw.start, expr, ty, deref, ctx, span),
            ExprKind::Func {
                func,
                name_span,
                distinct,
                args,
            } => self.func(*func, *name_span, distinct.is_some(), args, ctx, span),
            ExprKind::TypeOf { kw, name } => {
                let parts = &name.parts;
                let Some(t) = self.schema.type_by_name_managed(self.memory, parts)? else {
                    return Err(self.quoted_path_error(
                        MsgId::SELECT_ERROR_TABLENOTFOUND,
                        name,
                        kw.start,
                    ));
                };
                let t = self.scalar_type(t);
                let ty = self.scalar_type(mem::other(self.memory, Other::Type)?);
                self.scalar(mem::ScalarKind::Type(t), ty, span)
            }
            ExprKind::Value { path, .. } => {
                let parts = &path.parts;
                let vp = self.schema.value_path_owned(self.memory, parts)?;
                let ty = match &*vp {
                    ValuePath::Unknown => None,
                    v => self.schema.value_type_managed(self.memory, v)?,
                };
                let Some(ty) = ty else {
                    return Err(self.wrong_param_path(path));
                };
                self.scalar(mem::ScalarKind::Value(vp), self.scalar_type(ty), span)
            }
            ExprKind::DateTime { kw, args } => self.date_time(kw.start, args, span),
            ExprKind::Dcs(_) => Err(self.unsupported("a DCS block in an expression", span.start)),
        }
    }

    fn wrong_like(&self, at: u32) -> CompileError {
        self.wrong_param(mem::Argument::Kw(Kw::Like), at)
    }

    pub(super) fn operand_value(
        &mut self,
        e: &ast::Expr,
        ctx: ExprCtx,
        hint: Hint<'_>,
    ) -> R<mem::ExprValue<'scope>> {
        match as_param(e) {
            Some(p) => self.param_expr(p, hint),
            None => self.expr_value(e, ctx),
        }
    }

    fn pair(
        &mut self,
        l: &ast::Expr,
        r: &ast::Expr,
        ctx: ExprCtx,
        operator: u32,
    ) -> R<(mem::ExprValue<'scope>, mem::ExprValue<'scope>)> {
        match (as_param(l).is_some(), as_param(r).is_some()) {
            (true, false) => {
                let r = self.expr_value(r, ctx)?;
                let l = self.operand_value(l, ctx, Hint::Cmp(&r.ty, operator, true))?;
                Ok((l, r))
            }
            (false, true) => {
                let l = self.expr_value(l, ctx)?;
                let r = self.operand_value(r, ctx, Hint::Cmp(&l.ty, operator, true))?;
                Ok((l, r))
            }
            _ => {
                let l =
                    self.operand_value(l, ctx, Hint::Cmp(&QType::undefined(), operator, true))?;
                let r =
                    self.operand_value(r, ctx, Hint::Cmp(&QType::undefined(), operator, true))?;
                Ok((l, r))
            }
        }
    }

    fn literal(&self, l: &Lit, span: Span) -> R<mem::ExprValue<'scope>> {
        self.literal_owned(l, span).map(mem::ExprValue::owned)
    }

    pub(crate) fn literal_owned(&self, literal: &Lit, span: Span) -> R<mem::LiteralExpr<'scope>> {
        mem::LiteralExpr::new(self.memory, literal, span)?
            .ok_or_else(|| self.unsupported("number literal", span.start))
    }

    fn param_expr(&mut self, p: &ast::Param, hint: Hint<'_>) -> R<mem::ExprValue<'scope>> {
        let at = p.span.start;
        let name = mem::OwnedString::new(self.memory, &p.name.text)?;
        let (ty, use_ty) = if self.opts.mode == AnalyzeMode::Static {
            let use_ty = match hint {
                Hint::Plain => QType::undefined(),
                Hint::Cmp(t, _, strip) => {
                    let ty = mem::copy(self.memory, t)?;
                    if strip { unqualified(ty) } else { ty }
                }
                Hint::In(t) => mem::copy(self.memory, t)?.without_null(),
                Hint::Hierarchy(t) => only_refs(self.memory, t)?,
                Hint::Arith => number0(),
                Hint::Like => QType::str_(StrQ {
                    len: 0,
                    fixed: false,
                }),
                Hint::Required(t) => mem::copy(self.memory, t)?,
            };
            (QType::undefined(), use_ty)
        } else {
            let in_rhs = matches!(hint, Hint::In(_) | Hint::Hierarchy(_));
            let shape = self.params.shape(&name)?;
            let ty = match shape {
                None => {
                    return Err(self.quoted_text_error(
                        MsgId::SELECT_ERROR_PARAMETERVALUENOTFOUND,
                        &name,
                        at,
                    ));
                }
                Some(std::borrow::Cow::Owned(ParamShape::Value(t))) => t,
                Some(std::borrow::Cow::Borrowed(ParamShape::Value(t))) => {
                    mem::copy(self.memory, t)?
                }
                Some(std::borrow::Cow::Owned(ParamShape::List(t))) if in_rhs => t,
                Some(std::borrow::Cow::Borrowed(ParamShape::List(t))) if in_rhs => {
                    mem::copy(self.memory, t)?
                }
                Some(shape) if in_rhs && matches!(&*shape, ParamShape::Table(_)) => {
                    QType::undefined()
                }
                Some(shape) if matches!(&*shape, ParamShape::List(_)) => {
                    return Err(match hint {
                        Hint::Cmp(_, op, _) => self.wrong_compare(op),
                        _ => self.message_error(MsgId::SELECT_ERROR_WRONGPARAMETER, at),
                    });
                }
                Some(_) => return Err(self.message_error(MsgId::SELECT_ERROR_WRONGPARAMETER, at)),
            };
            let use_ty = self.scalar_type(ty);
            let ty = mem::type_copy(self.memory, &use_ty)?;
            (ty.into_type(), use_ty.into_type())
        };
        let ty = self.scalar_type(ty);
        self.record_param_use(&p.name.text, self.scalar_type(use_ty).without_null())?;
        self.scalar(mem::ScalarKind::Param(name), ty, p.span)
    }

    fn unary(
        &mut self,
        op: UnOp,
        x: &ast::Expr,
        ctx: ExprCtx,
        span: Span,
    ) -> R<mem::ExprValue<'scope>> {
        let inner = if op == UnOp::Not {
            self.expr_value(x, ctx)?
        } else {
            self.operand_value(x, ctx, Hint::Arith)?
        };
        self.check_unary(op, x, &inner)?;
        let ty = self.scalar_type(mem::OwnedExpr::unary_type(op, inner.ty.null));
        if op == UnOp::Plus {
            return inner.replace_type(self.memory, ty, span);
        }
        if op == UnOp::Neg
            && let TKind::Const(QValue::Number(n)) = &inner.kind
        {
            let number = mem::number_neg(self.memory, n)?;
            let result = self.scalar(mem::ScalarKind::Number(number), ty, span)?;
            drop(inner);
            return Ok(result);
        }
        let child = inner.boxed(self.memory)?;
        self.scalar(
            if op == UnOp::Not {
                mem::ScalarKind::Not(child)
            } else {
                mem::ScalarKind::Neg(child)
            },
            ty,
            span,
        )
    }

    fn chain(
        &mut self,
        e: &ast::Expr,
        first: &ast::Expr,
        rest: &[ast::ChainOp],
        ctx: ExprCtx,
    ) -> R<mem::ExprValue<'scope>> {
        let logic = rest.iter().all(|o| matches!(o.op, BinOp::And | BinOp::Or));
        if logic {
            let and = rest.first().is_none_or(|o| o.op == BinOp::And);
            let mut xs = mem::ExprList::new(self.memory, rest.len() + 1)?;
            for (i, x) in std::iter::once(first)
                .chain(rest.iter().map(|o| &o.expr))
                .enumerate()
            {
                let t = self.expr_value(x, ctx)?;
                if !bool_like(&t.ty) {
                    let at = elem_at(x);
                    if ctx.and_root == Some(e.span) {
                        let kw = if ctx.clause == Clause::Having {
                            Kw::Having
                        } else {
                            Kw::Where
                        };
                        return Err(self.quoted_argument_error(
                            MsgId::SELECT_ERROR_OPERATIONNOTALLOWEDINCLAUSE,
                            mem::Argument::Kw(kw),
                            at,
                        ));
                    }
                    let op = rest.get(i.saturating_sub(1)).map_or(BinOp::And, |o| o.op);
                    return Err(self.wrong_param(mem::Argument::Text(op_text(op)), at));
                }
                xs.push(t)?;
            }
            let ty = if xs.iter().any(|x| x.ty.null) {
                QType::boolean().with_null()
            } else {
                QType::boolean()
            };
            return self.scalar(
                if and {
                    mem::ScalarKind::And(xs)
                } else {
                    mem::ScalarKind::Or(xs)
                },
                self.scalar_type(ty),
                e.span,
            );
        }
        if rest.iter().any(|o| matches!(o.op, BinOp::And | BinOp::Or)) {
            return Err(self.unsupported("mixed chain", e.span.start));
        }
        self.arith_chain(e, first, rest, ctx)
    }

    fn arith_chain(
        &mut self,
        e: &ast::Expr,
        first: &ast::Expr,
        rest: &[ast::ChainOp],
        ctx: ExprCtx,
    ) -> R<mem::ExprValue<'scope>> {
        let mut acc = self.operand_value(first, ctx, Hint::Arith)?;
        let mut cur = mem::type_copy(self.memory, &acc.ty)?;
        let mut acc_const = is_const(&acc);
        let mut steps = mem::OwnedSteps::new(self.memory, 0)?;
        let mut sbuf: Option<mem::Utf16Buffer<'scope>> = None;
        for o in rest {
            let mut rhs = self.operand_value(&o.expr, ctx, Hint::Arith)?;
            let rhs_const = is_const(&rhs);
            let l = Arg {
                ty: &cur,
                constant: acc_const,
                int: None,
                unit: None,
            };
            let r = Arg {
                ty: &rhs.ty,
                constant: rhs_const,
                int: None,
                unit: None,
            };
            let res = match rules::arith(o.op, &l, &r) {
                Ok(ty) => ty,
                Err(ArithError::Operand) => {
                    return Err(self.wrong_param(mem::Argument::Text(op_text(o.op)), e.span.start));
                }
                Err(ArithError::Mix) => {
                    return Err(
                        self.wrong_param(mem::Argument::Text(op_text(o.op)), o.op_span.start)
                    );
                }
            };
            let ty = self.scalar_type(if cur.null || rhs.ty.null {
                res.with_null()
            } else {
                res
            });
            if self.memory.is_some()
                && steps.is_empty()
                && matches!(acc.kind, TKind::Const(QValue::Number(_)))
                && matches!(rhs.kind, TKind::Const(QValue::Number(_)))
            {
                let numeric_ty = mem::type_copy(self.memory, &ty)?;
                let Some(arena) = self.memory else {
                    return Err(self.unsupported("numeric arena", e.span.start));
                };
                match mem::numeric_prefix_owned(arena, acc, rhs, o.op, numeric_ty)? {
                    mem::OwnedNumericPrefix::Value(value) => {
                        acc = value;
                        acc_const &= rhs_const;
                        cur = ty;
                        continue;
                    }
                    mem::OwnedNumericPrefix::Runtime(a, b) => {
                        acc = a;
                        rhs = b;
                    }
                    mem::OwnedNumericPrefix::TooLong(digits) => {
                        return Err(self.quoted_text_error(
                            MsgId::SELECT_ERROR_TOOLONGLENGTHDESCRIPTOR,
                            digits.as_str(),
                            o.op_span.start,
                        ));
                    }
                }
            }
            if o.op == BinOp::Add
                && steps.is_empty()
                && let Some(QValue::String(b)) = const_value(&rhs)
                && (sbuf.is_some() || matches!(const_value(&acc), Some(QValue::String(_))))
            {
                if sbuf.is_none() {
                    let initial = match const_value(&acc) {
                        Some(QValue::String(a)) => &a.0[..],
                        _ => &[],
                    };
                    sbuf = Some(mem::Utf16Buffer::new(self.memory, initial)?);
                }
                let Some(buf) = sbuf.as_mut() else {
                    return Err(self.unsupported("string chain buffer", e.span.start));
                };
                crate::probe::hit_n(b.0.len() + 1);
                buf.extend(&b.0)?;
                let span = acc.span.to(rhs.span);
                acc = acc.replace_type(self.memory, mem::type_copy(self.memory, &ty)?, span)?;
                acc_const &= rhs_const;
                cur = ty;
                continue;
            }
            if let Some(buf) = sbuf.take() {
                let span = acc.span;
                let value = self.scalar(
                    mem::ScalarKind::String(mem::ScalarString::from_buffer(buf)?),
                    mem::type_copy(self.memory, &acc.ty)?,
                    span,
                )?;
                acc = value;
            }
            let folded = if self.memory.is_none() && steps.is_empty() {
                match (const_value(&acc), const_value(&rhs)) {
                    (Some(a), Some(b)) => match fold::arith(o.op, a, b) {
                        Ok(Folded::Value(value)) => Some(value),
                        Ok(Folded::Runtime)
                        | Err(FoldError::TooBigDate | FoldError::BadDate { .. }) => None,
                        Err(FoldError::TooLong(d)) => {
                            return Err(self
                                .err(MsgId::SELECT_ERROR_TOOLONGLENGTHDESCRIPTOR, o.op_span.start)
                                .with_quoted(MsgArg::text(&d)));
                        }
                    },
                    _ => None,
                }
            } else {
                None
            };
            if let Some(value) = folded {
                let span = acc.span.to(rhs.span);
                acc = self.legacy_folded(value, mem::type_copy(self.memory, &ty)?, span)?;
            } else {
                if steps.capacity() == 0 {
                    steps = mem::OwnedSteps::new(self.memory, rest.len())?;
                }
                steps.push(o.op, o.op_span, rhs, mem::type_copy(self.memory, &ty)?)?;
            }
            acc_const &= rhs_const;
            cur = ty;
        }
        if let Some(buf) = sbuf.take() {
            let span = acc.span;
            acc = self.scalar(
                mem::ScalarKind::String(mem::ScalarString::from_buffer(buf)?),
                mem::type_copy(self.memory, &acc.ty)?,
                span,
            )?;
        }
        if steps.is_empty() {
            return Ok(acc.span(e.span));
        }
        self.scalar(
            mem::ScalarKind::Arith {
                first: acc.boxed(self.memory)?,
                steps,
            },
            cur,
            e.span,
        )
    }

    fn legacy_folded(
        &self,
        value: QValue,
        ty: mem::ScratchType<'scope>,
        span: Span,
    ) -> R<mem::ExprValue<'scope>> {
        let kind = match value {
            QValue::Number(number) => mem::ScalarKind::Number(mem::ScratchNumber::legacy(number)),
            QValue::String(text) => mem::ScalarKind::String(mem::ScalarString::legacy(text)),
            QValue::Date(date) => mem::ScalarKind::Date(date),
            QValue::Null => {
                return self
                    .literal(&Lit::Null, span)?
                    .replace_type(self.memory, ty, span);
            }
            _ => return Err(self.unsupported("constant arithmetic output", span.start)),
        };
        self.scalar(kind, ty, span)
    }

    #[allow(clippy::too_many_arguments)]
    fn in_pred(
        &mut self,
        l: &ast::Expr,
        not: bool,
        hierarchy: bool,
        kw: u32,
        rhs: &InRhs,
        ctx: ExprCtx,
        span: Span,
    ) -> R<mem::ExprValue<'scope>> {
        let param_left = !matches!(l.kind, ExprKind::Tuple(_)) && as_param(l).is_some();
        let mut l_exprs = mem::ExprList::new(self.memory, 0)?;
        if !param_left {
            match &l.kind {
                ExprKind::Tuple(xs) => {
                    for x in xs {
                        l_exprs.push(self.expr_value(x, ctx)?)?;
                    }
                }
                _ => l_exprs.push(self.expr_value(l, ctx)?)?,
            }
        }
        if !l_exprs.iter().all(|x| self.comparable(x)) {
            return Err(self.wrong_compare(kw));
        }
        let undefined = QType::undefined();
        let left_ty = match l_exprs.as_slice() {
            [one] => &one.ty,
            _ => &undefined,
        };
        let mut nullable = l_exprs.iter().any(|x| x.ty.null);
        let mut other = mem::type_copy(self.memory, &undefined)?;
        let mut out_list = mem::ExprList::new(self.memory, 0)?;
        let mut out_query = None;
        match rhs {
            InRhs::List(xs) => {
                out_list = mem::ExprList::new(self.memory, xs.len())?;
                for ax in xs {
                    let hint = if hierarchy {
                        Hint::Hierarchy(left_ty)
                    } else {
                        Hint::In(left_ty)
                    };
                    let x = self.operand_value(ax, ctx, hint)?;
                    if !self.comparable(&x) {
                        return Err(self.wrong_compare(kw));
                    }
                    nullable |= x.ty.null;
                    if as_param(ax).is_none() {
                        let ty = mem::type_copy(self.memory, &x.ty)?.without_null();
                        other = mem::type_merge(self.memory, &other, &ty)?;
                    }
                    out_list.push(x)?;
                }
            }
            InRhs::Query(q) => {
                let q = self.subquery_owned(q)?;
                let types = super::select::column_types_scoped(
                    self.memory,
                    &q.trusted_borrow().parts,
                    q.trusted_borrow().totals.as_ref(),
                )?;
                nullable |= types.iter().any(|t| t.null);
                if let [one] = &*types {
                    other = mem::type_copy(self.memory, one)?.without_null();
                }
                out_query = Some(q);
            }
        }
        if param_left {
            let hint = Hint::Cmp(&other, kw, false);
            let lt = self.operand_value(l, ctx, hint)?;
            if !self.comparable(&lt) {
                return Err(self.wrong_compare(kw));
            }
            nullable |= lt.ty.null;
            l_exprs.push(lt)?;
        }
        let kind = match out_query {
            Some(q) => mem::ScalarKind::InQuery {
                q: mem::QueryBox::from_subquery(self.memory, q)?,
                l: l_exprs,
                not,
                hierarchy,
            },
            None => mem::ScalarKind::InList {
                l: l_exprs,
                not,
                hierarchy,
                rhs: out_list,
            },
        };
        let ty = if nullable {
            QType::boolean().with_null()
        } else {
            QType::boolean()
        };
        self.scalar(kind, self.scalar_type(ty), span)
    }

    fn case(
        &mut self,
        operand: Option<&ast::Expr>,
        whens: &[ast::When],
        else_: Option<&ast::Expr>,
        ctx: ExprCtx,
        span: Span,
    ) -> R<mem::ExprValue<'scope>> {
        let operand = match operand {
            Some(o) => Some(self.expr_value(o, ctx)?.boxed(self.memory)?),
            None => None,
        };
        let mut ws = mem::WhenList::new(self.memory, whens.len())?;
        let mut ty: Option<mem::ScratchType<'_>> = None;
        for w in whens {
            let cond = self.expr_value(&w.cond, ctx)?;
            if operand.is_none() && !bool_like(&cond.ty) && !is_const(&cond) {
                return Err(self.wrong_param(mem::Argument::Kw(Kw::Case), elem_at(&w.cond)));
            }
            let then = self.expr_value(&w.then, ctx)?;
            ty = Some(match ty {
                Some(t) => mem::type_merge(self.memory, &t, &then.ty)?,
                None => mem::type_copy(self.memory, &then.ty)?,
            });
            ws.push(cond, then)?;
        }
        let else_ = match else_ {
            Some(x) => Some(self.expr_value(x, ctx)?.boxed(self.memory)?),
            None => None,
        };
        let ty = match (ty, &else_) {
            (Some(t), Some(x)) => mem::type_merge(self.memory, &t, &x.ty)?,
            (Some(t), None) => t.with_null(),
            (None, Some(x)) => mem::type_copy(self.memory, &x.ty)?,
            (None, None) => self.scalar_type(QType::null_only()),
        };
        self.scalar(
            mem::ScalarKind::Case {
                operand,
                whens: ws,
                else_,
            },
            ty,
            span,
        )
    }

    fn cast(
        &mut self,
        kw: u32,
        x: &ast::Expr,
        to: &CastType,
        deref: &[ast::Ident],
        ctx: ExprCtx,
        span: Span,
    ) -> R<mem::ExprValue<'scope>> {
        let x = self.operand_value(x, ctx, Hint::Plain)?;
        let mut num = None;
        let target = match to {
            CastType::Boolean => QType::boolean(),
            CastType::Date => QType::date(DateQ::DateTime),
            CastType::Number { len, scale } => {
                let q = |l: &Option<query_syntax::ast::NumLit>| {
                    l.as_ref().map(|l| {
                        l.digits
                            .parse::<u32>()
                            .map_or(u8::MAX, |n| n.min(255) as u8)
                    })
                };
                num = q(len).map(|digits| NumQ {
                    digits,
                    scale: q(scale).unwrap_or(0),
                    nonneg: false,
                });
                number0()
            }
            CastType::String { len } => {
                let n = len
                    .as_ref()
                    .map_or(0, |l| l.digits.parse::<u32>().unwrap_or(u32::MAX));
                QType::str_(StrQ {
                    len: n,
                    fixed: false,
                })
            }
            CastType::Table(path) => {
                let parts = &path.parts;
                match self.schema.type_by_name_managed(self.memory, parts)? {
                    Some(t) => t,
                    None => {
                        return Err(self.quoted_path_error(
                            MsgId::SELECT_ERROR_TABLENOTFOUND,
                            path,
                            kw,
                        ));
                    }
                }
            }
        };
        let target = self.scalar_type(target);
        if !rules::cast(&x.ty, &target) || self.is_untyped_column(&x) {
            return Err(self.quoted_argument_error(
                MsgId::SELECT_ERROR_INCOMPATIBLETYPES,
                mem::Argument::Kw(Kw::Cast),
                kw,
            ));
        }
        let (derefs, ty) = if deref.is_empty() {
            let ty = if x.ty.null {
                mem::type_copy(self.memory, &target)?.with_null()
            } else {
                mem::type_copy(self.memory, &target)?
            };
            (mem::OwnedDerefs::new(self.memory, 0)?, ty)
        } else {
            self.derefs_owned(mem::type_copy(self.memory, &target)?, None, deref, 0)?
        };
        self.scalar(
            mem::ScalarKind::Cast {
                e: x.boxed(self.memory)?,
                to: target,
                derefs,
                num,
            },
            ty,
            span,
        )
    }

    fn date_time(&self, kw: u32, args: &[ast::Expr], span: Span) -> R<mem::ExprValue<'scope>> {
        let fail = |at: u32| self.wrong_param(mem::Argument::Kw(Kw::DateTime), at);
        let mut vals = mem::ScratchVec::new(self.memory, args.len())?;
        for a in args {
            let v = match &a.kind {
                ExprKind::Lit(Lit::Number(n)) => n.digits.parse::<i64>().ok(),
                _ => None,
            };
            let Some(v) = v else {
                return Err(fail(a.span.start));
            };
            vals.push(self.memory, v)?;
        }
        match fold::datetime(&vals) {
            Ok(d) => self.scalar(
                mem::ScalarKind::Date(d),
                self.scalar_type(QType::date(if args.len() == 3 {
                    DateQ::Date
                } else {
                    DateQ::DateTime
                })),
                span,
            ),
            Err(FoldError::BadDate { arg: Some(i) }) => {
                Err(fail(args.get(i).map_or(kw, |a| a.span.start)))
            }
            Err(FoldError::BadDate { arg: None }) => Err(fail(kw)),
            Err(FoldError::TooBigDate) => {
                Err(self.message_error(MsgId::SELECT_ERROR_TOOBIGDATE, kw))
            }
            Err(FoldError::TooLong(_)) => Err(fail(kw)),
        }
    }

    fn func(
        &mut self,
        f: Func,
        name: Span,
        distinct: bool,
        args: &ast::FuncArgs,
        ctx: ExprCtx,
        span: Span,
    ) -> R<mem::ExprValue<'scope>> {
        if is_aggregate(f) {
            return self.aggregate(f, name, distinct, args, ctx, span);
        }
        let ast::FuncArgs::List(list) = args else {
            return Err(self.wrong_param(mem::Argument::Func(f), name.start));
        };
        let mut xs = mem::ExprList::new(self.memory, list.len())?;
        let mut units = mem::ScratchVec::new(self.memory, list.len())?;
        for (i, a) in list.iter().enumerate() {
            let unit = if unit_index(f) == Some(i) {
                self.unit_word(a)
            } else {
                None
            };
            units.push(self.memory, unit)?;
            xs.push(match unit {
                Some(k) => self.scalar(
                    mem::ScalarKind::Unit(k),
                    self.scalar_type(QType::undefined()),
                    a.span,
                )?,
                None => {
                    let required = rules::arg_type(f, i);
                    let hint = match required.as_ref() {
                        Some(t) => Hint::Required(t),
                        None => Hint::Plain,
                    };
                    self.operand_value(a, ctx, hint)?
                }
            })?;
        }
        if f == Func::ValueType
            && self.opts.mode == AnalyzeMode::Execute
            && let Some(x) = xs.first()
            && matches!(x.kind, TKind::Param(_))
            && x.ty.others.contains(&Other::Type)
            && x.ty.atom_count() == 1
        {
            if let Some(memory) = self.memory {
                return Err(memory.semantic_error(
                    MsgId::SELECT_ERROR_WRONGPARAMETER,
                    None,
                    Some(mem::Argument::Text("VALUETYPE")),
                ));
            }
            return Err(CompileError::new(MsgId::SELECT_ERROR_WRONGPARAMETER, None)
                .with_quoted(MsgArg::text("VALUETYPE")));
        }
        if f == Func::StoredDataSize
            && let (Some(x), Some(a)) = (xs.first(), list.first())
            && !matches!(x.kind, TKind::Field { .. })
        {
            return Err(self.wrong_param(mem::Argument::Func(f), elem_at(a)));
        }
        let mut ints = mem::ScratchVec::new(self.memory, xs.len())?;
        for (i, x) in xs.iter().enumerate() {
            ints.push(
                self.memory,
                match const_int(x) {
                    Some(n) => Some(n),
                    None => self.param_int(f, i, x)?,
                },
            )?;
        }
        let rargs = mem::scratch_collect(
            self.memory,
            xs.iter().zip(&units).zip(&ints).map(|((x, u), n)| {
                Ok(Arg {
                    ty: &x.ty,
                    constant: is_const(x),
                    int: *n,
                    unit: *u,
                })
            }),
        )?;
        let recipe = match rules::func_recipe(f, &rargs) {
            Ok(t) => t,
            Err(ArgError::Func) => {
                return Err(self.wrong_param(mem::Argument::Func(f), name.start));
            }
            Err(ArgError::Count) => {
                return Err(self.wrong_param_uppercase(name.text(self.text), name.start));
            }
            Err(ArgError::At(i)) => {
                return Err(self.wrong_param(
                    mem::Argument::Func(f),
                    list.get(i).map_or(name.start, root_at),
                ));
            }
        };
        let res = self.scalar_type(mem::type_result(self.memory, recipe)?);
        let null = match f {
            Func::GroupedBy | Func::RecordAutoNumber => false,
            Func::IsNull => xs.get(1).is_some_and(|b| b.ty.null),
            _ => xs.iter().any(|x| x.ty.null),
        };
        if let Some(d) = self.fold_date_func(f, &xs, &units, name.start)? {
            return self.scalar(mem::ScalarKind::Date(d), res, span);
        }
        self.scalar(
            mem::ScalarKind::Func { f, args: xs },
            if null { res.with_null() } else { res },
            span,
        )
    }

    fn comparable(&self, expr: &TExpr) -> bool {
        if is_type_ref(expr) {
            return false;
        }
        let by_param = self.opts.mode == AnalyzeMode::Static
            && matches!(&expr.kind, TKind::Func { f: Func::Substring, args }
                if matches!(args.get(2).map(|a| &a.kind), Some(TKind::Param(_))));
        by_param || rules::comparable(&expr.ty)
    }

    fn param_int(&mut self, f: Func, i: usize, x: &TExpr) -> R<Option<i64>> {
        if self.opts.mode != AnalyzeMode::Execute
            || !matches!((f, i), (Func::Substring, 2) | (Func::Round, 1))
        {
            return Ok(None);
        }
        let TKind::Param(name) = &x.kind else {
            return Ok(None);
        };
        let constant = self.params.constant(name)?;
        let Some(ParamConst::Value(QValue::Number(n))) = constant.as_deref() else {
            return Ok(None);
        };
        let value = n.trunc_to_i64_saturating();
        drop(constant);
        self.note_value_dep(name)?;
        Ok(Some(value))
    }

    fn unit_word(&self, a: &ast::Expr) -> Option<Kw> {
        let ExprKind::Path(p) = &a.kind else {
            return None;
        };
        let [one] = p.parts.as_slice() else {
            return None;
        };
        let (set, _) = query_syntax::kw::lookup(&one.text);
        rules::UNITS_WITH_SECOND
            .into_iter()
            .find(|k| set.contains(*k))
    }

    fn fold_date_func(
        &self,
        f: Func,
        xs: &[TExpr],
        units: &[Option<Kw>],
        at: u32,
    ) -> R<Option<bsl_prim::BslDate>> {
        let date = xs.first().and_then(const_value);
        let unit = units.get(1).copied().flatten();
        let week = matches!(f, Func::BeginOfPeriod | Func::EndOfPeriod) && unit == Some(Kw::Week);
        let r = match (f, date, unit) {
            (Func::BeginOfPeriod, Some(QValue::Date(d)), Some(u)) => {
                Ok(fold::begin_of_period(*d, u))
            }
            (Func::EndOfPeriod, Some(QValue::Date(d)), Some(u)) => fold::end_of_period(*d, u),
            (Func::DateAdd, Some(QValue::Date(d)), Some(u)) => {
                match xs.get(2).and_then(const_value) {
                    Some(QValue::Number(n)) => fold::date_add(*d, u, n),
                    _ => return Ok(None),
                }
            }
            _ => return Ok(None),
        };
        match r {
            Ok(_) if week => Ok(None),
            Ok(d) => Ok(Some(d)),
            Err(FoldError::TooBigDate) => {
                Err(self.message_error(MsgId::SELECT_ERROR_TOOBIGDATE, at))
            }
            Err(_) => Ok(None),
        }
    }

    fn only_tabular_fields(&self, e: &TExpr) -> R<bool> {
        let mut stack = mem::ScratchVec::new(self.memory, 1)?;
        stack.push(self.memory, e)?;
        let mut any = false;
        while let Some(e) = stack.pop() {
            if let TKind::Field { derefs, .. } = &e.kind {
                any = true;
                let tabular = derefs.first().is_some_and(|d| {
                    !d.targets.is_empty()
                        && d.targets.iter().all(|(t, _)| {
                            self.schema.table(*t).kind == crate::schema::TableKind::Tabular
                        })
                });
                if !tabular {
                    return Ok(false);
                }
            } else {
                let mut result = Ok(());
                super::select::each_child(e, &mut |c| {
                    if result.is_ok() {
                        result = stack.push(self.memory, c);
                    }
                });
                result?;
            }
        }
        Ok(any)
    }

    fn aggregate(
        &mut self,
        f: Func,
        name: Span,
        distinct: bool,
        args: &ast::FuncArgs,
        ctx: ExprCtx,
        span: Span,
    ) -> R<mem::ExprValue<'scope>> {
        if ctx.in_agg {
            return Err(
                self.message_error(MsgId::SELECT_ERROR_ENESTEDAGGREGATESNOTALLOWED, name.start)
            );
        }
        let clause_kw = match ctx.clause {
            Clause::Where => Some(Kw::Where),
            Clause::On => Some(Kw::On),
            _ => None,
        };
        if let Some(k) = clause_kw {
            return Err(self.quoted_argument_error(
                MsgId::SELECT_ERROR_OPERATIONNOTALLOWEDINCLAUSE,
                mem::Argument::Kw(k),
                ctx.clause_at,
            ));
        }
        let inner = ExprCtx {
            in_agg: true,
            ..ctx
        };
        let (arg, ast_arg) = match args {
            ast::FuncArgs::Star(_) if f == Func::Count => (None, None),
            ast::FuncArgs::List(l) if l.len() == 1 => match l.first() {
                Some(a) => (Some(self.operand_value(a, inner, Hint::Plain)?), Some(a)),
                None => return Err(self.wrong_param(mem::Argument::Func(f), name.start)),
            },
            _ => return Err(self.wrong_param(mem::Argument::Func(f), name.start)),
        };
        let checked = arg.as_ref().map(|a| self.comparable_view(a)).transpose()?;
        let recipe = rules::aggregate_recipe(f, checked.as_deref()).map_err(|_| {
            let at = ast_arg.map_or(name.start, root_at);
            self.wrong_param(mem::Argument::Func(f), at)
        })?;
        let recipe = if let (Func::Min | Func::Max, Some(a)) = (f, arg.as_ref()) {
            rules::TypeResult::WithoutNull(&a.ty)
        } else {
            recipe
        };
        let res = self.scalar_type(mem::type_result(self.memory, recipe)?);
        drop(checked);
        let ty = if f == Func::Count {
            res
        } else {
            res.with_null()
        };
        let nested = match &arg {
            Some(a) => self.only_tabular_fields(a)?,
            None => false,
        };
        self.scalar(
            mem::ScalarKind::Agg {
                f,
                distinct,
                arg: arg.map(|value| value.boxed(self.memory)).transpose()?,
                nested,
            },
            ty,
            span,
        )
    }
}
