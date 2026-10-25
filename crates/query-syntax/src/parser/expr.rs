use super::{PResult, Parser, first_overflow, push_rec, reserved_prefix};
use crate::BUDGET_OPS;
use crate::ast::{
    BinOp, CastType, ChainOp, CmpOp, Escape, Expr, ExprKind, FuncArgs, Ident, InRhs, Lit, NumLit,
    Param, Path, UnOp, When,
};
use crate::diag::MsgArg;
use crate::kw::{Func, Kw};
use crate::lexer::{Punct, TokKind};
use crate::msg::MsgId;
use crate::source::Span;

fn or_op(p: &Parser<'_>) -> Option<BinOp> {
    p.at_kw(Kw::Or).then_some(BinOp::Or)
}

fn and_op(p: &Parser<'_>) -> Option<BinOp> {
    p.at_kw(Kw::And).then_some(BinOp::And)
}

fn add_op(p: &Parser<'_>) -> Option<BinOp> {
    match p.tok().kind {
        TokKind::Punct(Punct::Plus) => Some(BinOp::Add),
        TokKind::Punct(Punct::Minus) => Some(BinOp::Sub),
        _ => None,
    }
}

fn mul_op(p: &Parser<'_>) -> Option<BinOp> {
    match p.tok().kind {
        TokKind::Punct(Punct::Star) => Some(BinOp::Mul),
        TokKind::Punct(Punct::Slash) => Some(BinOp::Div),
        _ => None,
    }
}

fn cmp_op(p: &Parser<'_>) -> Option<CmpOp> {
    match p.tok().kind {
        TokKind::Punct(Punct::Eq) => Some(CmpOp::Eq),
        TokKind::Punct(Punct::Ne) => Some(CmpOp::Ne),
        TokKind::Punct(Punct::Lt) => Some(CmpOp::Lt),
        TokKind::Punct(Punct::Gt) => Some(CmpOp::Gt),
        TokKind::Punct(Punct::Le) => Some(CmpOp::Le),
        TokKind::Punct(Punct::Ge) => Some(CmpOp::Ge),
        _ => None,
    }
}

const PREDICATES: [Kw; 5] = [Kw::In, Kw::Between, Kw::Is, Kw::Refs, Kw::Like];

#[derive(Clone, Copy, PartialEq)]
enum Run {
    Signs,
    Nots,
}

fn between_of_not(
    memory: Option<&crate::resource::ParseScope>,
    op: UnOp,
    op_span: Span,
    between: Expr,
) -> PResult<Expr> {
    let span = op_span.to(between.span);
    let ExprKind::Between { l, not, kw, lo, hi } = between.kind else {
        return Ok(between);
    };
    let l_span = op_span.to(l.span);
    let l = Expr {
        kind: ExprKind::Unary {
            op,
            op_span,
            expr: l,
        },
        span: l_span,
    };
    Ok(Expr {
        kind: ExprKind::Between {
            l: crate::resource::boxed(memory, l)?,
            not,
            kw,
            lo,
            hi,
        },
        span,
    })
}

#[derive(PartialEq)]
enum PredicateLast {
    Open,
    CmpOnly,
    Closed,
}

impl<'t> Parser<'t> {
    pub fn expr(&mut self) -> PResult<Expr> {
        self.chain(Self::and_level, or_op)
    }

    fn and_level(&mut self) -> PResult<Expr> {
        self.chain(Self::not_level, and_op)
    }

    fn not_level(&mut self) -> PResult<Expr> {
        self.prefixed(
            |p| p.at_lead_kw(Kw::Not).then_some(UnOp::Not),
            Run::Nots,
            Self::pred,
        )
    }

    fn pred(&mut self) -> PResult<Expr> {
        let ops_limit = BUDGET_OPS as usize;
        let mut outer = std::mem::take(&mut self.rec);
        let base = self.p1;
        let mut l = self.add()?;
        let first_rec = std::mem::take(&mut self.rec);
        let mut right_recs: Vec<(usize, Vec<super::Rec>)> = Vec::new();
        let mut last = PredicateLast::Open;
        let mut n = 0usize;
        let mut signs: Vec<u32> = Vec::new();
        loop {
            if let Some(op) = cmp_op(self) {
                if last == PredicateLast::Closed {
                    return Err(self.unexpected());
                }
                let op_span = self.tok().span;
                crate::resource::push(self.memory, &mut signs, op_span.start)?;
                if n > 0 && n < ops_limit {
                    self.enter(op_span.start)?;
                }
                self.bump()?;
                self.p1 += 1;
                let r = self.add()?;
                self.p1 -= 1;
                n += 1;
                let rec = std::mem::take(&mut self.rec);
                if !rec.is_empty() {
                    crate::resource::push(self.memory, &mut right_recs, (n, rec))?;
                }
                if n <= ops_limit {
                    let span = l.span.to(r.span);
                    l = Expr {
                        kind: ExprKind::Compare {
                            op,
                            op_span,
                            l: crate::resource::boxed(self.memory, l)?,
                            r: crate::resource::boxed(self.memory, r)?,
                        },
                        span,
                    };
                }
                last = PredicateLast::Open;
            } else if self.at_predicate() {
                if last != PredicateLast::Open {
                    return Err(self.unexpected());
                }
                let is_in = self.at_kw(Kw::In) || (self.at_kw(Kw::Not) && self.nth_kw(1, Kw::In));
                let at = self.tok().span.start;
                crate::resource::push(self.memory, &mut signs, at)?;
                if n > 0 && n < ops_limit {
                    self.enter(at)?;
                }
                self.p1 += 1;
                let kept = n < ops_limit;
                let arg = if kept {
                    l
                } else {
                    Expr {
                        kind: ExprKind::Lit(Lit::Null),
                        span: Span::new(at, at),
                    }
                };
                let node = self.predicate(arg)?;
                self.p1 -= 1;
                n += 1;
                let rec = std::mem::take(&mut self.rec);
                if !rec.is_empty() {
                    crate::resource::push(self.memory, &mut right_recs, (n, rec))?;
                }
                last = match &node.kind {
                    _ if is_in => PredicateLast::Open,
                    ExprKind::Between { .. } | ExprKind::IsNull { .. } => PredicateLast::Closed,
                    _ => PredicateLast::CmpOnly,
                };
                l = if kept {
                    node
                } else {
                    Expr {
                        kind: ExprKind::Lit(Lit::Null),
                        span: Span::new(at, at),
                    }
                };
            } else {
                break;
            }
        }
        for _ in 1..n.min(ops_limit) {
            self.leave();
        }
        if n == 0 {
            for r in first_rec {
                push_rec(self.memory, &mut outer, r.cum, r.at)?;
            }
            self.rec = outer;
            return Ok(l);
        }
        let total = base as usize + n;
        if total > ops_limit {
            let k = total - ops_limit;
            return Err(self.overflow(signs.get(k - 1).copied().unwrap_or(0)));
        }
        let n32 = n as u32;
        if let Some(at) = first_overflow(&first_rec, n32) {
            return Err(self.overflow(at));
        }
        for (k, rec) in &right_recs {
            if let Some(at) = first_overflow(rec, n32 - *k as u32) {
                return Err(self.overflow(at));
            }
        }
        for j in 1..=n {
            push_rec(
                self.memory,
                &mut outer,
                base + j as u32,
                signs.get(n - j).copied().unwrap_or(0),
            )?;
        }
        for r in first_rec {
            push_rec(self.memory, &mut outer, r.cum + n32, r.at)?;
        }
        for (k, rec) in right_recs {
            for r in rec {
                push_rec(self.memory, &mut outer, r.cum + n32 - k as u32, r.at)?;
            }
        }
        self.rec = outer;
        Ok(l)
    }

    fn at_predicate(&self) -> bool {
        PREDICATES.iter().any(|k| self.at_kw(*k))
            || (self.at_kw(Kw::Not)
                && [Kw::In, Kw::Between, Kw::Like]
                    .iter()
                    .any(|k| self.nth_kw(1, *k)))
    }

    fn predicate(&mut self, l: Expr) -> PResult<Expr> {
        let start = l.span.start;
        let l = crate::resource::boxed(self.memory, l)?;
        let not = self.eat_kw(Kw::Not)?.is_some();
        let Some(op) = PREDICATES.into_iter().find(|k| self.at_kw(*k)) else {
            return Err(self.unexpected());
        };
        let kw = self.bump()?.span;
        let kind = match op {
            Kw::In => {
                let hierarchy = self.eat_kw(Kw::InHierarchy)?.is_some();
                self.open_paren()?;
                self.enter_bracket(false)?;
                let rhs = if self.at_lead_kw(Kw::Select) {
                    InRhs::Query(crate::resource::boxed(self.memory, self.query(false)?)?)
                } else {
                    InRhs::List(self.expr_list()?)
                };
                self.close_paren()?;
                self.leave_bracket(false);
                ExprKind::In {
                    l,
                    not,
                    hierarchy,
                    kw,
                    rhs,
                }
            }
            Kw::Between => {
                let lo = crate::resource::boxed(self.memory, self.add()?)?;
                self.expect_kw(Kw::And)?;
                let hi = crate::resource::boxed(self.memory, self.add()?)?;
                ExprKind::Between { l, not, kw, lo, hi }
            }
            Kw::Is => {
                let not = self.eat_kw(Kw::Not)?.is_some();
                self.expect_kw(Kw::Null)?;
                ExprKind::IsNull { l, not, kw }
            }
            Kw::Refs => ExprKind::Refs {
                l,
                kw,
                table: self.name_path()?,
            },
            _ => {
                let pattern = crate::resource::boxed(self.memory, self.add()?)?;
                let escape = match self.eat_kw(Kw::Escape)? {
                    Some(esc_kw) => Some(self.escape(esc_kw)?),
                    None => None,
                };
                ExprKind::Like {
                    l,
                    not,
                    kw,
                    pattern,
                    escape,
                }
            }
        };
        Ok(Expr {
            kind,
            span: Span::new(start, self.prev_end()),
        })
    }

    fn escape(&mut self, kw: Span) -> PResult<Escape> {
        let t = self.tok();
        let ch = crate::resource::decoded(self.memory, t.span.text(self.text))?;
        if t.kind != TokKind::Str || ch.chars().count() != 1 {
            return Err(self
                .err_here(MsgId::SELECT_ERROR_CONSTANTEXPECTED)
                .with_quoted(MsgArg::Kw(Kw::Escape)));
        }
        self.bump()?;
        Ok(Escape {
            kw,
            ch: crate::resource::text(self.memory, &ch)?,
            span: kw.to(t.span),
        })
    }

    pub fn add(&mut self) -> PResult<Expr> {
        self.chain(Self::mul, add_op)
    }

    fn mul(&mut self) -> PResult<Expr> {
        self.chain(Self::unary, mul_op)
    }

    fn unary(&mut self) -> PResult<Expr> {
        self.prefixed(
            |p| match p.tok().kind {
                TokKind::Punct(Punct::Minus) => Some(UnOp::Neg),
                TokKind::Punct(Punct::Plus) => Some(UnOp::Plus),
                _ => None,
            },
            Run::Signs,
            Self::primary,
        )
    }

    fn chain(
        &mut self,
        next: fn(&mut Self) -> PResult<Expr>,
        op_of: fn(&Self) -> Option<BinOp>,
    ) -> PResult<Expr> {
        let first = next(self)?;
        let mut rest = Vec::new();
        while let Some(op) = op_of(self) {
            let op_span = self.tok().span;
            self.bump()?;
            self.charge_op(op_span.start)?;
            let expr = next(self);
            self.uncharge_op(1);
            crate::resource::push(
                self.memory,
                &mut rest,
                ChainOp {
                    op,
                    op_span,
                    expr: expr?,
                },
            )?;
        }
        Ok(match rest.last() {
            None => first,
            Some(last) => {
                let span = first.span.to(last.expr.span);
                Expr {
                    kind: ExprKind::Chain {
                        first: crate::resource::boxed(self.memory, first)?,
                        rest,
                    },
                    span,
                }
            }
        })
    }

    fn prefixed(
        &mut self,
        op_of: fn(&Self) -> Option<UnOp>,
        run: Run,
        operand: fn(&mut Self) -> PResult<Expr>,
    ) -> PResult<Expr> {
        let mut ops: Vec<(UnOp, Span)> = Vec::new();
        while let Some(op) = op_of(self) {
            let span = self.tok().span;
            if run == Run::Nots {
                self.charge_units(1, span.end)?;
            }
            self.charge_op(if run == Run::Nots {
                span.end
            } else {
                span.start
            })?;
            self.enter(span.start)?;
            self.bump()?;
            crate::resource::push(self.memory, &mut ops, (op, span))?;
        }
        let count = ops.len() as u32;
        let mut e = operand(self)?;
        self.uncharge_op(count);
        if run == Run::Nots {
            self.uncharge_units(count);
        }
        if run == Run::Nots
            && !ops.is_empty()
            && let ExprKind::Between { .. } = e.kind
            && let Some((op, op_span)) = ops.pop()
        {
            self.leave();
            e = between_of_not(self.memory, op, op_span, e)?;
        }
        while let Some((op, op_span)) = ops.pop() {
            self.leave();
            let span = op_span.to(e.span);
            e = Expr {
                kind: ExprKind::Unary {
                    op,
                    op_span,
                    expr: crate::resource::boxed(self.memory, e)?,
                },
                span,
            };
        }
        Ok(e)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let expr = self.primary_atom()?;
        if !matches!(expr.kind, ExprKind::Paren(_)) {
            return Ok(expr);
        }
        let mut deref = Vec::new();
        while self.at_punct(Punct::Dot) && matches!(self.nth(1).kind, TokKind::Word { .. }) {
            self.bump()?;
            crate::resource::push(self.memory, &mut deref, self.word_ident()?)?;
        }
        if deref.is_empty() {
            return Ok(expr);
        }
        let span = Span::new(expr.span.start, self.prev_end());
        Ok(Expr {
            kind: ExprKind::Deref {
                expr: crate::resource::boxed(self.memory, expr)?,
                deref,
            },
            span,
        })
    }

    fn primary_atom(&mut self) -> PResult<Expr> {
        let t = self.tok();
        let lit = |kind| Expr {
            kind: ExprKind::Lit(kind),
            span: t.span,
        };
        if t.kind == TokKind::Punct(Punct::Dot) {
            let n = self.nth(1);
            if n.kind == TokKind::Number && n.span.start == t.span.end {
                self.check_leaf(n.span.end)?;
                self.bump()?;
                self.bump()?;
                let span = t.span.to(n.span);
                let path = Path {
                    parts: crate::resource::list(
                        self.memory,
                        [Ident {
                            text: crate::resource::text(self.memory, span.text(self.text))?,
                            span,
                        }],
                    )?,
                };
                return Ok(Expr {
                    kind: ExprKind::Path(path),
                    span,
                });
            }
        }
        match t.kind {
            TokKind::Number => {
                self.check_leaf(t.span.end)?;
                self.bump()?;
                Ok(lit(Lit::Number(NumLit {
                    digits: crate::resource::number(self.memory, t.span.text(self.text))?,
                })))
            }
            TokKind::Str => {
                self.check_leaf(t.span.end)?;
                self.bump()?;
                Ok(lit(Lit::String(crate::resource::decoded_text(
                    self.memory,
                    t.span.text(self.text),
                )?)))
            }
            TokKind::Param => {
                self.check_leaf(t.span.end)?;
                let p = self.param()?;
                Ok(Expr {
                    span: p.span,
                    kind: ExprKind::Param(p),
                })
            }
            TokKind::Punct(Punct::LParen) => self.paren(),
            TokKind::Word { .. } if self.at_word_before_dot() => {
                self.check_leaf(t.span.end)?;
                self.path_expr()
            }
            TokKind::Word { kws, func } => {
                let call = self.nth_punct(1, Punct::LParen);
                if call {
                    if let Some(f) = func {
                        return self.func(f);
                    }
                    if kws.contains(Kw::Cast) {
                        return self.cast();
                    }
                    if kws.contains(Kw::Type) || kws.contains(Kw::Value) {
                        return self.type_or_value(kws.contains(Kw::Type));
                    }
                    if kws.contains(Kw::DateTime) {
                        return self.datetime();
                    }
                }
                if kws.contains(Kw::Case) {
                    return self.case();
                }
                if call {
                    return Err(self.err(MsgId::SELECT_ERROR_SYNTAXERROR, t.span.start));
                }
                let word_lit = if kws.contains(Kw::True) {
                    Some(Lit::Bool(true))
                } else if kws.contains(Kw::False) {
                    Some(Lit::Bool(false))
                } else if kws.contains(Kw::Null) {
                    Some(Lit::Null)
                } else if kws.contains(Kw::Undefined) {
                    Some(Lit::Undefined)
                } else {
                    None
                };
                if let Some(l) = word_lit {
                    self.check_leaf(t.span.end)?;
                    self.bump()?;
                    return Ok(lit(l));
                }
                if reserved_prefix(kws) {
                    return Err(self.unexpected());
                }
                self.check_leaf(t.span.end)?;
                self.path_expr()
            }
            _ => Err(self.unexpected()),
        }
    }

    fn path_expr(&mut self) -> PResult<Expr> {
        let path = self.path()?;
        let span = path.span();
        Ok(Expr {
            kind: ExprKind::Path(path),
            span,
        })
    }

    fn paren(&mut self) -> PResult<Expr> {
        let open = self.open_paren()?;
        self.enter_bracket(false)?;
        let kind = match <[Expr; 1]>::try_from(self.expr_list()?) {
            Ok([e]) => ExprKind::Paren(crate::resource::boxed(self.memory, e)?),
            Err(items) => ExprKind::Tuple(items),
        };
        let close = self.close_paren()?;
        self.leave_bracket(false);
        Ok(Expr {
            kind,
            span: open.to(close),
        })
    }

    pub fn param(&mut self) -> PResult<Param> {
        let t = self.bump()?;
        let name = Span::new(t.span.start.saturating_add(1), t.span.end);
        Ok(Param {
            name: Ident {
                text: crate::resource::text(self.memory, name.text(self.text))?,
                span: name,
            },
            span: t.span,
        })
    }

    pub fn expr_list(&mut self) -> PResult<Vec<Expr>> {
        let mut items = crate::resource::list(self.memory, [self.expr()?])?;
        while self.eat_punct(Punct::Comma)?.is_some() {
            crate::resource::push(self.memory, &mut items, self.expr()?)?;
        }
        Ok(items)
    }

    pub fn open_paren(&mut self) -> PResult<Span> {
        if !self.at_punct(Punct::LParen) {
            return self.expect_punct(Punct::LParen);
        }
        let open = self.tok().span;
        self.enter(open.start)?;
        self.bump()?;
        Ok(open)
    }

    pub fn close_paren(&mut self) -> PResult<Span> {
        let close = self.expect_punct(Punct::RParen)?;
        self.leave();
        Ok(close)
    }

    fn func(&mut self, func: Func) -> PResult<Expr> {
        let name_span = self.bump()?.span;
        self.enter_bracket(true)?;
        self.open_paren()?;
        let mut distinct = None;
        let args = if self.at_punct(Punct::Star) {
            if func != Func::Count {
                return Err(self.unexpected());
            }
            FuncArgs::Star(self.bump()?.span)
        } else if func == Func::IsNull {
            let first = self.expr()?;
            self.expect_punct(Punct::Comma)?;
            FuncArgs::List(crate::resource::list(self.memory, [first, self.expr()?])?)
        } else {
            if func.is_aggregate() {
                distinct = self.eat_lead_kw(Kw::Distinct)?;
            }
            if distinct.is_none() && self.at_punct(Punct::RParen) {
                FuncArgs::List(Vec::new())
            } else {
                let list = self.expr_list()?;
                if func == Func::DateDiff && list.len() < 3 && self.at_punct(Punct::RParen) {
                    self.expect_punct(Punct::Comma)?;
                }
                FuncArgs::List(list)
            }
        };
        let close = self.close_paren()?;
        self.leave_bracket(true);
        Ok(Expr {
            kind: ExprKind::Func {
                func,
                name_span,
                distinct,
                args,
            },
            span: name_span.to(close),
        })
    }

    fn type_or_value(&mut self, type_of: bool) -> PResult<Expr> {
        let kw = self.bump()?.span;
        self.enter_bracket(true)?;
        self.open_paren()?;
        let path = if type_of {
            self.name_path()?
        } else {
            self.value_path()?
        };
        let close = self.close_paren()?;
        self.leave_bracket(true);
        Ok(Expr {
            kind: if type_of {
                ExprKind::TypeOf { kw, name: path }
            } else {
                ExprKind::Value { kw, path }
            },
            span: kw.to(close),
        })
    }

    fn value_path(&mut self) -> PResult<Path> {
        let t = self.tok();
        let shown = match t.kind {
            TokKind::Word { kws, .. } if !reserved_prefix(kws) => None,
            TokKind::Word { .. } | TokKind::Eof | TokKind::Error(_) => {
                return Err(self.unexpected());
            }
            TokKind::Str => Some(crate::resource::decoded(
                self.memory,
                t.span.text(self.text),
            )?),
            TokKind::Param => Some(crate::resource::string(
                self.memory,
                t.span.text(self.text).trim_start_matches('&'),
            )?),
            _ => Some(crate::resource::string(
                self.memory,
                t.span.text(self.text),
            )?),
        };
        if let Some(shown) = shown {
            return Err(self
                .err_here(MsgId::SELECT_ERROR_NAMEEXPECTED)
                .with_quoted(crate::resource::arg(self.memory, &shown)));
        }
        let mut parts = crate::resource::list(self.memory, [self.word_ident()?])?;
        loop {
            let (dot, word) = (self.tok(), self.nth(1));
            let end = self.prev_end();
            let joined = dot.kind == TokKind::Punct(Punct::Dot)
                && dot.span.start == end
                && matches!(word.kind, TokKind::Word { .. })
                && word.span.start == dot.span.end;
            if !joined {
                break;
            }
            self.bump()?;
            crate::resource::push(self.memory, &mut parts, self.word_ident()?)?;
        }
        let path = Path { parts };
        crate::resource::push(
            self.memory,
            &mut self.values,
            crate::ValueUse {
                path: crate::resource::string(self.memory, path.span().text(self.text))?,
                at: path.span().start,
            },
        )?;
        Ok(path)
    }

    fn datetime(&mut self) -> PResult<Expr> {
        let kw = self.bump()?.span;
        self.enter_bracket(true)?;
        self.open_paren()?;
        let args = if self.at_punct(Punct::RParen) {
            Vec::new()
        } else {
            self.expr_list()?
        };
        let close = self.close_paren()?;
        self.leave_bracket(true);
        Ok(Expr {
            kind: ExprKind::DateTime { kw, args },
            span: kw.to(close),
        })
    }

    fn cast(&mut self) -> PResult<Expr> {
        let kw = self.bump()?.span;
        self.enter_bracket(true)?;
        self.open_paren()?;
        let expr = crate::resource::boxed(self.memory, self.expr()?)?;
        self.expect_kw(Kw::As)?;
        let ty = self.cast_type()?;
        self.close_paren()?;
        self.leave_bracket(true);
        let mut deref = Vec::new();
        while self.at_punct(Punct::Dot) && matches!(self.nth(1).kind, TokKind::Word { .. }) {
            self.bump()?;
            crate::resource::push(self.memory, &mut deref, self.word_ident()?)?;
        }
        Ok(Expr {
            kind: ExprKind::Cast {
                kw,
                expr,
                ty,
                deref,
            },
            span: Span::new(kw.start, self.prev_end()),
        })
    }

    fn cast_type(&mut self) -> PResult<CastType> {
        let TokKind::Word { kws, .. } = self.tok().kind else {
            return Err(self.unexpected());
        };
        if kws.contains(Kw::Boolean) {
            self.bump()?;
            Ok(CastType::Boolean)
        } else if kws.contains(Kw::Date) {
            self.bump()?;
            Ok(CastType::Date)
        } else if kws.contains(Kw::Number) {
            self.bump()?;
            let (mut len, mut scale) = (None, None);
            if self.eat_punct(Punct::LParen)?.is_some() {
                len = Some(self.int_const()?);
                if self.eat_punct(Punct::Comma)?.is_some() {
                    scale = Some(self.int_const()?);
                }
                self.expect_punct(Punct::RParen)?;
            }
            Ok(CastType::Number { len, scale })
        } else if kws.contains(Kw::String) {
            self.bump()?;
            let mut len = None;
            if self.eat_punct(Punct::LParen)?.is_some() {
                len = Some(self.int_const()?);
                self.expect_punct(Punct::RParen)?;
            }
            Ok(CastType::String { len })
        } else {
            Ok(CastType::Table(self.name_path()?))
        }
    }

    pub fn top_count(&mut self) -> PResult<NumLit> {
        let t = self.tok();
        if t.kind != TokKind::Number {
            return Err(self.err_here(MsgId::SELECT_ERROR_CONSTANTEXPECTED));
        }
        self.bump()?;
        let (int, _) = crate::lexer::split_number(t.span.text(self.text));
        Ok(NumLit {
            digits: crate::resource::text(self.memory, int)?,
        })
    }

    pub fn int_const(&mut self) -> PResult<NumLit> {
        let t = self.tok();
        let text = t.span.text(self.text);
        if t.kind != TokKind::Number || text.contains('.') {
            return Err(self.err_here(MsgId::SELECT_ERROR_CONSTANTEXPECTED));
        }
        self.bump()?;
        Ok(NumLit {
            digits: crate::resource::number(self.memory, text)?,
        })
    }

    fn case(&mut self) -> PResult<Expr> {
        let kw = self.tok().span;
        self.enter(kw.start)?;
        self.bump()?;
        self.enter_bracket(true)?;
        let operand = if self.at_lead_kw(Kw::When) {
            None
        } else {
            Some(crate::resource::boxed(self.memory, self.expr()?)?)
        };
        let mut whens = Vec::new();
        loop {
            let when_kw = self.expect_kw(Kw::When)?;
            let cond = self.expr()?;
            self.expect_kw(Kw::Then)?;
            let then = self.expr()?;
            crate::resource::push(
                self.memory,
                &mut whens,
                When {
                    kw: when_kw,
                    cond,
                    then,
                },
            )?;
            if !self.at_kw(Kw::When) {
                break;
            }
        }
        let else_ = match self.eat_kw(Kw::Else)? {
            Some(_) => Some(crate::resource::boxed(self.memory, self.expr()?)?),
            None => None,
        };
        let end = self.expect_kw(Kw::End)?;
        self.leave();
        self.leave_bracket(true);
        Ok(Expr {
            kind: ExprKind::Case {
                kw,
                operand,
                whens,
                else_,
            },
            span: kw.to(end),
        })
    }

    pub fn name_path(&mut self) -> PResult<Path> {
        match self.tok().kind {
            TokKind::Word { kws, .. } if !reserved_prefix(kws) => self.path(),
            _ => Err(self.unexpected()),
        }
    }

    pub fn path(&mut self) -> PResult<Path> {
        let mut parts = crate::resource::list(self.memory, [self.word_ident()?])?;
        while self.at_punct(Punct::Dot) && matches!(self.nth(1).kind, TokKind::Word { .. }) {
            self.bump()?;
            crate::resource::push(self.memory, &mut parts, self.word_ident()?)?;
        }
        if self.at_punct(Punct::Dot)
            && matches!(self.nth(1).kind, TokKind::Error(_))
            && self.nth(1).span.text(self.text).starts_with('[')
        {
            self.bump()?;
        }
        Ok(Path { parts })
    }
}
