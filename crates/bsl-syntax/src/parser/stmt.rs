use std::collections::HashMap;

use super::Parser;
use super::expr::prepend;
use crate::ast::{Expr, ExprKind, Ident, MethodKind, RaiseForm, Stmt, StmtKind};
use crate::diag::{MsgId, msg};
use crate::fold::fold_ident;
use crate::keywords::Kw;
use crate::lexer::TokenKind;
use crate::source::Span;

#[derive(Clone, Copy)]
pub(super) struct Block {
    ends: &'static [Kw],
    pub(super) expect: MsgId,
}

const TEXT: Block = Block {
    ends: &[],
    expect: msg::EXP_STMTS,
};
const IF_PART: Block = Block {
    ends: &[Kw::ElsIf, Kw::Else, Kw::EndIf],
    expect: msg::EXP_ENDIF,
};
const LOOP_BODY: Block = Block {
    ends: &[Kw::EndDo],
    expect: msg::EXP_ENDDO,
};
const TRY_BODY: Block = Block {
    ends: &[Kw::Except, Kw::EndTry],
    expect: msg::EXP_EXCEPT,
};
const TRY_HANDLER: Block = Block {
    ends: &[Kw::Except, Kw::EndTry],
    expect: msg::EXP_ENDTRY,
};
pub(super) const PROC_BODY: Block = Block {
    ends: &[Kw::EndProcedure],
    expect: msg::EXP_ENDPROCEDURE,
};
pub(super) const FUNC_BODY: Block = Block {
    ends: &[Kw::EndFunction],
    expect: msg::EXP_ENDFUNCTION,
};

const NO_BLOCK: u32 = u32::MAX;

#[derive(Default)]
pub(super) struct BodyState {
    open: Vec<u32>,
    parents: Vec<u32>,
    labels: Vec<Mark>,
    gotos: Vec<Mark>,
}

struct Mark {
    key: String,
    written: String,
    block: u32,
}

impl BodyState {
    fn label_error(&self) -> Option<(MsgId, String)> {
        if self.gotos.is_empty() && self.labels.len() < 2 {
            return None;
        }
        let mut defs: HashMap<&str, u32> = HashMap::with_capacity(self.labels.len());
        for l in &self.labels {
            if defs.insert(&l.key, l.block).is_some() {
                return Some((msg::LABEL_DEFINED, l.written.clone()));
            }
        }
        for g in &self.gotos {
            match defs.get(g.key.as_str()) {
                None => return Some((msg::LABEL_NOTDEFINED, g.written.clone())),
                Some(&b) if !self.encloses(b, g.block) => {
                    return Some((msg::LABEL_WRONG_GOTO, g.written.clone()));
                }
                Some(_) => {}
            }
        }
        None
    }

    fn encloses(&self, outer: u32, mut inner: u32) -> bool {
        while inner != NO_BLOCK {
            if inner == outer {
                return true;
            }
            inner = self
                .parents
                .get(inner as usize)
                .copied()
                .unwrap_or(NO_BLOCK);
        }
        false
    }
}

fn is_block_word(k: Kw) -> bool {
    matches!(
        k,
        Kw::ElsIf
            | Kw::Else
            | Kw::EndIf
            | Kw::EndDo
            | Kw::Except
            | Kw::EndTry
            | Kw::EndProcedure
            | Kw::EndFunction
    )
}

fn unparen(e: Expr) -> Expr {
    let span = e.span;
    match e.into_kind() {
        ExprKind::Paren(inner) => *inner,
        kind => Expr { kind, span },
    }
}

fn name_expr(var: &Ident) -> Expr {
    Expr {
        kind: ExprKind::Name(var.clone()),
        span: var.span,
    }
}

impl Parser<'_> {
    pub(crate) fn text_body(&mut self) -> Vec<Stmt> {
        let body = self.block(TEXT);
        let close = self.tok().span.start;
        self.finish_labels(close);
        body
    }

    pub(super) fn block(&mut self, b: Block) -> Vec<Stmt> {
        let st = &mut self.body;
        let id = u32::try_from(st.parents.len()).unwrap_or(NO_BLOCK);
        st.parents.push(st.open.last().copied().unwrap_or(NO_BLOCK));
        st.open.push(id);
        let out = self.statements(b);
        self.body.open.pop();
        out
    }

    fn statements(&mut self, b: Block) -> Vec<Stmt> {
        let mut out = Vec::new();
        loop {
            match self.kind() {
                TokenKind::Semicolon => {
                    self.bump();
                    continue;
                }
                TokenKind::Eof => break,
                TokenKind::Kw(k) if b.ends.contains(&k) => break,
                _ => {}
            }
            let s = self.statement(b);
            let label = matches!(s.kind, StmtKind::Label(_));
            out.push(s);
            if self.failed() || !(label || self.stmt_end(b)) {
                break;
            }
        }
        out
    }

    fn stmt_end(&mut self, b: Block) -> bool {
        let t = self.tok();
        match t.kind {
            TokenKind::Semicolon => {
                self.bump();
                true
            }
            TokenKind::Eof => false,
            TokenKind::Kw(k) if b.ends.contains(&k) => false,
            TokenKind::Ident | TokenKind::Kw(_) => {
                self.fail_after(msg::EXP_SEMICOLON, None);
                false
            }
            TokenKind::Unknown => {
                self.fail_at(b.expect, None, t.span.start);
                false
            }
            _ => {
                self.fail_at(msg::UNRECOGN_STMT, None, t.span.start);
                false
            }
        }
    }

    fn at_stmt_end(&self, ends: &[Kw]) -> bool {
        match self.kind() {
            TokenKind::Semicolon | TokenKind::Eof => true,
            TokenKind::Kw(k) => ends.contains(&k),
            _ => false,
        }
    }

    fn statement(&mut self, b: Block) -> Stmt {
        let t = self.tok();
        let start = t.span.start;
        let kind = match t.kind {
            TokenKind::Ident => self.assign_or_call(start, b),
            TokenKind::Tilde => self.label(),
            TokenKind::Kw(Kw::If) => self.if_stmt(),
            TokenKind::Kw(Kw::While) => self.while_stmt(),
            TokenKind::Kw(Kw::For) => self.for_stmt(),
            TokenKind::Kw(Kw::Try) => self.try_stmt(),
            TokenKind::Kw(k) => self.keyword_stmt(k, start, b),
            TokenKind::Amp => self.annotated_stmt(start),
            TokenKind::Unknown => self.bad_start(b.expect, start),
            TokenKind::Bad(m) if m == msg::BAD_NUMCONST || m == msg::BAD_DATECONST => {
                self.fail_exact(msg::UNRECOGN_STMT, None, start);
                StmtKind::Incomplete(Vec::new())
            }
            _ => self.bad_start(msg::UNRECOGN_STMT, start),
        };
        Stmt {
            kind,
            span: Span {
                start,
                end: self.last_good_end.max(start),
            },
        }
    }

    #[inline(never)]
    fn keyword_stmt(&mut self, k: Kw, start: u32, b: Block) -> StmtKind {
        match k {
            Kw::Return => self.return_stmt(b),
            Kw::Break => self.loop_jump(StmtKind::Break, msg::BREAK_INVALID),
            Kw::Continue => self.loop_jump(StmtKind::Continue, msg::CONTINUE_INVALID),
            Kw::Raise => self.raise_stmt(b.ends),
            Kw::Goto => self.goto_stmt(),
            Kw::Execute => self.execute_stmt(),
            Kw::AddHandler => self.handler_stmt(true),
            Kw::RemoveHandler => self.handler_stmt(false),
            Kw::Var => self.bad_start(msg::LATE_DECLARE, start),
            Kw::Procedure | Kw::Function => self.misplaced_method(start, b),
            k if is_block_word(k) => self.bad_start(b.expect, start),
            _ => self.bad_start(msg::UNRECOGN_STMT, start),
        }
    }

    fn bad_start(&mut self, m: MsgId, at: u32) -> StmtKind {
        self.fail_at(m, None, at);
        StmtKind::Incomplete(Vec::new())
    }

    fn at_module_top(&self) -> bool {
        self.module_body && self.body.open.len() == 1
    }

    fn misplaced_method(&mut self, start: u32, b: Block) -> StmtKind {
        let m = if self.at_module_top() {
            msg::LATE_PROCFUNC
        } else {
            b.expect
        };
        self.bad_start(m, start)
    }

    #[inline(never)]
    fn annotated_stmt(&mut self, start: u32) -> StmtKind {
        if self.at_module_top() {
            self.late_definition();
            return StmtKind::Incomplete(Vec::new());
        }
        self.bad_start(msg::UNRECOGN_STMT, start)
    }

    #[inline(never)]
    fn assign_or_call(&mut self, start: u32, b: Block) -> StmtKind {
        if self.at_async_method() {
            return self.misplaced_method(start, b);
        }
        let target = match self.stmt_chain() {
            Ok(e) => {
                let span = e.span;
                match e.into_kind() {
                    ExprKind::Await(inner) => return StmtKind::Await(*inner),
                    kind => Expr { kind, span },
                }
            }
            Err(parts) => return StmtKind::Incomplete(parts),
        };
        match self.kind() {
            TokenKind::Eq => {
                self.bump();
                match self.expr(msg::EXP_EXPR) {
                    Ok(value) => StmtKind::Assign { target, value },
                    Err(parts) => StmtKind::Incomplete(prepend(target, parts)),
                }
            }
            _ if matches!(target.kind, ExprKind::Call { .. }) => StmtKind::Call(target),
            _ => {
                let at = self.tok().span.start;
                self.fail_at(msg::UNRECOGN_STMT, None, at);
                StmtKind::Incomplete(vec![target])
            }
        }
    }

    pub(super) fn expect_word(&mut self, kw: Kw, m: MsgId) -> bool {
        let t = self.tok();
        if t.kind == TokenKind::Kw(kw) {
            self.bump();
            return true;
        }
        self.fail_at(m, None, t.span.start);
        false
    }

    fn header_word(&mut self, kw: Kw, m: MsgId) -> bool {
        if self.kind() == TokenKind::Kw(kw) {
            self.bump();
            true
        } else {
            self.fail_after(m, None);
            false
        }
    }

    fn if_stmt(&mut self) -> StmtKind {
        if !self.enter() {
            return StmtKind::Incomplete(Vec::new());
        }
        let kw = self.tok().span.start;
        self.bump();
        let r = self.if_tail(kw);
        self.leave();
        r
    }

    fn if_tail(&mut self, kw: u32) -> StmtKind {
        let mut arms: Vec<(Expr, Vec<Stmt>)> = Vec::new();
        let mut arm_kw = vec![kw];
        let mut head = 0;
        loop {
            let cond = match self.expr(msg::EXP_EXPR) {
                Ok(c) => c,
                Err(parts) => return self.broken_arm(arms, arm_kw, parts, head),
            };
            if !self.header_word(Kw::Then, msg::EXP_THEN) {
                return self.broken_arm(arms, arm_kw, vec![cond], head);
            }
            let body = self.block(IF_PART);
            arms.push((cond, body));
            if self.failed() {
                return StmtKind::If {
                    arms,
                    else_body: None,
                    arm_kw,
                };
            }
            match self.kind() {
                TokenKind::Kw(Kw::ElsIf) => {
                    head = self.tok().span.start;
                    arm_kw.push(head);
                    self.bump();
                }
                TokenKind::Kw(Kw::Else) => break,
                _ => {
                    self.expect_word(Kw::EndIf, msg::EXP_ENDIF);
                    return StmtKind::If {
                        arms,
                        else_body: None,
                        arm_kw,
                    };
                }
            }
        }
        self.bump();
        let body = self.block(IF_PART);
        if !self.failed() {
            self.expect_word(Kw::EndIf, msg::EXP_ENDIF);
        }
        StmtKind::If {
            arms,
            else_body: Some(body),
            arm_kw,
        }
    }

    fn broken_arm(
        &self,
        mut arms: Vec<(Expr, Vec<Stmt>)>,
        mut arm_kw: Vec<u32>,
        parts: Vec<Expr>,
        head: u32,
    ) -> StmtKind {
        let Some((_, body)) = arms.last_mut() else {
            return StmtKind::Incomplete(parts);
        };
        if !parts.is_empty() {
            body.push(Stmt {
                kind: StmtKind::Incomplete(parts),
                span: Span {
                    start: head,
                    end: self.last_good_end.max(head),
                },
            });
        }
        arm_kw.truncate(arms.len());
        StmtKind::If {
            arms,
            else_body: None,
            arm_kw,
        }
    }

    fn loop_body(&mut self) -> Vec<Stmt> {
        self.loops = self.loops.saturating_add(1);
        let body = self.block(LOOP_BODY);
        self.loops = self.loops.saturating_sub(1);
        body
    }

    fn while_stmt(&mut self) -> StmtKind {
        if !self.enter() {
            return StmtKind::Incomplete(Vec::new());
        }
        self.bump();
        let r = self.while_tail();
        self.leave();
        r
    }

    fn while_tail(&mut self) -> StmtKind {
        let cond = match self.expr(msg::EXP_EXPR) {
            Ok(c) => c,
            Err(parts) => return StmtKind::Incomplete(parts),
        };
        if !self.header_word(Kw::Do, msg::EXP_DO) {
            return StmtKind::Incomplete(vec![cond]);
        }
        let body = self.loop_body();
        if !self.failed() {
            self.expect_word(Kw::EndDo, msg::EXP_ENDDO);
        }
        StmtKind::While { cond, body }
    }

    fn for_stmt(&mut self) -> StmtKind {
        if !self.enter() {
            return StmtKind::Incomplete(Vec::new());
        }
        self.bump();
        let r = if self.kind() == TokenKind::Kw(Kw::Each) {
            self.bump();
            self.for_each_tail()
        } else {
            self.for_tail()
        };
        self.leave();
        r
    }

    fn loop_var(&mut self) -> Option<Ident> {
        let t = self.tok();
        if t.kind != TokenKind::Ident {
            self.fail_after(msg::EXP_IDENT, None);
            return None;
        }
        self.bump();
        Some(self.ident(t))
    }

    fn for_tail(&mut self) -> StmtKind {
        let Some(var) = self.loop_var() else {
            return StmtKind::Incomplete(Vec::new());
        };
        if self.kind() != TokenKind::Eq {
            self.fail_after(msg::EXP_EQ, None);
            return StmtKind::Incomplete(vec![name_expr(&var)]);
        }
        self.bump();
        let from = match self.expr(msg::EXP_EXPR) {
            Ok(e) => e,
            Err(parts) => return StmtKind::Incomplete(prepend(name_expr(&var), parts)),
        };
        if !self.header_word(Kw::To, msg::EXP_TO) {
            return StmtKind::Incomplete(vec![name_expr(&var), from]);
        }
        let to = match self.expr(msg::EXP_EXPR) {
            Ok(e) => e,
            Err(parts) => {
                let mut v = vec![name_expr(&var), from];
                v.extend(parts);
                return StmtKind::Incomplete(v);
            }
        };
        if !self.header_word(Kw::Do, msg::EXP_DO) {
            return StmtKind::Incomplete(vec![name_expr(&var), from, to]);
        }
        let body = self.loop_body();
        if !self.failed() {
            self.expect_word(Kw::EndDo, msg::EXP_ENDDO);
        }
        StmtKind::For {
            var,
            from,
            to,
            body,
        }
    }

    fn for_each_tail(&mut self) -> StmtKind {
        let Some(var) = self.loop_var() else {
            return StmtKind::Incomplete(Vec::new());
        };
        if !self.header_word(Kw::In, msg::EXP_IN) {
            return StmtKind::Incomplete(vec![name_expr(&var)]);
        }
        let collection = match self.expr(msg::EXP_EXPR) {
            Ok(e) => e,
            Err(parts) => return StmtKind::Incomplete(prepend(name_expr(&var), parts)),
        };
        if !self.header_word(Kw::Do, msg::EXP_DO) {
            return StmtKind::Incomplete(vec![name_expr(&var), collection]);
        }
        let body = self.loop_body();
        if !self.failed() {
            self.expect_word(Kw::EndDo, msg::EXP_ENDDO);
        }
        StmtKind::ForEach {
            var,
            collection,
            body,
        }
    }

    fn try_stmt(&mut self) -> StmtKind {
        if !self.enter() {
            return StmtKind::Incomplete(Vec::new());
        }
        self.bump();
        let r = self.try_tail();
        self.leave();
        r
    }

    fn try_tail(&mut self) -> StmtKind {
        let body = self.block(TRY_BODY);
        if self.failed() || !self.expect_word(Kw::Except, msg::EXP_EXCEPT) {
            return StmtKind::Try {
                body,
                handler: Vec::new(),
            };
        }
        self.handlers = self.handlers.saturating_add(1);
        let handler = self.block(TRY_HANDLER);
        self.handlers = self.handlers.saturating_sub(1);
        if !self.failed() {
            self.expect_word(Kw::EndTry, msg::EXP_ENDTRY);
        }
        StmtKind::Try { body, handler }
    }

    fn loop_jump(&mut self, kind: StmtKind, m: MsgId) -> StmtKind {
        self.bump();
        if self.loops == 0 {
            self.fail_after(m, None);
            return StmtKind::Incomplete(Vec::new());
        }
        kind
    }

    #[inline(never)]
    fn return_stmt(&mut self, b: Block) -> StmtKind {
        self.bump();
        let Some(kind) = self.method else {
            self.fail_after(msg::RETURN_INVALID, None);
            return StmtKind::Incomplete(Vec::new());
        };
        if kind == MethodKind::Procedure && self.at_stmt_end(b.ends) {
            return StmtKind::Return(None);
        }
        let value = match self.expr(msg::EXP_EXPR) {
            Ok(v) => v,
            Err(parts) => return StmtKind::Incomplete(parts),
        };
        if kind == MethodKind::Procedure {
            self.fail_after(msg::PROC_RETVAL, None);
            return StmtKind::Incomplete(vec![value]);
        }
        StmtKind::Return(Some(value))
    }

    #[inline(never)]
    fn raise_stmt(&mut self, ends: &[Kw]) -> StmtKind {
        self.bump();
        if self.at_stmt_end(ends) {
            if self.handlers == 0 {
                self.fail_after(msg::RAISE_INVALID, None);
                return StmtKind::Incomplete(Vec::new());
            }
            return StmtKind::Raise(RaiseForm::Rethrow);
        }
        if self.kind() == TokenKind::LParen {
            return match self.args() {
                Ok((args, _)) => StmtKind::Raise(RaiseForm::Call(args)),
                Err(parts) => StmtKind::Incomplete(parts),
            };
        }
        match self.expr(msg::EXP_EXPR) {
            Ok(e) => StmtKind::Raise(RaiseForm::Expr(e)),
            Err(parts) => StmtKind::Incomplete(parts),
        }
    }

    #[inline(never)]
    fn execute_stmt(&mut self) -> StmtKind {
        self.bump();
        match self.expr(msg::EXP_EXPR) {
            Ok(e) => StmtKind::Execute(unparen(e)),
            Err(parts) => StmtKind::Incomplete(parts),
        }
    }

    #[inline(never)]
    fn handler_stmt(&mut self, add: bool) -> StmtKind {
        self.bump();
        let event = match self.expr(msg::EXP_EXPR) {
            Ok(e) => e,
            Err(parts) => return StmtKind::Incomplete(parts),
        };
        if self.kind() != TokenKind::Comma {
            self.fail_after(msg::EXP_COMMA, None);
            return StmtKind::Incomplete(vec![event]);
        }
        self.bump();
        let handler = match self.expr(msg::EXP_EXPR) {
            Ok(h) => h,
            Err(parts) => return StmtKind::Incomplete(prepend(event, parts)),
        };
        if add {
            StmtKind::AddHandler { event, handler }
        } else {
            StmtKind::RemoveHandler { event, handler }
        }
    }

    #[inline(never)]
    fn label(&mut self) -> StmtKind {
        let name = self.label_name();
        if self.kind() != TokenKind::Colon {
            self.fail_after(msg::EXP_COLON, None);
            return StmtKind::Incomplete(Vec::new());
        }
        self.bump();
        self.mark(false, &name);
        StmtKind::Label(name)
    }

    #[inline(never)]
    fn goto_stmt(&mut self) -> StmtKind {
        self.bump();
        if self.kind() != TokenKind::Tilde {
            self.fail_after(msg::EXP_LABEL, None);
            return StmtKind::Incomplete(Vec::new());
        }
        let name = self.label_name();
        self.mark(true, &name);
        StmtKind::Goto(name)
    }

    fn label_name(&mut self) -> Ident {
        let tilde = self.tok().span;
        self.bump();
        let t = self.tok();
        if matches!(t.kind, TokenKind::Ident | TokenKind::Kw(_)) && t.span.start == tilde.end {
            self.bump();
            return self.ident(t);
        }
        Ident {
            name: String::new(),
            span: Span {
                start: tilde.end,
                end: tilde.end,
            },
        }
    }

    fn mark(&mut self, goto: bool, name: &Ident) {
        let st = &mut self.body;
        let m = Mark {
            key: fold_ident(&name.name),
            written: format!("~{}", name.name),
            block: st.open.last().copied().unwrap_or(NO_BLOCK),
        };
        if goto {
            st.gotos.push(m);
        } else {
            st.labels.push(m);
        }
    }

    pub(crate) fn finish_labels(&mut self, close: u32) {
        if !self.failed()
            && let Some((m, arg)) = self.body.label_error()
        {
            self.fail_at(m, Some(arg), close);
        }
        let st = &mut self.body;
        st.open.clear();
        st.parents.clear();
        st.labels.clear();
        st.gotos.clear();
    }
}
