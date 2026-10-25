use std::collections::HashSet;

use super::Parser;
use super::stmt::{Block, FUNC_BODY, PROC_BODY};
use crate::ast::{Annotation, Literal, Method, MethodKind, Module, Param, VarDecl, VarItem};
use crate::diag::msg;
use crate::fold::fold_ident;
use crate::keywords::{Kw, ident_is};
use crate::lexer::TokenKind;
use crate::preproc::Origin;
use crate::source::Span;

const DIRECTIVES: [[&str; 2]; 5] = [
    ["наклиенте", "atclient"],
    ["насервере", "atserver"],
    ["насерверебезконтекста", "atservernocontext"],
    [
        "наклиентенасерверебезконтекста",
        "atclientatservernocontext",
    ],
    ["наклиентенасервере", "atclientatserver"],
];

const EXT_ANNOTATIONS: [[&str; 2]; 4] = [
    ["перед", "before"],
    ["после", "after"],
    ["вместо", "around"],
    ["изменениеиконтроль", "changeandvalidate"],
];

fn is_method_kw(k: TokenKind) -> bool {
    matches!(k, TokenKind::Kw(Kw::Procedure | Kw::Function))
}

impl Parser<'_> {
    pub(super) fn module(&mut self, methods_only: bool) -> Module {
        let mut m = Module::default();
        let mut names = HashSet::new();
        while !self.failed() {
            match self.kind() {
                TokenKind::Semicolon => {
                    self.bump();
                    continue;
                }
                TokenKind::Eof => break,
                _ => {}
            }
            let annotations = self.annotations();
            if self.failed() {
                break;
            }
            let t = self.tok();
            match t.kind {
                TokenKind::Kw(Kw::Var) if methods_only => {
                    self.fail_at(msg::LIBRARY_MODULE, None, t.span.start);
                }
                TokenKind::Kw(Kw::Var) if !m.methods.is_empty() => {
                    self.fail_at(msg::LATE_DECLARE, None, t.span.start);
                }
                TokenKind::Kw(Kw::Var) => {
                    let d = self.var_decl(annotations, &mut names, false);
                    m.vars.push(d);
                }
                _ if self.at_method() => {
                    if let Some(meth) = self.method(annotations) {
                        m.methods.push(meth);
                    }
                }
                _ if !annotations.is_empty() => self.fail_after(msg::EXP_VARPROCFUNC, None),
                _ if methods_only && m.methods.is_empty() => {
                    self.fail_at(msg::LIBRARY_MODULE, None, t.span.start);
                }
                _ if methods_only => self.fail_after(msg::EXP_EOT, None),
                _ => {
                    self.module_body = true;
                    m.body = self.text_body();
                    self.module_body = false;
                    break;
                }
            }
        }
        m
    }

    fn at_method(&self) -> bool {
        is_method_kw(self.kind()) || self.at_async_method()
    }

    pub(super) fn at_async_method(&self) -> bool {
        let t = self.tok();
        is_method_kw(self.kind_at(1))
            && t.kind == TokenKind::Ident
            && t.span.end.saturating_sub(t.span.start) == 5
            && {
                let u = self.units(t.span);
                ident_is(u, "асинх") || ident_is(u, "async")
            }
    }

    pub(super) fn late_definition(&mut self) {
        self.annotations();
        if self.failed() {
            return;
        }
        let t = self.tok();
        if t.kind == TokenKind::Kw(Kw::Var) {
            self.fail_at(msg::LATE_DECLARE, None, t.span.start);
        } else if self.at_method() {
            self.fail_at(msg::LATE_PROCFUNC, None, t.span.start);
        } else {
            self.fail_after(msg::EXP_VARPROCFUNC, None);
        }
    }

    fn annotations(&mut self) -> Vec<Annotation> {
        let mut out = Vec::new();
        while self.kind() == TokenKind::Amp && !self.failed() {
            match self.annotation() {
                Some(a) => out.push(a),
                None => break,
            }
        }
        out
    }

    fn annotation(&mut self) -> Option<Annotation> {
        let amp = self.tok().span;
        self.bump();
        let t = self.tok();
        if !matches!(t.kind, TokenKind::Ident | TokenKind::Kw(_)) || t.span.start != amp.end {
            self.fail_after(msg::EXP_IDENT, None);
            return None;
        }
        let name = self.ident(t);
        let units = self.units(t.span);
        let known = DIRECTIVES
            .iter()
            .chain(&EXT_ANNOTATIONS)
            .flatten()
            .any(|a| ident_is(units, a));
        if t.kind != TokenKind::Ident || !known {
            self.fail_at(msg::UNKNOWN_ANNOT, Some(name.name), t.span.start);
            return None;
        }
        self.bump();
        let mut arg = None;
        if self.kind() == TokenKind::LParen {
            self.bump();
            let TokenKind::Str(i) = self.kind() else {
                self.fail_after(msg::EXP_CONST, None);
                return None;
            };
            self.bump();
            arg = Some(self.take_str(i));
            if self.kind() != TokenKind::RParen {
                self.fail_after(msg::EXP_RPAR, None);
                return None;
            }
            self.bump();
        }
        Some(Annotation {
            name,
            arg,
            span: Span {
                start: amp.start,
                end: self.last_good_end,
            },
        })
    }

    fn directive_set(&self, annotations: &[Annotation]) -> u8 {
        let mut set = 0u8;
        for a in annotations {
            let units = self.units(a.name.span);
            if let Some(i) = DIRECTIVES
                .iter()
                .position(|d| d.iter().any(|n| ident_is(units, n)))
            {
                set |= 1 << i;
            }
        }
        set
    }

    fn var_decl(
        &mut self,
        annotations: Vec<Annotation>,
        names: &mut HashSet<(String, u8)>,
        local: bool,
    ) -> VarDecl {
        let directives = self.directive_set(&annotations);
        let start = self.tok().span.start;
        self.bump();
        let mut items = Vec::new();
        loop {
            let t = self.tok();
            if t.kind != TokenKind::Ident {
                self.fail_after(msg::EXP_VARNAME, None);
                break;
            }
            let name = self.ident(t);
            if !names.insert((fold_ident(&name.name), directives)) {
                self.fail_at(msg::VAR_DEFINED, Some(name.name), t.span.start);
                break;
            }
            self.bump();
            let export = self.kind() == TokenKind::Kw(Kw::Export);
            if export {
                self.bump();
                if local {
                    self.fail_after(msg::EXPORT_LOCAL, None);
                }
            }
            items.push(VarItem { name, export });
            if self.failed() {
                break;
            }
            match self.kind() {
                TokenKind::Comma => self.bump(),
                TokenKind::Semicolon => {
                    self.bump();
                    break;
                }
                _ => {
                    self.fail_after(msg::EXP_SEMICOLON, None);
                    break;
                }
            }
        }
        VarDecl {
            annotations,
            items,
            span: Span {
                start,
                end: self.last_good_end,
            },
        }
    }

    fn method(&mut self, annotations: Vec<Annotation>) -> Option<Method> {
        let first = self.tok();
        let mut async_kw = None;
        if first.kind == TokenKind::Ident {
            async_kw = Some(first.span);
            self.bump();
        }
        let kw_at = self.pos;
        let (kind, name_msg, end_word, body) = match self.kind() {
            TokenKind::Kw(Kw::Function) => (
                MethodKind::Function,
                msg::EXP_FUNCNAME,
                Kw::EndFunction,
                FUNC_BODY,
            ),
            _ => (
                MethodKind::Procedure,
                msg::EXP_PROCNAME,
                Kw::EndProcedure,
                PROC_BODY,
            ),
        };
        self.bump();
        let t = self.tok();
        if t.kind != TokenKind::Ident {
            self.fail_after(name_msg, None);
            return None;
        }
        self.bump();
        let mut m = Method {
            annotations,
            async_kw,
            kind,
            name: self.ident(t),
            params: Vec::new(),
            export: false,
            vars: Vec::new(),
            body: Vec::new(),
            span: Span {
                start: first.span.start,
                end: first.span.start,
            },
            end_kw: None,
            base_tokens: None,
            incomplete: true,
        };
        if self.builtins.builtin(&m.name.name).is_some() {
            self.fail_at(name_msg, None, t.span.start);
        } else {
            self.method_rest(&mut m, end_word, body, kw_at);
        }
        m.span.end = self.last_good_end.max(m.span.start);
        m.incomplete = m.end_kw.is_none();
        Some(m)
    }

    fn method_rest(&mut self, m: &mut Method, end_word: Kw, body: Block, kw_at: usize) {
        let mut names = HashSet::new();
        if self.kind() != TokenKind::LParen {
            self.fail_after(msg::EXP_FORMPARAMS, None);
            return;
        }
        self.bump();
        if !self.params(&mut m.params, &mut names) {
            return;
        }
        if self.kind() == TokenKind::Kw(Kw::Export) {
            self.bump();
            m.export = true;
        }
        self.method = Some(m.kind);
        self.is_async = m.async_kw.is_some();
        while !self.failed() {
            match self.kind() {
                TokenKind::Semicolon => self.bump(),
                TokenKind::Kw(Kw::Var) => {
                    let d = self.var_decl(Vec::new(), &mut names, true);
                    m.vars.push(d);
                }
                _ => break,
            }
        }
        if !self.failed() {
            m.body = self.block(body);
        }
        let end = self.tok();
        if !self.failed() && end.kind == TokenKind::Kw(end_word) {
            self.finish_labels(end.span.start);
            if !self.failed() {
                let end_at = self.pos;
                self.bump();
                m.end_kw = Some(end.span);
                m.base_tokens = self.base_tokens(kw_at, end_at);
            }
        } else {
            if !self.failed() {
                self.expect_word(end_word, body.expect);
            }
            self.finish_labels(end.span.start);
        }
        self.method = None;
        self.is_async = false;
    }

    fn params(&mut self, out: &mut Vec<Param>, names: &mut HashSet<(String, u8)>) -> bool {
        if self.kind() == TokenKind::RParen {
            self.bump();
            return true;
        }
        loop {
            let start = self.tok().span.start;
            let by_val = self.kind() == TokenKind::Kw(Kw::Val);
            if by_val {
                self.bump();
            }
            let t = self.tok();
            if t.kind != TokenKind::Ident {
                let m = if by_val {
                    msg::EXP_FORMPARAMNAME
                } else {
                    msg::EXP_FORMPARAM
                };
                self.fail_after(m, None);
                return false;
            }
            let name = self.ident(t);
            if !names.insert((fold_ident(&name.name), 0)) {
                self.fail_at(msg::FORMPARAM_DEFINED, Some(name.name), t.span.start);
                return false;
            }
            self.bump();
            let default = if self.kind() == TokenKind::Eq {
                self.bump();
                match self.constant() {
                    Some(l) => Some(l),
                    None => return false,
                }
            } else {
                None
            };
            out.push(Param {
                by_val,
                name,
                default,
                span: Span {
                    start,
                    end: self.last_good_end,
                },
            });
            match self.kind() {
                TokenKind::Comma => self.bump(),
                TokenKind::RParen => {
                    self.bump();
                    return true;
                }
                _ => {
                    self.fail_after(msg::EXP_RPAR, None);
                    return false;
                }
            }
        }
    }

    fn constant(&mut self) -> Option<Literal> {
        let t = self.tok();
        let lit = match t.kind {
            TokenKind::Number => Literal::Number(self.src.text(t.span)),
            TokenKind::Str(i) => Literal::Str(self.take_str(i)),
            TokenKind::Date(d) => Literal::Date(d),
            TokenKind::Kw(Kw::True) => Literal::Bool(true),
            TokenKind::Kw(Kw::False) => Literal::Bool(false),
            TokenKind::Kw(Kw::Undefined) => Literal::Undefined,
            TokenKind::Kw(Kw::Null) => Literal::Null,
            TokenKind::Minus | TokenKind::Plus => {
                self.bump();
                let n = self.tok();
                if n.kind != TokenKind::Number {
                    self.fail_after(msg::EXP_CONST, None);
                    return None;
                }
                self.bump();
                let digits = self.src.text(n.span);
                return Some(Literal::Number(if t.kind == TokenKind::Minus {
                    format!("-{digits}")
                } else {
                    digits
                }));
            }
            _ => {
                self.fail_after(msg::EXP_CONST, None);
                return None;
            }
        };
        self.bump();
        Some(lit)
    }

    fn take_str(&mut self, i: u32) -> Box<[u16]> {
        self.strings
            .get_mut(i as usize)
            .map(std::mem::take)
            .unwrap_or_default()
    }

    fn base_tokens(&self, first: usize, last: usize) -> Option<Vec<Span>> {
        let toks = self.toks.get(first..=last)?;
        let origin = self.origin.get(first..=last)?;
        let lo = self.deleted.partition_point(|d| d.0 as usize <= first);
        let hi = self.deleted.partition_point(|d| d.0 as usize <= last);
        let deleted = self.deleted.get(lo..hi)?;
        if deleted.is_empty() && !origin.contains(&Origin::Inserted) {
            return None;
        }
        let mut out = Vec::with_capacity(toks.len());
        let mut del = deleted.iter().peekable();
        for (i, (t, o)) in (first..).zip(toks.iter().zip(origin)) {
            while let Some((_, ts)) = del.next_if(|d| d.0 as usize == i) {
                out.extend(ts.iter().map(|t| t.span));
            }
            if *o != Origin::Inserted {
                out.push(t.span);
            }
        }
        Some(out)
    }
}
