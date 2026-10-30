use super::L;
use super::prec::{Prec, chain_operand, prec};
use query_syntax::ast::*;
use query_syntax::kw::Kw;

impl L {
    fn operand(&mut self, e: &Expr, min: Prec) {
        if prec(e) < min {
            self.tok("(");
            self.expr(e);
            self.raw(")");
        } else {
            self.expr(e);
        }
    }

    fn predicate_left(&mut self, e: &Expr, bare_cast: bool) {
        let e = match &e.kind {
            ExprKind::Paren(inner)
                if self.ordinary_query
                    && bare_cast
                    && matches!(&inner.kind, ExprKind::Cast { deref, .. } if deref.is_empty()) =>
            {
                inner
            }
            _ => e,
        };
        self.operand(e, Prec::Cmp);
    }

    fn string(&mut self, s: &str) {
        let mut q = String::with_capacity(s.len() + 2);
        q.push('"');
        let mut rest = s;
        while let Some(i) = rest.find('\n') {
            let (head, tail) = rest.split_at(i + 1);
            push_escaped(&mut q, head);
            let ws = tail.len() - tail.trim_start_matches([' ', '\t']).len();
            if tail.get(ws..).is_some_and(|t| t.starts_with('|')) {
                q.extend(std::iter::repeat_n('\t', self.ei));
            }
            rest = tail;
        }
        push_escaped(&mut q, rest);
        q.push('"');
        self.tok(&q);
    }

    fn call(&mut self, body: impl FnOnce(&mut Self)) {
        self.raw("(");
        self.ei += 1;
        body(self);
        self.ei -= 1;
        self.raw(")");
    }

    pub(crate) fn expr(&mut self, e: &Expr) {
        let expression_base = self.expression_base;
        self.expression_base = Some(expression_base.unwrap_or(self.ei));
        let tight = std::mem::take(&mut self.tight);
        match &e.kind {
            ExprKind::Lit(l) => match l {
                Lit::Number(n) => self.tok(&n.digits),
                Lit::String(s) => self.string(s),
                Lit::Bool(true) => self.kw(Kw::True),
                Lit::Bool(false) => self.kw(Kw::False),
                Lit::Null => self.kw(Kw::Null),
                Lit::Undefined => self.kw(Kw::Undefined),
            },
            ExprKind::Param(p) => self.param(p),
            ExprKind::Path(p) => self.path(p),
            ExprKind::Chain { first, rest } if rest.is_empty() => self.expr(first),
            ExprKind::Chain { first, rest } => {
                let level = prec(e);
                let min = chain_operand(level);
                let logical = matches!(level, Prec::And | Prec::Or);
                let (ei, sep) = (self.ei, self.sep());
                if logical {
                    self.ei = ei + 1;
                    if self.ordinary_query
                        && level == Prec::And
                        && matches!(&first.kind, ExprKind::Paren(e) if prec(e) == Prec::Or)
                    {
                        self.ei = ei;
                    }
                }
                self.operand(first, min);
                if logical {
                    self.ei = ei + 1;
                }
                for op in rest {
                    if logical {
                        self.brk(sep);
                    }
                    match op.op {
                        BinOp::Add => self.tok("+"),
                        BinOp::Sub => self.tok("-"),
                        BinOp::Mul => self.tok("*"),
                        BinOp::Div => self.tok("/"),
                        BinOp::And => self.kw(Kw::And),
                        BinOp::Or => self.kw(Kw::Or),
                    }
                    if self.ordinary_query
                        && op.op == BinOp::And
                        && matches!(op.expr.kind, ExprKind::Between { not: false, .. })
                    {
                        self.tok("(");
                        self.expr(&op.expr);
                        self.raw(")");
                    } else {
                        self.operand(&op.expr, min);
                    }
                }
                self.ei = ei;
            }
            ExprKind::Unary { op, expr, .. } => match op {
                UnOp::Not => {
                    let expr: &Expr = match &expr.kind {
                        ExprKind::Paren(inner)
                            if self.ordinary_query
                                && matches!(
                                    inner.kind,
                                    ExprKind::Compare { .. }
                                        | ExprKind::In { .. }
                                        | ExprKind::IsNull { .. }
                                        | ExprKind::Refs { .. }
                                        | ExprKind::Like { .. }
                                ) =>
                        {
                            inner
                        }
                        _ => expr,
                    };
                    self.kw(Kw::Not);
                    self.glue = matches!(expr.kind, ExprKind::Paren(_));
                    self.ei += 1;
                    self.operand(expr, Prec::Not);
                    self.ei -= 1;
                }
                UnOp::Neg | UnOp::Plus => {
                    self.tok(if *op == UnOp::Neg { "-" } else { "+" });
                    self.glue = true;
                    self.operand(expr, Prec::Unary);
                }
            },
            ExprKind::Compare { op, l, r, .. } => {
                self.operand(l, Prec::Cmp);
                self.tok(match op {
                    CmpOp::Eq => "=",
                    CmpOp::Ne => "<>",
                    CmpOp::Lt => "<",
                    CmpOp::Gt => ">",
                    CmpOp::Le => "<=",
                    CmpOp::Ge => ">=",
                });
                self.operand(r, Prec::Add);
            }
            ExprKind::In {
                l,
                not,
                hierarchy,
                rhs,
                ..
            } => {
                self.predicate_left(l, !*not && !*hierarchy);
                if *not {
                    self.kw(Kw::Not);
                }
                self.kw(Kw::In);
                if *hierarchy {
                    self.kw(Kw::InHierarchy);
                }
                match rhs {
                    InRhs::List(es) => {
                        let single =
                            matches!(es.as_slice(), [x] if matches!(x.kind, ExprKind::Param(_)));
                        if tight
                            && single
                            && !*not
                            && self.embedded == 0
                            && matches!(l.kind, ExprKind::Path(_))
                        {
                            self.glue = true;
                        }
                        self.tok("(");
                        for (i, x) in es.iter().enumerate() {
                            if i > 0 {
                                self.raw(",");
                            }
                            self.expr(x);
                        }
                        self.raw(")");
                    }
                    InRhs::Query(q) => {
                        let (sep, ei) = (self.sep(), self.ei);
                        self.brk(sep);
                        self.tok("(");
                        self.embedded += 1;
                        self.query(q, sep, sep.saturating_sub(1));
                        self.embedded -= 1;
                        self.raw(")");
                        self.ei = ei;
                    }
                }
            }
            ExprKind::Between { l, not, lo, hi, .. } => {
                self.predicate_left(l, !*not);
                if *not {
                    self.kw(Kw::Not);
                }
                self.kw(Kw::Between);
                self.operand(lo, Prec::Add);
                self.kw(Kw::And);
                self.operand(hi, Prec::Add);
            }
            ExprKind::IsNull { l, not, .. } => {
                self.predicate_left(l, !*not);
                self.kw(Kw::Is);
                if *not {
                    self.kw(Kw::Not);
                }
                self.kw(Kw::Null);
                self.pending = *not;
            }
            ExprKind::Refs { l, table, .. } => {
                self.operand(l, Prec::Cmp);
                self.kw(Kw::Refs);
                self.path(table);
            }
            ExprKind::Like {
                l,
                not,
                pattern,
                escape,
                ..
            } => {
                self.predicate_left(l, !*not);
                if *not {
                    self.kw(Kw::Not);
                }
                self.kw(Kw::Like);
                self.operand(pattern, Prec::Add);
                if let Some(esc) = escape {
                    self.kw(Kw::Escape);
                    self.string(&esc.ch);
                }
            }
            ExprKind::Tuple(es) => {
                self.tok("(");
                for (i, x) in es.iter().enumerate() {
                    if i > 0 {
                        self.raw(",");
                    }
                    self.expr(x);
                }
                self.raw(")");
            }
            ExprKind::Case {
                operand,
                whens,
                else_,
                ..
            } => self.case(operand.as_deref(), whens, else_.as_deref()),
            ExprKind::Deref { expr, deref } => {
                self.expr(expr);
                for name in deref {
                    self.raw(".");
                    self.raw(&name.text);
                }
            }
            ExprKind::Cast {
                expr, ty, deref, ..
            } => {
                self.kw(Kw::Cast);
                self.call(|t| {
                    t.expr(expr);
                    t.kw(Kw::As);
                    t.cast_type(ty);
                });
                for name in deref {
                    self.raw(".");
                    self.raw(&name.text);
                }
            }
            ExprKind::Func {
                func,
                distinct,
                args,
                ..
            } => {
                let name = func.name(self.script);
                self.tok(name);
                self.call(|t| {
                    if distinct.is_some() {
                        t.kw(Kw::Distinct);
                    }
                    match args {
                        FuncArgs::Star(_) => t.tok("*"),
                        FuncArgs::List(es) => t.list(es),
                    }
                });
            }
            ExprKind::TypeOf { name, .. } => {
                self.kw(Kw::Type);
                self.raw("(");
                self.path(name);
                self.raw(")");
            }
            ExprKind::Value { path, .. } => {
                self.kw(Kw::Value);
                self.raw("(");
                self.path(path);
                self.raw(")");
            }
            ExprKind::DateTime { args, .. } => {
                self.kw(Kw::DateTime);
                self.raw("(");
                self.list(args);
                self.raw(")");
            }
            ExprKind::Paren(inner) => {
                self.tok("(");
                self.expr(inner);
                self.raw(")");
            }
            ExprKind::Dcs(l) => self.dcs_group(l, true),
        }
        self.expression_base = expression_base;
    }

    fn list(&mut self, es: &[Expr]) {
        for (i, x) in es.iter().enumerate() {
            if i > 0 {
                self.raw(",");
            }
            self.expr(x);
        }
    }

    fn case(&mut self, operand: Option<&Expr>, whens: &[When], else_: Option<&Expr>) {
        let base = self.ei;
        self.kw(Kw::Case);
        let at = |t: &mut Self, line: usize, x: &Expr| {
            t.ei = line + 1;
            t.expr(x);
        };
        if let Some(x) = operand {
            at(self, base, x);
        }
        for w in whens {
            self.brk(base + 1);
            self.kw(Kw::When);
            at(self, base + 1, &w.cond);
            self.brk(base + 2);
            self.kw(Kw::Then);
            at(self, base + 2, &w.then);
        }
        if let Some(x) = else_ {
            self.brk(base + 1);
            self.kw(Kw::Else);
            at(self, base + 1, x);
        }
        self.brk(base);
        self.kw(Kw::End);
        self.ei = base;
    }

    fn cast_type(&mut self, ty: &CastType) {
        match ty {
            CastType::Boolean => self.kw(Kw::Boolean),
            CastType::Date => self.kw(Kw::Date),
            CastType::Number { len, scale } => {
                self.kw(Kw::Number);
                if let Some(l) = len {
                    self.raw("(");
                    self.raw(&l.digits);
                    if let Some(s) = scale {
                        self.raw(", ");
                        self.raw(&s.digits);
                    }
                    self.raw(")");
                }
            }
            CastType::String { len } => {
                self.kw(Kw::String);
                if let Some(l) = len {
                    self.raw("(");
                    self.raw(&l.digits);
                    self.raw(")");
                }
            }
            CastType::Table(p) => self.path(p),
        }
    }

    pub(crate) fn vt_args(&mut self, a: &VtArgs, line: usize) {
        let column = a
            .args
            .iter()
            .flatten()
            .any(|g| any_node(&g.expr, &mut is_multiline));
        self.raw("(");
        for (i, arg) in a.args.iter().enumerate() {
            if i > 0 {
                self.raw(",");
            }
            if column {
                self.nl(line + 2);
            }
            match arg {
                None if i > 0 && !column => self.raw(" "),
                None => {}
                Some(arg) => {
                    self.expr(&arg.expr);
                    if let Some((d, _)) = arg.dir {
                        self.dir(d, true);
                    }
                    for g in &arg.dcs {
                        self.dcs_group(g, true);
                    }
                }
            }
        }
        self.raw(")");
    }

    pub(crate) fn dcs_block(&mut self, b: &DcsBlock, r: usize) {
        self.tok("{");
        match &b.kind {
            DcsKind::Select(l) => {
                self.kw(Kw::Select);
                self.dcs_lines(l, r + 1, false);
            }
            DcsKind::Where(l) => {
                self.kw(Kw::Where);
                self.dcs_lines(l, r + 1, true);
            }
            DcsKind::OrderBy(l) => {
                self.kws(&[Kw::Order, Kw::By]);
                self.dcs_lines(l, r + 1, false);
            }
            DcsKind::GroupBy(l) => {
                self.kws(&[Kw::Group, Kw::By]);
                self.dcs_lines(l, r + 1, false);
            }
            DcsKind::Totals(l) => {
                self.kws(&[Kw::Totals, Kw::By]);
                self.dcs_lines(l, r + 1, false);
            }
            DcsKind::Characteristics(c) => {
                self.kw(Kw::Characteristics);
                self.characteristics(c, r + 1);
                self.raw(" ");
            }
        }
        self.raw("}");
    }

    fn dcs_lines(&mut self, l: &DcsList, indent: usize, cond: bool) {
        self.lines(&l.items, indent, |t, it| {
            t.ei += usize::from(cond);
            t.dcs_item(it, cond, false);
        });
    }

    fn dcs_group(&mut self, l: &DcsList, cond: bool) {
        self.tok("{");
        for (i, it) in l.items.iter().enumerate() {
            if i > 0 {
                self.raw(",");
            }
            self.dcs_item(it, cond, true);
        }
        if let Some(DcsUnresolvedTail::AdjacentParam(parameter)) = &l.unresolved_tail {
            self.param(parameter);
        }
        self.raw("}");
    }

    fn dcs_item(&mut self, it: &DcsItem, cond: bool, vt: bool) {
        let wrap = cond
            && (!matches!(it.expr.kind, ExprKind::Path(_) | ExprKind::Paren(_))
                || (vt && it.star.is_some() && matches!(it.expr.kind, ExprKind::Path(_))));
        if wrap {
            self.tok("(");
            self.expr(&it.expr);
            self.raw(")");
        } else {
            self.expr(&it.expr);
        }
        if it.star.is_some() {
            self.raw(".*");
        }
        if let Some(n) = &it.nested {
            self.raw(".(");
            for (i, x) in n.items.iter().enumerate() {
                if i > 0 {
                    self.raw(",");
                }
                self.dcs_item(x, false, false);
            }
            self.raw(")");
        }
        for group in &it.dcs {
            self.dcs_group(group, cond);
        }
        if let Some((d, _)) = it.dir {
            self.dir(d, true);
        }
        self.alias(it.alias.as_ref());
    }

    fn characteristics(&mut self, c: &Characteristics, line: usize) {
        self.nl(line);
        self.kw(Kw::Type);
        self.raw("(");
        self.path(&c.type_name);
        self.raw(")");
        self.nl(line);
        self.kw(Kw::CharacteristicTypes);
        self.char_source(&c.kinds, line);
        self.field(Kw::KeyField, Some(&c.key_field), line);
        self.field(Kw::NameField, Some(&c.name_field), line);
        self.field(Kw::ValueTypeField, c.value_type_field.as_ref(), line);
        self.nl(line);
        self.kw(Kw::CharacteristicValues);
        self.char_source(&c.values, line);
        self.field(Kw::ObjectField, Some(&c.object_field), line);
        self.field(Kw::TypeField, Some(&c.kind_field), line);
        self.field(Kw::ValueField, c.value_field.as_ref(), line);
    }

    fn field(&mut self, word: Kw, p: Option<&Path>, line: usize) {
        if let Some(p) = p {
            self.nl(line);
            self.kw(word);
            self.path(p);
        }
    }

    fn char_source(&mut self, s: &CharSource, line: usize) {
        match s {
            CharSource::Table(p) => self.path(p),
            CharSource::Query(q) => {
                self.tok("(");
                self.query(q, line, line);
                self.raw(")");
            }
        }
    }
}

fn push_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
}

fn is_multiline(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::In {
            rhs: InRhs::Query(_),
            ..
        } => true,
        ExprKind::Chain { rest, .. } => rest
            .first()
            .is_some_and(|o| matches!(o.op, BinOp::And | BinOp::Or)),
        _ => false,
    }
}

fn any_node(e: &Expr, f: &mut impl FnMut(&Expr) -> bool) -> bool {
    if f(e) {
        return true;
    }
    let mut any = |x: &Expr| any_node(x, f);
    match &e.kind {
        ExprKind::Chain { first, rest } => any(first) || rest.iter().any(|o| any(&o.expr)),
        ExprKind::Unary { expr, .. } | ExprKind::Paren(expr) => any(expr),
        ExprKind::Compare { l, r, .. } => any(l) || any(r),
        ExprKind::In { l, rhs, .. } => {
            any(l)
                || match rhs {
                    InRhs::List(es) => es.iter().any(any),
                    InRhs::Query(_) => false,
                }
        }
        ExprKind::Between { l, lo, hi, .. } => any(l) || any(lo) || any(hi),
        ExprKind::IsNull { l, .. } | ExprKind::Refs { l, .. } => any(l),
        ExprKind::Like { l, pattern, .. } => any(l) || any(pattern),
        ExprKind::Tuple(es) | ExprKind::DateTime { args: es, .. } => es.iter().any(any),
        ExprKind::Case {
            operand,
            whens,
            else_,
            ..
        } => {
            operand.as_deref().is_some_and(&mut any)
                || whens.iter().any(|w| any(&w.cond) || any(&w.then))
                || else_.as_deref().is_some_and(&mut any)
        }
        ExprKind::Cast { expr, .. } | ExprKind::Deref { expr, .. } => any(expr),
        ExprKind::Func {
            args: FuncArgs::List(es),
            ..
        } => es.iter().any(any),
        _ => false,
    }
}
