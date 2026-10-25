use crate::ast::{
    Annotation, Arg, BinOp, Expr, ExprKind, Literal, Method, MethodKind, Module, Param, RaiseForm,
    Stmt, StmtKind, UnOp, VarDecl,
};
use std::fmt::Write;

#[doc(hidden)]
pub fn dump_expr(e: &Expr) -> String {
    let mut s = String::new();
    expr(&mut s, e);
    s
}

#[doc(hidden)]
pub fn dump_module(m: &Module) -> String {
    let mut s = String::new();
    for v in &m.vars {
        line(&mut s, 0, |s| var_decl(s, v));
    }
    for meth in &m.methods {
        method(&mut s, meth);
    }
    stmts(&mut s, 0, &m.body);
    if m.incomplete {
        s.push_str("incomplete\n");
    }
    s
}

fn line(s: &mut String, indent: usize, f: impl FnOnce(&mut String)) {
    for _ in 0..indent {
        s.push_str("  ");
    }
    f(s);
    s.push('\n');
}

fn annotations(s: &mut String, list: &[Annotation]) {
    for a in list {
        s.push('&');
        s.push_str(&a.name.name);
        if let Some(arg) = &a.arg {
            s.push('(');
            string(s, arg);
            s.push(')');
        }
        s.push(' ');
    }
}

fn var_decl(s: &mut String, v: &VarDecl) {
    s.push_str("var ");
    annotations(s, &v.annotations);
    for (i, item) in v.items.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&item.name.name);
        if item.export {
            s.push_str(" export");
        }
    }
}

fn param(s: &mut String, p: &Param) {
    if p.by_val {
        s.push_str("val ");
    }
    s.push_str(&p.name.name);
    if let Some(d) = &p.default {
        s.push_str(" = ");
        literal(s, d);
    }
}

fn method(s: &mut String, m: &Method) {
    line(s, 0, |s| {
        annotations(s, &m.annotations);
        if m.async_kw.is_some() {
            s.push_str("async ");
        }
        s.push_str(match m.kind {
            MethodKind::Procedure => "procedure ",
            MethodKind::Function => "function ",
        });
        s.push_str(&m.name.name);
        s.push('(');
        for (i, p) in m.params.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            param(s, p);
        }
        s.push(')');
        if m.export {
            s.push_str(" export");
        }
        if m.incomplete {
            s.push_str(" incomplete");
        }
    });
    for v in &m.vars {
        line(s, 1, |s| var_decl(s, v));
    }
    stmts(s, 1, &m.body);
}

fn stmts(s: &mut String, indent: usize, list: &[Stmt]) {
    for st in list {
        stmt(s, indent, st);
    }
}

fn head_exprs(s: &mut String, indent: usize, head: &str, list: &[&Expr]) {
    line(s, indent, |s| {
        s.push_str(head);
        for e in list {
            s.push(' ');
            expr(s, e);
        }
    });
}

fn stmt(s: &mut String, indent: usize, st: &Stmt) {
    match &st.kind {
        StmtKind::Assign { target, value } => head_exprs(s, indent, "assign", &[target, value]),
        StmtKind::Call(e) => head_exprs(s, indent, "call", &[e]),
        StmtKind::If {
            arms, else_body, ..
        } => {
            for (i, (cond, body)) in arms.iter().enumerate() {
                head_exprs(s, indent, if i == 0 { "if" } else { "elsif" }, &[cond]);
                stmts(s, indent + 1, body);
            }
            if let Some(body) = else_body {
                line(s, indent, |s| s.push_str("else"));
                stmts(s, indent + 1, body);
            }
        }
        StmtKind::While { cond, body } => {
            head_exprs(s, indent, "while", &[cond]);
            stmts(s, indent + 1, body);
        }
        StmtKind::For {
            var,
            from,
            to,
            body,
        } => {
            head_exprs(s, indent, &format!("for {}", var.name), &[from, to]);
            stmts(s, indent + 1, body);
        }
        StmtKind::ForEach {
            var,
            collection,
            body,
        } => {
            head_exprs(s, indent, &format!("foreach {}", var.name), &[collection]);
            stmts(s, indent + 1, body);
        }
        StmtKind::Break => line(s, indent, |s| s.push_str("break")),
        StmtKind::Continue => line(s, indent, |s| s.push_str("continue")),
        StmtKind::Return(v) => {
            let list: Vec<&Expr> = v.iter().collect();
            head_exprs(s, indent, "return", &list);
        }
        StmtKind::Try { body, handler } => {
            line(s, indent, |s| s.push_str("try"));
            stmts(s, indent + 1, body);
            line(s, indent, |s| s.push_str("except"));
            stmts(s, indent + 1, handler);
        }
        StmtKind::Raise(RaiseForm::Rethrow) => line(s, indent, |s| s.push_str("raise")),
        StmtKind::Raise(RaiseForm::Expr(e)) => head_exprs(s, indent, "raise", &[e]),
        StmtKind::Raise(RaiseForm::Call(args)) => line(s, indent, |s| {
            s.push_str("raise-call");
            arg_list(s, args);
        }),
        StmtKind::Goto(l) => line(s, indent, |s| {
            let _ = write!(s, "goto ~{}", l.name);
        }),
        StmtKind::Label(l) => line(s, indent, |s| {
            let _ = write!(s, "label ~{}", l.name);
        }),
        StmtKind::Execute(e) => head_exprs(s, indent, "execute", &[e]),
        StmtKind::AddHandler { event, handler } => {
            head_exprs(s, indent, "addhandler", &[event, handler]);
        }
        StmtKind::RemoveHandler { event, handler } => {
            head_exprs(s, indent, "removehandler", &[event, handler]);
        }
        StmtKind::Await(e) => head_exprs(s, indent, "await", &[e]),
        StmtKind::Value(e) => head_exprs(s, indent, "expr", &[e]),
        StmtKind::Incomplete(parts) => {
            let list: Vec<&Expr> = parts.iter().collect();
            head_exprs(s, indent, "incomplete", &list);
        }
    }
}

fn arg_list(s: &mut String, args: &[Arg]) {
    for a in args {
        s.push(' ');
        match &a.value {
            Some(e) => expr(s, e),
            None => s.push('_'),
        }
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

fn literal(s: &mut String, l: &Literal) {
    match l {
        Literal::Number(n) => s.push_str(n),
        Literal::Str(u) => string(s, u),
        Literal::Date(d) => {
            let _ = write!(
                s,
                "'{:04}{:02}{:02}{:02}{:02}{:02}'",
                d.year, d.month, d.day, d.hour, d.minute, d.second
            );
        }
        Literal::Bool(true) => s.push_str("Истина"),
        Literal::Bool(false) => s.push_str("Ложь"),
        Literal::Undefined => s.push_str("Неопределено"),
        Literal::Null => s.push_str("NULL"),
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

enum Piece<'a> {
    Text(&'a str),
    Expr(&'a Expr),
}

fn arg_pieces<'a>(out: &mut Vec<Piece<'a>>, args: &'a [Arg], lead: bool) {
    for (i, a) in args.iter().enumerate() {
        if lead || i > 0 {
            out.push(Piece::Text(" "));
        }
        out.push(match &a.value {
            Some(e) => Piece::Expr(e),
            None => Piece::Text("_"),
        });
    }
}

fn expr(s: &mut String, root: &Expr) {
    use Piece::{Expr as E, Text as T};
    let mut todo = vec![E(root)];
    let mut parts: Vec<Piece> = Vec::new();
    while let Some(piece) = todo.pop() {
        let e = match piece {
            T(t) => {
                s.push_str(t);
                continue;
            }
            E(e) => e,
        };
        match &e.kind {
            ExprKind::Lit(l) => literal(s, l),
            ExprKind::Name(n) => s.push_str(&n.name),
            ExprKind::Member { object, name } => {
                parts.extend([T("(. "), E(object), T(" "), T(&name.name), T(")")]);
            }
            ExprKind::Index { object, index } => {
                parts.extend([T("([] "), E(object), T(" "), E(index), T(")")]);
            }
            ExprKind::Call { callee, args } => {
                parts.extend([T("(call "), E(callee)]);
                arg_pieces(&mut parts, args, true);
                parts.push(T(")"));
            }
            ExprKind::New { type_name, args } => {
                parts.extend([T("(new "), T(&type_name.name)]);
                if let Some(args) = args {
                    parts.push(T(" ("));
                    arg_pieces(&mut parts, args, false);
                    parts.push(T(")"));
                }
                parts.push(T(")"));
            }
            ExprKind::NewDynamic { args } => {
                parts.push(T("(new*"));
                arg_pieces(&mut parts, args, true);
                parts.push(T(")"));
            }
            ExprKind::Unary { op, operand } => {
                let head = match op {
                    UnOp::Neg => "(neg ",
                    UnOp::Pos => "(pos ",
                    UnOp::Not => "(not ",
                };
                parts.extend([T(head), E(operand), T(")")]);
            }
            ExprKind::Binary { op, lhs, rhs } => {
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
            ExprKind::Ternary {
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
            ExprKind::Paren(inner) => parts.extend([T("(paren "), E(inner), T(")")]),
            ExprKind::Await(inner) => parts.extend([T("(await "), E(inner), T(")")]),
        }
        todo.extend(parts.drain(..).rev());
    }
}
