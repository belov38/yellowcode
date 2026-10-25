use super::{Resolver, constant, hole};
use crate::ir::{Callee, IrArg, IrExpr, IrExprKind, Place};
use bsl_syntax::Span;
use bsl_syntax::ast::{Arg, BinOp, Expr, ExprKind, Ident, UnOp};
use platform_catalog::TypeIdx;

pub(super) enum Task<'e> {
    Expr(&'e Expr, bool),
    Arg(&'e Arg),
    Build(Build<'e>),
}

pub(super) enum Build<'e> {
    Unary {
        op: UnOp,
        span: Span,
    },
    Binary {
        op: BinOp,
        span: Span,
    },
    Ternary {
        span: Span,
    },
    Await {
        span: Span,
    },
    Evaluate {
        span: Span,
    },
    Member {
        name: &'e Ident,
        span: Span,
    },
    Index {
        span: Span,
    },
    CallMember {
        name: &'e Ident,
        n: usize,
        want_value: bool,
        span: Span,
    },
    Call {
        callee: Callee,
        n: usize,
        want_value: bool,
        span: Span,
    },
    New {
        ty: TypeIdx,
        n: Option<usize>,
        span: Span,
    },
    NewDynamic {
        n: usize,
        span: Span,
    },
    Hole {
        n: usize,
        span: Span,
    },
    RefMember {
        name: &'e Ident,
    },
    RefIndex,
}

pub(super) enum Val {
    Expr(IrExpr),
    Arg(IrArg),
}

fn pop_expr(vals: &mut Vec<Val>) -> IrExpr {
    match vals.pop() {
        Some(Val::Expr(e) | Val::Arg(IrArg::Value(e))) => e,
        _ => hole(Span::default()),
    }
}

fn pop_args(vals: &mut Vec<Val>, n: usize) -> Vec<IrArg> {
    let at = vals.len().saturating_sub(n);
    vals.split_off(at)
        .into_iter()
        .map(|v| match v {
            Val::Expr(e) => IrArg::Value(e),
            Val::Arg(a) => a,
        })
        .collect()
}

pub(super) fn push_args<'e>(tasks: &mut Vec<Task<'e>>, args: &'e [Arg]) {
    tasks.extend(args.iter().rev().map(Task::Arg));
}

impl Resolver<'_> {
    pub(super) fn expr(&mut self, e: &Expr) -> IrExpr {
        self.expr_with(e, true)
    }

    pub(super) fn expr_with(&mut self, root: &Expr, want_value: bool) -> IrExpr {
        let mut vals = self.run(vec![Task::Expr(root, want_value)]);
        pop_expr(&mut vals)
    }

    pub(super) fn args(&mut self, args: &[Arg]) -> Vec<IrArg> {
        let mut tasks = Vec::new();
        push_args(&mut tasks, args);
        let mut vals = self.run(tasks);
        pop_args(&mut vals, args.len())
    }

    fn run<'e>(&mut self, mut tasks: Vec<Task<'e>>) -> Vec<Val> {
        let mut vals = Vec::new();
        while let Some(t) = tasks.pop() {
            match t {
                Task::Expr(e, want) => self.visit(e, want, &mut tasks, &mut vals),
                Task::Arg(a) => self.visit_arg(a, &mut tasks, &mut vals),
                Task::Build(b) => self.build(b, &mut vals),
            }
        }
        vals
    }

    fn visit<'e>(
        &mut self,
        e: &'e Expr,
        want: bool,
        tasks: &mut Vec<Task<'e>>,
        vals: &mut Vec<Val>,
    ) {
        let span = e.span;
        match &e.kind {
            ExprKind::Lit(l) => vals.push(Val::Expr(IrExpr {
                kind: IrExprKind::Const(constant(l)),
                span,
            })),
            ExprKind::Name(id) => {
                let v = match self.read(id) {
                    Some(p) => IrExpr {
                        kind: IrExprKind::Read(p),
                        span,
                    },
                    None => hole(span),
                };
                vals.push(Val::Expr(v));
            }
            ExprKind::Paren(inner) => tasks.push(Task::Expr(inner, true)),
            ExprKind::Unary { op, operand } => tasks.extend([
                Task::Build(Build::Unary { op: *op, span }),
                Task::Expr(operand, true),
            ]),
            ExprKind::Binary { op, lhs, rhs } => tasks.extend([
                Task::Build(Build::Binary { op: *op, span }),
                Task::Expr(rhs, true),
                Task::Expr(lhs, true),
            ]),
            ExprKind::Member { object, name } => tasks.extend([
                Task::Build(Build::Member { name, span }),
                Task::Expr(object, true),
            ]),
            ExprKind::Index { object, index } => tasks.extend([
                Task::Build(Build::Index { span }),
                Task::Expr(index, true),
                Task::Expr(object, true),
            ]),
            ExprKind::Ternary {
                cond,
                then,
                otherwise,
            } => tasks.extend([
                Task::Build(Build::Ternary { span }),
                Task::Expr(otherwise, true),
                Task::Expr(then, true),
                Task::Expr(cond, true),
            ]),
            ExprKind::Await(inner) => {
                tasks.extend([Task::Build(Build::Await { span }), Task::Expr(inner, true)])
            }
            ExprKind::Call { callee, args } => match &callee.kind {
                ExprKind::Member { object, name } => {
                    tasks.push(Task::Build(Build::CallMember {
                        name,
                        n: args.len(),
                        want_value: want,
                        span,
                    }));
                    push_args(tasks, args);
                    tasks.push(Task::Expr(object, true));
                }
                ExprKind::Name(id) => self.visit_call(id, args, want, span, tasks),
                _ => tasks.push(Task::Expr(callee, want)),
            },
            ExprKind::New { type_name, args } => {
                self.visit_new(type_name, args.as_deref(), span, tasks)
            }
            ExprKind::NewDynamic { args } => {
                tasks.push(Task::Build(Build::NewDynamic {
                    n: args.len(),
                    span,
                }));
                push_args(tasks, args);
            }
        }
    }

    fn visit_arg<'e>(&mut self, a: &'e Arg, tasks: &mut Vec<Task<'e>>, vals: &mut Vec<Val>) {
        let Some(mut x) = a.value.as_ref() else {
            vals.push(Val::Arg(IrArg::Omitted));
            return;
        };
        while let ExprKind::Paren(inner) = &x.kind {
            x = inner;
        }
        match &x.kind {
            ExprKind::Name(id) => {
                let arg = match self.read(id) {
                    Some(p @ (Place::Local(_) | Place::ModVar(_) | Place::Outer(_))) => {
                        IrArg::ByRef(p)
                    }
                    Some(p) => IrArg::Value(IrExpr {
                        kind: IrExprKind::Read(p),
                        span: x.span,
                    }),
                    None => IrArg::Value(hole(x.span)),
                };
                vals.push(Val::Arg(arg));
            }
            ExprKind::Member { object, name } => tasks.extend([
                Task::Build(Build::RefMember { name }),
                Task::Expr(object, true),
            ]),
            ExprKind::Index { object, index } => tasks.extend([
                Task::Build(Build::RefIndex),
                Task::Expr(index, true),
                Task::Expr(object, true),
            ]),
            _ => tasks.push(Task::Expr(x, true)),
        }
    }

    fn build(&mut self, b: Build, vals: &mut Vec<Val>) {
        let (kind, span) = match b {
            Build::Unary { op, span } => {
                let operand = Box::new(pop_expr(vals));
                (IrExprKind::Unary { op, operand }, span)
            }
            Build::Binary { op, span } => {
                let rhs = Box::new(pop_expr(vals));
                let lhs = Box::new(pop_expr(vals));
                (IrExprKind::Binary { op, lhs, rhs }, span)
            }
            Build::Ternary { span } => {
                let otherwise = Box::new(pop_expr(vals));
                let then = Box::new(pop_expr(vals));
                let cond = Box::new(pop_expr(vals));
                let kind = IrExprKind::Ternary {
                    cond,
                    then,
                    otherwise,
                };
                (kind, span)
            }
            Build::Await { span } => (IrExprKind::Await(Box::new(pop_expr(vals))), span),
            Build::Evaluate { span } => (IrExprKind::Evaluate(Box::new(pop_expr(vals))), span),
            Build::Member { name, span } => {
                let object = Box::new(pop_expr(vals));
                let name = self.interner.sym(&name.name);
                (IrExprKind::Read(Place::Member { object, name }), span)
            }
            Build::Index { span } => {
                let index = Box::new(pop_expr(vals));
                let object = Box::new(pop_expr(vals));
                (IrExprKind::Read(Place::Index { object, index }), span)
            }
            Build::CallMember {
                name,
                n,
                want_value,
                span,
            } => {
                let args = pop_args(vals, n);
                let object = Box::new(pop_expr(vals));
                let name = self.interner.sym(&name.name);
                let kind = IrExprKind::CallMember {
                    object,
                    name,
                    args,
                    want_value,
                };
                (kind, span)
            }
            Build::Call {
                callee,
                n,
                want_value,
                span,
            } => {
                let args = pop_args(vals, n);
                let kind = IrExprKind::Call {
                    callee,
                    args,
                    want_value,
                };
                (kind, span)
            }
            Build::New { ty, n, span } => {
                let args = n.map(|n| pop_args(vals, n));
                (IrExprKind::New { ty, args }, span)
            }
            Build::NewDynamic { n, span } => {
                let args = pop_args(vals, n);
                (IrExprKind::NewDynamic { args }, span)
            }
            Build::Hole { n, span } => {
                drop(pop_args(vals, n));
                vals.push(Val::Expr(hole(span)));
                return;
            }
            Build::RefMember { name } => {
                let object = Box::new(pop_expr(vals));
                let name = self.interner.sym(&name.name);
                vals.push(Val::Arg(IrArg::ByRef(Place::Member { object, name })));
                return;
            }
            Build::RefIndex => {
                let index = Box::new(pop_expr(vals));
                let object = Box::new(pop_expr(vals));
                vals.push(Val::Arg(IrArg::ByRef(Place::Index { object, index })));
                return;
            }
        };
        vals.push(Val::Expr(IrExpr { kind, span }));
    }
}
