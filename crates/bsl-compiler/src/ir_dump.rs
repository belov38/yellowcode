use crate::ir::{
    Callee, Const, GlobalRef, IrArg, IrBody, IrExpr, IrExprKind, IrMethod, IrModule, IrStmt,
    IrStmtKind, Place,
};
use crate::sym::Sym;
use bsl_prim::BslDate;
use bsl_syntax::ast::{BinOp, MethodKind, UnOp};
use std::fmt::Write;

struct Names<'a> {
    m: &'a IrModule,
    slots: &'a [Sym],
}

pub fn dump_ir(m: &IrModule) -> String {
    let mut s = String::new();
    for v in &m.vars {
        s.push_str("var ");
        s.push_str(&v.name.name);
        if v.export {
            s.push_str(" export");
        }
        s.push('\n');
    }
    for meth in &m.methods {
        method(&mut s, m, meth);
    }
    if let Some(b) = &m.body {
        s.push_str("body\n");
        stmts(&mut s, &Names { m, slots: &b.slots }, 1, &b.stmts);
    }
    s
}

fn method(s: &mut String, m: &IrModule, meth: &IrMethod) {
    s.push_str("method ");
    s.push_str(&meth.name.name);
    s.push('(');
    for (i, p) in meth.params.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        if p.by_val {
            s.push_str("Знач ");
        }
        s.push_str(&p.name.name);
        if let Some(d) = &p.default {
            s.push_str(" = ");
            constant(s, d);
        }
    }
    s.push_str(match meth.kind {
        MethodKind::Procedure => ") procedure",
        MethodKind::Function => ") function",
    });
    if meth.export {
        s.push_str(" export");
    }
    if meth.is_async {
        s.push_str(" async");
    }
    s.push('\n');
    body(s, m, &meth.body);
}

fn body(s: &mut String, m: &IrModule, b: &IrBody) {
    stmts(s, &Names { m, slots: &b.slots }, 1, &b.stmts);
}

fn stmts(s: &mut String, n: &Names, indent: usize, list: &[IrStmt]) {
    for st in list {
        stmt(s, n, indent, st);
    }
}

fn line(s: &mut String, indent: usize, text: &str) {
    for _ in 0..indent {
        s.push_str("  ");
    }
    s.push_str(text);
    s.push('\n');
}

fn head(s: &mut String, n: &Names, indent: usize, head: &str, list: &[Piece]) {
    for _ in 0..indent {
        s.push_str("  ");
    }
    s.push_str(head);
    for p in list {
        s.push(' ');
        pieces(s, n, p.clone());
    }
    s.push('\n');
}

fn stmt(s: &mut String, n: &Names, indent: usize, st: &IrStmt) {
    use Piece::{Arg as A, Expr as E, Place as P};
    match &st.kind {
        IrStmtKind::Assign { target, value } => {
            head(s, n, indent, "assign", &[P(target), E(value)])
        }
        IrStmtKind::Eval(e) => head(s, n, indent, "call", &[E(e)]),
        IrStmtKind::If { arms, otherwise } => {
            for (i, a) in arms.iter().enumerate() {
                head(
                    s,
                    n,
                    indent,
                    if i == 0 { "if" } else { "elsif" },
                    &[E(&a.cond)],
                );
                stmts(s, n, indent + 1, &a.body);
            }
            if !otherwise.is_empty() {
                line(s, indent, "else");
                stmts(s, n, indent + 1, otherwise);
            }
            line(s, indent, "end");
        }
        IrStmtKind::While { cond, body } => {
            head(s, n, indent, "while", &[E(cond)]);
            stmts(s, n, indent + 1, body);
        }
        IrStmtKind::For {
            var,
            from,
            to,
            body,
            ..
        } => {
            head(s, n, indent, "for", &[P(var), E(from), E(to)]);
            stmts(s, n, indent + 1, body);
        }
        IrStmtKind::ForEach {
            var,
            collection,
            body,
            ..
        } => {
            head(s, n, indent, "foreach", &[P(var), E(collection)]);
            stmts(s, n, indent + 1, body);
        }
        IrStmtKind::Break => line(s, indent, "break"),
        IrStmtKind::Continue => line(s, indent, "continue"),
        IrStmtKind::Return(None) => line(s, indent, "return"),
        IrStmtKind::Return(Some(e)) => head(s, n, indent, "return", &[E(e)]),
        IrStmtKind::Try { body, handler } => {
            line(s, indent, "try");
            stmts(s, n, indent + 1, body);
            line(s, indent, "except");
            stmts(s, n, indent + 1, handler);
            line(s, indent, "end");
        }
        IrStmtKind::Raise(e) => head(s, n, indent, "raise", &[E(e)]),
        IrStmtKind::RaiseCall(args) => {
            let list: Vec<Piece> = args.iter().map(A).collect();
            head(s, n, indent, "raise-call", &list);
        }
        IrStmtKind::Reraise => line(s, indent, "reraise"),
        IrStmtKind::Goto(l) => line(s, indent, &format!("goto L{l}")),
        IrStmtKind::Label(l) => line(s, indent, &format!("label L{l}")),
        IrStmtKind::Execute(e) => head(s, n, indent, "execute", &[E(e)]),
        IrStmtKind::AddHandler { event, handler } => {
            head(s, n, indent, "add-handler", &[E(event), E(handler)]);
        }
        IrStmtKind::RemoveHandler { event, handler } => {
            head(s, n, indent, "remove-handler", &[E(event), E(handler)]);
        }
        IrStmtKind::Await(e) => head(s, n, indent, "await", &[E(e)]),
    }
}

fn string(s: &mut String, units: &[u16]) {
    s.push('"');
    for c in char::decode_utf16(units.iter().copied()) {
        match c {
            Ok('\\') => s.push_str("\\\\"),
            Ok('"') => s.push_str("\\\""),
            Ok('\n') => s.push_str("\\n"),
            Ok('\r') => s.push_str("\\r"),
            Ok('\t') => s.push_str("\\t"),
            Ok(c) if c.is_control() => {
                let _ = write!(s, "\\u{{{:x}}}", u32::from(c));
            }
            Ok(c) => s.push(c),
            Err(e) => {
                let _ = write!(s, "\\u{{{:x}}}", e.unpaired_surrogate());
            }
        }
    }
    s.push('"');
}

pub(crate) fn constant(s: &mut String, c: &Const) {
    match c {
        Const::Number(n) => {
            let _ = write!(s, "{n}");
        }
        Const::String(u) => string(s, u),
        Const::Date(d) if *d == BslDate::EMPTY => s.push_str("'00000000'"),
        Const::Date(d) => {
            let p = d.parts();
            let _ = write!(
                s,
                "'{:04}{:02}{:02}{:02}{:02}{:02}'",
                p.year, p.month, p.day, p.hour, p.minute, p.second
            );
        }
        Const::Bool(true) => s.push_str("Истина"),
        Const::Bool(false) => s.push_str("Ложь"),
        Const::Undefined => s.push_str("Неопределено"),
        Const::Null => s.push_str("NULL"),
    }
}

fn binop(op: BinOp) -> &'static str {
    match op {
        BinOp::Or => "or",
        BinOp::And => "and",
        BinOp::Eq => "=",
        BinOp::Ne => "<>",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
    }
}

#[derive(Clone)]
enum Piece<'a> {
    Text(&'a str),
    Owned(String),
    Expr(&'a IrExpr),
    Place(&'a Place),
    Arg(&'a IrArg),
}

fn slot_name(names: &[Sym], i: u32, prefix: &str) -> String {
    match names.get(i as usize) {
        Some(n) if !n.name.is_empty() => n.name.to_string(),
        _ => format!("{prefix}{i}"),
    }
}

pub(crate) fn global_name(g: &GlobalRef) -> String {
    let c = platform_catalog::catalog();
    match g {
        GlobalRef::App(crate::app::AppGlobal::CommonModule { name }) => name.clone(),
        GlobalRef::ApplicationVariable(variable) => {
            format!("{:?}.{}", variable.module, variable.name)
        }
        GlobalRef::PlatformProperty(i) => c
            .global_properties()
            .get(usize::from(*i))
            .map_or_else(|| format!("#{i}"), |p| p.name_ru.clone()),
        GlobalRef::SystemEnum(t) => c
            .types()
            .get(usize::from(t.0))
            .map_or_else(|| format!("#{}", t.0), |t| t.name_ru.clone()),
    }
}

fn pieces(s: &mut String, n: &Names, root: Piece) {
    use Piece::{Arg as A, Expr as E, Owned as O, Place as P, Text as T};
    let mut todo = vec![root];
    let mut parts: Vec<Piece> = Vec::new();
    while let Some(piece) = todo.pop() {
        match piece {
            T(t) => s.push_str(t),
            O(t) => s.push_str(&t),
            A(IrArg::Omitted) => s.push('_'),
            A(IrArg::Value(e)) => todo.push(E(e)),
            A(IrArg::ByRef(p)) => parts.extend([T("(ref "), P(p), T(")")]),
            P(p) => match p {
                Place::Local(i) => {
                    s.push_str(&format!("(local {})", slot_name(n.slots, *i, "$")));
                }
                Place::ModVar(i) => {
                    let name = n.m.vars.get(*i as usize).map(|v| &v.name);
                    let name = name.or(n.m.caller_vars.get(*i as usize));
                    let name = name.map_or_else(|| format!("#{i}"), |v| v.name.to_string());
                    s.push_str(&format!("(modvar {name})"));
                }
                Place::Outer(i) => {
                    s.push_str(&format!("(outer {})", slot_name(&n.m.outer, *i, "#")));
                }
                Place::Global(g) => s.push_str(&format!("(global {})", global_name(g))),
                Place::Context(sym) => s.push_str(&format!("(ctx {})", sym.name)),
                Place::Member { object, name } => {
                    parts.extend([T("(. "), E(object), O(format!(" {})", name.name))]);
                }
                Place::Index { object, index } => {
                    parts.extend([T("([] "), E(object), T(" "), E(index), T(")")]);
                }
            },
            E(e) => match &e.kind {
                IrExprKind::Const(c) => constant(s, c),
                IrExprKind::Read(p) => parts.push(P(p)),
                IrExprKind::Call { callee, args, .. } => {
                    let head = match callee {
                        Callee::Local(i) => format!(
                            "(call-local {}",
                            n.m.methods
                                .get(*i as usize)
                                .map(|m| &m.name)
                                .or(n.m.caller_methods.get(*i as usize))
                                .map_or_else(|| format!("#{i}"), |s| s.name.to_string())
                        ),
                        Callee::App(sig) => format!("(call-app {}.{}", sig.module, sig.name),
                        Callee::Platform(i) => format!(
                            "(call-platform {}",
                            platform_catalog::catalog()
                                .global_methods()
                                .get(usize::from(*i))
                                .map_or_else(|| format!("#{i}"), |g| g.info.name_ru.clone())
                        ),
                        Callee::Context(sym) => format!("(call-ctx {}", sym.name),
                    };
                    parts.push(O(head));
                    for a in args {
                        parts.extend([T(" "), A(a)]);
                    }
                    parts.push(T(")"));
                }
                IrExprKind::CallMember {
                    object, name, args, ..
                } => {
                    parts.extend([T("(call-member "), E(object), O(format!(" {}", name.name))]);
                    for a in args {
                        parts.extend([T(" "), A(a)]);
                    }
                    parts.push(T(")"));
                }
                IrExprKind::New { ty, args } => {
                    let name = platform_catalog::catalog()
                        .types()
                        .get(usize::from(ty.0))
                        .map_or_else(|| format!("#{}", ty.0), |t| t.name_ru.clone());
                    parts.push(O(format!("(new {name}")));
                    if let Some(args) = args {
                        parts.push(T(" ("));
                        for (i, a) in args.iter().enumerate() {
                            if i > 0 {
                                parts.push(T(" "));
                            }
                            parts.push(A(a));
                        }
                        parts.push(T(")"));
                    }
                    parts.push(T(")"));
                }
                IrExprKind::NewDynamic { args } => {
                    parts.push(T("(new*"));
                    for a in args {
                        parts.extend([T(" "), A(a)]);
                    }
                    parts.push(T(")"));
                }
                IrExprKind::Unary { op, operand } => {
                    let head = match op {
                        UnOp::Neg => "(neg ",
                        UnOp::Pos => "(pos ",
                        UnOp::Not => "(not ",
                    };
                    parts.extend([T(head), E(operand), T(")")]);
                }
                IrExprKind::Binary { op, lhs, rhs } => {
                    parts.extend([
                        T("("),
                        T(binop(*op)),
                        T(" "),
                        E(lhs),
                        T(" "),
                        E(rhs),
                        T(")"),
                    ]);
                }
                IrExprKind::Ternary {
                    cond,
                    then,
                    otherwise,
                } => {
                    parts.extend([
                        T("(? "),
                        E(cond),
                        T(" "),
                        E(then),
                        T(" "),
                        E(otherwise),
                        T(")"),
                    ]);
                }
                IrExprKind::Evaluate(x) => parts.extend([T("(eval "), E(x), T(")")]),
                IrExprKind::Await(x) => parts.extend([T("(await "), E(x), T(")")]),
            },
        }
        todo.extend(parts.drain(..).rev());
    }
}
