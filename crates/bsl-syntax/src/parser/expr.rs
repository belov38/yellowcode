use super::{BuiltinSig, Parser};
use crate::ast::{Arg, BinOp, Expr, ExprKind, Ident, Literal, UnOp};
use crate::diag::{MsgId, msg};
use crate::keywords::{Kw, ident_is};
use crate::lexer::{Token, TokenKind};
use crate::source::Span;

pub(crate) type PExpr = Result<Expr, Vec<Expr>>;

type PList<T> = Result<(Vec<T>, u32), Vec<Expr>>;

const PREC_OR: u8 = 1;
const PREC_NOT: u8 = 3;
const PREC_CMP: u8 = 4;

const TERNARY: BuiltinSig = BuiltinSig {
    min: 3,
    max: Some(3),
};
const NEW_DYNAMIC: BuiltinSig = BuiltinSig {
    min: 1,
    max: Some(2),
};

fn binop(k: TokenKind) -> Option<BinOp> {
    Some(match k {
        TokenKind::Kw(Kw::Or) => BinOp::Or,
        TokenKind::Kw(Kw::And) => BinOp::And,
        TokenKind::Eq => BinOp::Eq,
        TokenKind::Ne => BinOp::Ne,
        TokenKind::Lt => BinOp::Lt,
        TokenKind::Le => BinOp::Le,
        TokenKind::Gt => BinOp::Gt,
        TokenKind::Ge => BinOp::Ge,
        TokenKind::Plus => BinOp::Add,
        TokenKind::Minus => BinOp::Sub,
        TokenKind::Star => BinOp::Mul,
        TokenKind::Slash => BinOp::Div,
        TokenKind::Percent => BinOp::Mod,
        _ => return None,
    })
}

fn prec(op: BinOp) -> u8 {
    match op {
        BinOp::Or => PREC_OR,
        BinOp::And => 2,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => PREC_CMP,
        BinOp::Add | BinOp::Sub => 5,
        BinOp::Mul | BinOp::Div | BinOp::Mod => 6,
    }
}

fn binary_node(op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
    let (start, end) = (lhs.span.start, rhs.span.end);
    mk(
        ExprKind::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        start,
        end,
    )
}

fn is_unary(k: TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Minus | TokenKind::Plus | TokenKind::Kw(Kw::Not)
    )
}

fn starts_await_operand(k: TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Number
            | TokenKind::Str(_)
            | TokenKind::Date(_)
            | TokenKind::Ident
            | TokenKind::Question
            | TokenKind::Kw(Kw::True | Kw::False | Kw::Undefined | Kw::Null | Kw::Not | Kw::New)
    )
}

fn mk(kind: ExprKind, start: u32, end: u32) -> Expr {
    Expr {
        kind,
        span: Span { start, end },
    }
}

pub(super) fn prepend(first: Expr, mut rest: Vec<Expr>) -> Vec<Expr> {
    rest.insert(0, first);
    rest
}

fn callee_parts(callee: Expr, rest: Vec<Expr>) -> Vec<Expr> {
    let span = callee.span;
    match callee.into_kind() {
        ExprKind::Name(_) => rest,
        ExprKind::Member { object, .. } => prepend(*object, rest),
        kind => prepend(Expr { kind, span }, rest),
    }
}

fn values(args: Vec<Arg>) -> Vec<Expr> {
    args.into_iter().filter_map(|a| a.value).collect()
}

fn to_args(vals: Vec<Expr>) -> Vec<Arg> {
    vals.into_iter()
        .map(|e| Arg {
            span: e.span,
            value: Some(e),
        })
        .collect()
}

impl Parser<'_> {
    #[inline]
    pub(crate) fn expr(&mut self, missing: MsgId) -> PExpr {
        self.binary(PREC_OR, missing)
    }

    fn err_after(&mut self, m: MsgId) -> PExpr {
        self.fail_after(m, None);
        Err(Vec::new())
    }

    fn binary(&mut self, min: u8, missing: MsgId) -> PExpr {
        let base = self.ops.len();
        let mut lhs = self.operand(min, missing)?;
        loop {
            let next = binop(self.kind()).filter(|&op| prec(op) >= min);
            while let Some(top) = self
                .ops
                .get(base..)
                .and_then(<[_]>::last)
                .map(|t| prec(t.0))
                && next.is_none_or(|op| prec(op) <= top)
                && let Some((op, l)) = self.ops.pop()
            {
                lhs = binary_node(op, l, lhs);
            }
            let Some(op) = next else {
                return Ok(lhs);
            };
            self.bump();
            self.ops.push((op, lhs));
            lhs = match self.operand(prec(op) + 1, msg::EXPR_ERROR) {
                Ok(e) => e,
                Err(parts) => {
                    let at = base.min(self.ops.len());
                    let mut v: Vec<Expr> = self.ops.drain(at..).map(|(_, l)| l).collect();
                    v.extend(parts);
                    return Err(v);
                }
            };
        }
    }

    fn operand(&mut self, min: u8, missing: MsgId) -> PExpr {
        let t = self.tok();
        let op = match t.kind {
            TokenKind::Minus => UnOp::Neg,
            TokenKind::Plus => UnOp::Pos,
            TokenKind::Kw(Kw::Not) if min <= PREC_NOT => UnOp::Not,
            _ => return self.primary(missing),
        };
        if !self.enter() {
            return Err(Vec::new());
        }
        self.bump();
        let not_sign = op == UnOp::Not && matches!(self.kind(), TokenKind::Minus | TokenKind::Plus);
        let inner = if is_unary(self.kind()) && !not_sign {
            self.err_after(msg::EXP_EXPR)
        } else if op == UnOp::Not {
            self.binary(PREC_CMP, msg::EXP_EXPR)
        } else {
            self.primary(msg::EXP_EXPR)
        };
        self.leave();
        let operand = inner?;
        let end = operand.span.end;
        Ok(mk(
            ExprKind::Unary {
                op,
                operand: Box::new(operand),
            },
            t.span.start,
            end,
        ))
    }

    fn primary(&mut self, missing: MsgId) -> PExpr {
        let t = self.tok();
        let lit = match t.kind {
            TokenKind::Ident => return self.name_chain(),
            TokenKind::LParen => return self.paren(),
            TokenKind::Question => return self.ternary_chain(),
            TokenKind::Kw(Kw::New) => return self.new_expr(),
            TokenKind::Number => Literal::Number(self.src.text(t.span)),
            TokenKind::Str(i) => Literal::Str(
                self.strings
                    .get_mut(i as usize)
                    .map(std::mem::take)
                    .unwrap_or_default(),
            ),
            TokenKind::Date(d) => Literal::Date(d),
            TokenKind::Kw(Kw::True) => Literal::Bool(true),
            TokenKind::Kw(Kw::False) => Literal::Bool(false),
            TokenKind::Kw(Kw::Undefined) => Literal::Undefined,
            TokenKind::Kw(Kw::Null) => Literal::Null,
            _ => return self.err_after(missing),
        };
        self.bump();
        Ok(Expr {
            kind: ExprKind::Lit(lit),
            span: t.span,
        })
    }

    fn is_await(&self, t: Token) -> bool {
        t.kind == TokenKind::Ident && {
            let u = self.units(t.span);
            ident_is(u, "ждать") || ident_is(u, "await")
        }
    }

    pub(crate) fn name_chain(&mut self) -> PExpr {
        self.chain(false)
    }

    pub(crate) fn stmt_chain(&mut self) -> PExpr {
        self.chain(true)
    }

    fn chain(&mut self, stmt: bool) -> PExpr {
        let t = self.tok();
        if self.is_await(t) {
            if self.is_async {
                return self.await_expr();
            }
            if starts_await_operand(self.kind_at(1)) {
                self.fail_at(msg::AWAIT_INVALID, None, t.span.start);
                return Err(Vec::new());
            }
        }
        self.bump();
        let name = self.ident(t);
        let sig = match self.kind() {
            TokenKind::LParen => self.builtins.builtin(&name.name),
            _ => None,
        };
        let (e, callable) = match sig {
            None => (mk(ExprKind::Name(name), t.span.start, t.span.end), true),
            Some(_) if stmt => {
                self.fail_after(msg::BLTNFUNC_BAD_USE, Some(name.name));
                return Err(Vec::new());
            }
            Some(sig) => (self.builtin_call(name, sig)?, false),
        };
        self.postfix(e, callable)
    }

    fn ternary_chain(&mut self) -> PExpr {
        let e = self.ternary()?;
        match self.kind() {
            TokenKind::Dot | TokenKind::LBracket => self.postfix_after(e),
            _ => Ok(e),
        }
    }

    #[inline(never)]
    fn postfix_after(&mut self, e: Expr) -> PExpr {
        self.postfix(e, false)
    }

    #[inline(always)]
    fn postfix(&mut self, mut e: Expr, mut callable: bool) -> PExpr {
        loop {
            match self.kind() {
                TokenKind::Dot => {
                    self.bump();
                    let n = self.tok();
                    if !matches!(n.kind, TokenKind::Ident | TokenKind::Kw(_)) {
                        self.fail_after(msg::EXP_IDENT, None);
                        return Err(vec![e]);
                    }
                    self.bump();
                    let start = e.span.start;
                    e = mk(
                        ExprKind::Member {
                            object: Box::new(e),
                            name: self.ident(n),
                        },
                        start,
                        n.span.end,
                    );
                    callable = true;
                }
                TokenKind::LBracket => {
                    e = self.index(e)?;
                    callable = false;
                }
                TokenKind::LParen if callable => {
                    let (args, end) = match self.args() {
                        Ok(a) => a,
                        Err(parts) => return Err(callee_parts(e, parts)),
                    };
                    let start = e.span.start;
                    e = mk(
                        ExprKind::Call {
                            callee: Box::new(e),
                            args,
                        },
                        start,
                        end,
                    );
                    callable = false;
                }
                _ => return Ok(e),
            }
        }
    }

    #[inline(never)]
    fn builtin_call(&mut self, name: Ident, sig: BuiltinSig) -> PExpr {
        let span = name.span;
        let start = span.start;
        let (vals, end) = self.strict_args(sig)?;
        let callee = Expr {
            kind: ExprKind::Name(name),
            span,
        };
        Ok(mk(
            ExprKind::Call {
                callee: Box::new(callee),
                args: to_args(vals),
            },
            start,
            end,
        ))
    }

    fn index(&mut self, object: Expr) -> PExpr {
        if !self.enter() {
            return Err(vec![object]);
        }
        self.bump();
        let index = self.expr(msg::EXPR_ERROR);
        self.leave();
        let index = match index {
            Ok(i) => i,
            Err(parts) => return Err(prepend(object, parts)),
        };
        if self.kind() != TokenKind::RBracket {
            self.fail_after(msg::EXP_RBRACK, None);
            return Err(vec![object, index]);
        }
        let end = self.tok().span.end;
        self.bump();
        let start = object.span.start;
        Ok(mk(
            ExprKind::Index {
                object: Box::new(object),
                index: Box::new(index),
            },
            start,
            end,
        ))
    }

    fn paren(&mut self) -> PExpr {
        let start = self.tok().span.start;
        if !self.enter() {
            return Err(Vec::new());
        }
        self.bump();
        let inner = self.expr(msg::EXPR_ERROR);
        self.leave();
        let inner = inner?;
        if self.kind() != TokenKind::RParen {
            self.fail_after(msg::EXP_RPAR, None);
            return Err(vec![inner]);
        }
        let end = self.tok().span.end;
        self.bump();
        Ok(mk(ExprKind::Paren(Box::new(inner)), start, end))
    }

    fn ternary(&mut self) -> PExpr {
        let start = self.tok().span.start;
        self.bump();
        if self.kind() != TokenKind::LParen {
            return self.err_after(msg::EXP_LPAR);
        }
        let (vals, end) = self.strict_args(TERNARY)?;
        let mut it = vals.into_iter();
        match (it.next(), it.next(), it.next()) {
            (Some(cond), Some(then), Some(otherwise)) => Ok(mk(
                ExprKind::Ternary {
                    cond: Box::new(cond),
                    then: Box::new(then),
                    otherwise: Box::new(otherwise),
                },
                start,
                end,
            )),
            (a, b, _) => {
                self.fail_after(msg::EXP_COMMA, None);
                Err(a.into_iter().chain(b).collect())
            }
        }
    }

    fn new_expr(&mut self) -> PExpr {
        let start = self.tok().span.start;
        if !self.enter() {
            return Err(Vec::new());
        }
        self.bump();
        let r = self.new_tail(start);
        self.leave();
        r
    }

    fn new_tail(&mut self, start: u32) -> PExpr {
        let t = self.tok();
        match t.kind {
            TokenKind::Ident => {
                self.bump();
                let type_name = self.ident(t);
                if self.kind() != TokenKind::LParen {
                    return Ok(mk(
                        ExprKind::New {
                            type_name,
                            args: None,
                        },
                        start,
                        t.span.end,
                    ));
                }
                let (args, end) = self.args()?;
                Ok(mk(
                    ExprKind::New {
                        type_name,
                        args: Some(args),
                    },
                    start,
                    end,
                ))
            }
            TokenKind::LParen => {
                let (vals, end) = self.strict_args(NEW_DYNAMIC)?;
                Ok(mk(
                    ExprKind::NewDynamic {
                        args: to_args(vals),
                    },
                    start,
                    end,
                ))
            }
            _ => self.err_after(msg::EXP_IDENT),
        }
    }

    fn await_expr(&mut self) -> PExpr {
        let start = self.tok().span.start;
        if !self.enter() {
            return Err(Vec::new());
        }
        self.bump();
        let inner = self.primary(msg::EXP_EXPR);
        self.leave();
        let inner = inner?;
        let end = inner.span.end;
        Ok(mk(ExprKind::Await(Box::new(inner)), start, end))
    }

    pub(super) fn args(&mut self) -> PList<Arg> {
        if !self.enter() {
            return Err(Vec::new());
        }
        self.bump();
        let r = self.args_tail();
        self.leave();
        r
    }

    fn args_tail(&mut self) -> PList<Arg> {
        let mut args = Vec::new();
        if self.kind() == TokenKind::RParen {
            let end = self.tok().span.end;
            self.bump();
            return Ok((args, end));
        }
        loop {
            let t = self.tok();
            if matches!(t.kind, TokenKind::Comma | TokenKind::RParen) {
                args.push(Arg {
                    value: None,
                    span: Span {
                        start: t.span.start,
                        end: t.span.start,
                    },
                });
            } else {
                match self.expr(msg::EXPR_ERROR) {
                    Ok(e) => args.push(Arg {
                        span: e.span,
                        value: Some(e),
                    }),
                    Err(parts) => {
                        let mut v = values(args);
                        v.extend(parts);
                        return Err(v);
                    }
                }
            }
            match self.kind() {
                TokenKind::Comma => self.bump(),
                TokenKind::RParen => {
                    let end = self.tok().span.end;
                    self.bump();
                    return Ok((args, end));
                }
                _ => {
                    self.fail_after(msg::EXP_RPAR, None);
                    return Err(values(args));
                }
            }
        }
    }

    fn strict_args(&mut self, sig: BuiltinSig) -> PList<Expr> {
        if !self.enter() {
            return Err(Vec::new());
        }
        self.bump();
        let r = self.strict_args_tail(sig);
        self.leave();
        r
    }

    fn strict_args_tail(&mut self, sig: BuiltinSig) -> PList<Expr> {
        let mut vals = Vec::new();
        let min = usize::from(sig.min);
        if self.kind() == TokenKind::RParen && min == 0 {
            let end = self.tok().span.end;
            self.bump();
            return Ok((vals, end));
        }
        if sig.max == Some(0) {
            self.fail_after(msg::EXP_RPAR, None);
            return Err(vals);
        }
        loop {
            match self.expr(msg::EXP_EXPR) {
                Ok(e) => vals.push(e),
                Err(parts) => {
                    vals.extend(parts);
                    return Err(vals);
                }
            }
            let n = vals.len();
            let more = sig.max.is_none_or(|m| n < usize::from(m));
            match self.kind() {
                TokenKind::Comma if more => self.bump(),
                TokenKind::RParen if n >= min => {
                    let end = self.tok().span.end;
                    self.bump();
                    return Ok((vals, end));
                }
                _ => {
                    let m = if n < min {
                        msg::EXP_COMMA
                    } else {
                        msg::EXP_RPAR
                    };
                    self.fail_after(m, None);
                    return Err(vals);
                }
            }
        }
    }
}
