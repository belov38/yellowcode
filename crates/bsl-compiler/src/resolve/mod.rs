mod calls;
mod expr;
mod globals;
mod new;
mod scope;

pub(crate) use calls::Arity;
pub(crate) use globals::{Globals, Platform};
pub(crate) use scope::{Labels, Scope};

use crate::error::CompileError;
use crate::ir::{
    Callee, Const, IrArm, IrBody, IrExpr, IrExprKind, IrMethod, IrModule, IrParam, IrStmt,
    IrStmtKind, IrVar, Place,
};
use crate::options::CompileOptions;
use crate::split::{MethodRole, ModuleView, directive_of, intercept_of};
use crate::sym::{Interner, Sym};
use bsl_prim::{BslDate, Number};
use bsl_syntax::ast::{self, Expr, ExprKind, Ident, Literal, RaiseForm, StmtKind};
use bsl_syntax::{MsgId, SourceText, Span};
use calls::Methods;
use std::collections::HashMap;
use std::sync::Arc;

const VAR_NOTDECLARED: MsgId = MsgId("IDS_STX_VAR_NOTDECLARED");
const PROC_NOTDEFINED: MsgId = MsgId("IDS_STX_PROC_NOTDEFINED");
const PROC_DEFINED: MsgId = MsgId("IDS_STX_PROC_DEFINED");
const PARAMS_TOOMANY: MsgId = MsgId("IDS_STX_PARAMS_TOOMANY");
const PARAMS_NOTENOUGH: MsgId = MsgId("IDS_STX_PARAMS_NOTENOUGH");
const PROC_ASFUNC: MsgId = MsgId("IDS_STX_PROC_ASFUNC");
const TYPE_NOTDEFINED: MsgId = MsgId("IDS_STX_TYPE_NOTDEFINED");
const CTOR_NOTFOUND: MsgId = MsgId("IDS_STX_CTOR_NOTFOUND");
const NEW_NOTOBJECT: MsgId = MsgId("IDS_STX_NEW_NOTOBJECT");
const ASYNC_INVALID: MsgId = MsgId("IDS_STX_ASYNC_INVALID");

pub(crate) trait GlobalLookup {
    fn value(&mut self, name: &ast::Ident, sym: &Sym) -> Option<Place>;
    fn method(&mut self, name: &ast::Ident, sym: &Sym) -> Option<(Callee, Arity)>;
}

pub(crate) fn select_error(
    syntax: Option<CompileError>,
    checks: &[CompileError],
    unknown_methods: &[CompileError],
) -> Option<CompileError> {
    let first = |list: &[CompileError]| list.iter().min_by_key(|e| (e.line, e.col)).cloned();
    syntax
        .or_else(|| first(checks))
        .or_else(|| first(unknown_methods))
}

pub(crate) struct Resolved {
    pub module: IrModule,
    pub checks: Vec<CompileError>,
    pub unknown: Vec<CompileError>,
}

impl Resolved {
    pub(crate) fn skipped(o: &CompileOptions<'_>) -> Resolved {
        Resolved {
            module: shell(o, Arc::from(Vec::new())),
            checks: Vec::new(),
            unknown: Vec::new(),
        }
    }
}

fn shell(o: &CompileOptions<'_>, source_lines: Arc<[Arc<[u16]>]>) -> IrModule {
    IrModule {
        name: o.module_name.to_string(),
        kind: o.kind,
        context: o.context,
        compat: o.compat,
        vars: Vec::new(),
        methods: Vec::new(),
        body: None,
        source_lines,
        outer: Vec::new(),
        caller_methods: Vec::new(),
        caller_vars: Vec::new(),
    }
}

pub(crate) fn resolve(
    src: &SourceText,
    o: &CompileOptions<'_>,
    v: &ModuleView<'_>,
    globals: &mut dyn GlobalLookup,
) -> Resolved {
    let mut r = Resolver::new(src, Platform::new(o), globals);
    let vars = r.module_vars(&v.vars);
    let (methods, dups) = Methods::build(&v.methods, &mut r.interner);
    r.methods = methods;
    r.report_duplicates(&dups);
    let methods = v
        .methods
        .iter()
        .map(|&(m, role)| r.method(m, role))
        .collect();
    let body = (!v.body.is_empty()).then(|| r.body([], v.body.iter().copied()));
    let lines: Vec<Arc<[u16]>> = (1..=src.line_count())
        .map(|l| Arc::from(src.line_units(l)))
        .collect();
    let mut module = shell(o, Arc::from(lines));
    module.vars = vars;
    module.methods = methods;
    module.body = body;
    Resolved {
        module,
        checks: r.checks,
        unknown: r.unknown,
    }
}

pub(crate) fn resolve_fragment(
    src: &SourceText,
    o: &CompileOptions<'_>,
    stmts: &[ast::Stmt],
    globals: &mut dyn GlobalLookup,
) -> Resolved {
    let mut r = Resolver::new(src, Platform::new(o), globals);
    let body = r.body([], stmts);
    let lines: Vec<Arc<[u16]>> = (1..=src.line_count())
        .map(|l| Arc::from(src.line_units(l)))
        .collect();
    let mut module = shell(o, Arc::from(lines));
    module.body = Some(body);
    Resolved {
        module,
        checks: r.checks,
        unknown: r.unknown,
    }
}

pub(crate) struct Resolver<'a> {
    src: &'a SourceText,
    platform: Platform<'a>,
    globals: &'a mut dyn GlobalLookup,
    interner: Interner,
    empty: Sym,
    modvars: HashMap<Sym, u32>,
    methods: Methods,
    scope: Scope,
    labels: Labels,
    checks: Vec<CompileError>,
    unknown: Vec<CompileError>,
}

fn hole(span: Span) -> IrExpr {
    IrExpr {
        kind: IrExprKind::Const(Const::Undefined),
        span,
    }
}

impl<'a> Resolver<'a> {
    pub(crate) fn new(
        src: &'a SourceText,
        platform: Platform<'a>,
        globals: &'a mut dyn GlobalLookup,
    ) -> Resolver<'a> {
        let mut interner = Interner::new();
        let empty = interner.sym("");
        Resolver {
            src,
            platform,
            globals,
            interner,
            empty,
            modvars: HashMap::new(),
            methods: Methods::default(),
            scope: Scope::default(),
            labels: Labels::default(),
            checks: Vec::new(),
            unknown: Vec::new(),
        }
    }

    fn error(&self, msg: MsgId, id: &Ident, at: u32) -> CompileError {
        let p = self.src.pos(at);
        CompileError {
            msg,
            arg: Some(id.name.clone()),
            line: p.line,
            col: p.col,
        }
    }

    fn check(&mut self, msg: MsgId, id: &Ident) {
        self.check_at(msg, id, id.span.start);
    }

    fn check_at(&mut self, msg: MsgId, id: &Ident, at: u32) {
        let e = self.error(msg, id, at);
        self.checks.push(e);
    }

    fn check_bare(&mut self, msg: MsgId, at: u32) {
        let p = self.src.pos(at);
        self.checks.push(CompileError {
            msg,
            arg: None,
            line: p.line,
            col: p.col,
        });
    }

    fn module_vars(&mut self, decls: &[&ast::VarDecl]) -> Vec<IrVar> {
        let mut vars = Vec::new();
        for d in decls {
            let directive = directive_of(&d.annotations);
            for item in &d.items {
                let name = self.interner.sym(&item.name.name);
                let i = u32::try_from(vars.len()).unwrap_or(u32::MAX);
                self.modvars.entry(name.clone()).or_insert(i);
                vars.push(IrVar {
                    name,
                    export: item.export,
                    directive,
                });
            }
        }
        vars
    }

    fn method(&mut self, m: &ast::Method, role: MethodRole) -> IrMethod {
        if role == MethodRole::Compiled
            && let Some(kw) = m.async_kw
            && !self.platform.ctx.is_client()
        {
            self.check_bare(ASYNC_INVALID, kw.start);
        }
        let params = m
            .params
            .iter()
            .map(|p| IrParam {
                name: self.interner.sym(&p.name.name),
                by_val: p.by_val,
                default: p.default.as_ref().map(constant),
            })
            .collect();
        let body = match role {
            MethodRole::CallableOnly => IrBody {
                slots: Vec::new(),
                stmts: Vec::new(),
            },
            MethodRole::Compiled => {
                let names = m.params.iter().map(|p| &p.name);
                let locals = m.vars.iter().flat_map(|v| &v.items).map(|i| &i.name);
                self.body(names.chain(locals), &m.body)
            }
        };
        IrMethod {
            name: self.interner.sym(&m.name.name),
            kind: m.kind,
            export: m.export,
            is_async: m.async_kw.is_some(),
            directive: directive_of(&m.annotations),
            intercept: intercept_of(&m.annotations),
            params,
            body,
            line: self.src.pos(m.span.start).line,
            callable_only: role == MethodRole::CallableOnly,
        }
    }

    fn body<'i, 's>(
        &mut self,
        names: impl IntoIterator<Item = &'i Ident>,
        stmts: impl IntoIterator<Item = &'s ast::Stmt>,
    ) -> IrBody {
        self.scope = Scope::default();
        self.labels = Labels::default();
        for n in names {
            let s = self.interner.sym(&n.name);
            self.scope.declare(s);
        }
        let stmts = self.block(stmts);
        IrBody {
            slots: std::mem::take(&mut self.scope).into_slots(),
            stmts,
        }
    }

    fn block<'s>(&mut self, list: impl IntoIterator<Item = &'s ast::Stmt>) -> Vec<IrStmt> {
        let mut out = Vec::new();
        for s in list {
            self.stmt(s, &mut out);
        }
        out
    }

    fn stmt(&mut self, s: &ast::Stmt, out: &mut Vec<IrStmt>) {
        let line = self.src.pos(s.span.start).line;
        let kind = match &s.kind {
            StmtKind::Assign { target, value } => match &target.kind {
                ExprKind::Name(id) => {
                    let target = self.assign_place(id);
                    IrStmtKind::Assign {
                        target,
                        value: self.expr(value),
                    }
                }
                ExprKind::Member { object, name } => {
                    let target = self.member_place(object, name);
                    IrStmtKind::Assign {
                        target,
                        value: self.expr(value),
                    }
                }
                ExprKind::Index { object, index } => {
                    let target = self.index_place(object, index);
                    IrStmtKind::Assign {
                        target,
                        value: self.expr(value),
                    }
                }
                _ => {
                    let call = self.expr_with(target, false);
                    out.push(IrStmt {
                        kind: IrStmtKind::Eval(call),
                        line,
                        span: s.span,
                    });
                    IrStmtKind::Eval(self.expr(value))
                }
            },
            StmtKind::Call(e) => IrStmtKind::Eval(self.expr_with(e, false)),
            StmtKind::If {
                arms,
                else_body,
                arm_kw,
            } => {
                let arms = arms
                    .iter()
                    .enumerate()
                    .map(|(i, (cond, body))| {
                        let at = arm_kw.get(i).copied().unwrap_or(cond.span.start);
                        IrArm {
                            line: self.src.pos(at).line,
                            cond: self.expr(cond),
                            body: self.block(body),
                        }
                    })
                    .collect();
                let otherwise = match else_body {
                    Some(b) => self.block(b),
                    None => Vec::new(),
                };
                IrStmtKind::If { arms, otherwise }
            }
            StmtKind::While { cond, body } => IrStmtKind::While {
                cond: self.expr(cond),
                body: self.block(body),
            },
            StmtKind::For {
                var,
                from,
                to,
                body,
            } => {
                let var = self.assign_place(var);
                let from = self.expr(from);
                let to = self.expr(to);
                let bound_slot = self.scope.hidden(self.empty.clone());
                IrStmtKind::For {
                    var,
                    from,
                    to,
                    bound_slot,
                    body: self.block(body),
                }
            }
            StmtKind::ForEach {
                var,
                collection,
                body,
            } => {
                let var = self.assign_place(var);
                let collection = self.expr(collection);
                let iter_slot = self.scope.hidden(self.empty.clone());
                IrStmtKind::ForEach {
                    var,
                    collection,
                    iter_slot,
                    body: self.block(body),
                }
            }
            StmtKind::Break => IrStmtKind::Break,
            StmtKind::Continue => IrStmtKind::Continue,
            StmtKind::Return(e) => IrStmtKind::Return(e.as_ref().map(|e| self.expr(e))),
            StmtKind::Try { body, handler } => IrStmtKind::Try {
                body: self.block(body),
                handler: self.block(handler),
            },
            StmtKind::Raise(RaiseForm::Rethrow) => IrStmtKind::Reraise,
            StmtKind::Raise(RaiseForm::Expr(e)) => IrStmtKind::Raise(self.expr(e)),
            StmtKind::Raise(RaiseForm::Call(args)) => IrStmtKind::RaiseCall(self.args(args)),
            StmtKind::Goto(l) => IrStmtKind::Goto(self.labels.id(&l.name)),
            StmtKind::Label(l) => IrStmtKind::Label(self.labels.id(&l.name)),
            StmtKind::Execute(e) => IrStmtKind::Execute(self.expr(e)),
            StmtKind::AddHandler { event, handler } => IrStmtKind::AddHandler {
                event: self.expr(event),
                handler: self.expr(handler),
            },
            StmtKind::RemoveHandler { event, handler } => IrStmtKind::RemoveHandler {
                event: self.expr(event),
                handler: self.expr(handler),
            },
            StmtKind::Await(e) => IrStmtKind::Await(self.expr(e)),
            StmtKind::Value(e) => IrStmtKind::Return(Some(self.expr(e))),
            StmtKind::Incomplete(_) => return,
        };
        out.push(IrStmt {
            kind,
            line,
            span: s.span,
        });
    }

    fn lookup(&mut self, id: &Ident) -> (Sym, Option<Place>) {
        let sym = self.interner.sym(&id.name);
        let place = if let Some(i) = self.scope.get(&sym) {
            Some(Place::Local(i))
        } else if let Some(&i) = self.modvars.get(&sym) {
            Some(Place::ModVar(i))
        } else {
            self.globals.value(id, &sym)
        };
        (sym, place)
    }

    fn read(&mut self, id: &Ident) -> Option<Place> {
        let (_, place) = self.lookup(id);
        if place.is_none() {
            self.check(VAR_NOTDECLARED, id);
        }
        place
    }

    fn assign_place(&mut self, id: &Ident) -> Place {
        match self.lookup(id) {
            (_, Some(p)) => p,
            (sym, None) => Place::Local(self.scope.declare(sym)),
        }
    }

    fn member_place(&mut self, object: &Expr, name: &Ident) -> Place {
        Place::Member {
            object: Box::new(self.expr(object)),
            name: self.interner.sym(&name.name),
        }
    }

    fn index_place(&mut self, object: &Expr, index: &Expr) -> Place {
        let object = self.expr(object);
        Place::Index {
            object: Box::new(object),
            index: Box::new(self.expr(index)),
        }
    }
}

pub(crate) fn constant(l: &Literal) -> Const {
    match l {
        Literal::Number(s) => Const::Number(Number::parse(s).unwrap_or_else(Number::zero)),
        Literal::Str(u) => Const::String(Arc::from(&**u)),
        Literal::Date(d) => Const::Date(
            BslDate::from_parts(
                u32::from(d.year),
                u32::from(d.month),
                u32::from(d.day),
                u32::from(d.hour),
                u32::from(d.minute),
                u32::from(d.second),
            )
            .unwrap_or(BslDate::EMPTY),
        ),
        Literal::Bool(b) => Const::Bool(*b),
        Literal::Undefined => Const::Undefined,
        Literal::Null => Const::Null,
    }
}
