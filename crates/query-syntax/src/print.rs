use crate::ast::*;
use crate::kw::{Kw, lookup};
use crate::lang::ScriptVariant;
use crate::with_deep_stack;

pub fn to_sexpr(b: &Batch) -> String {
    with_deep_stack(b.max_depth, || {
        let mut w = W::default();
        w.open("batch");
        for s in &b.stmts {
            w.stmt(s);
        }
        w.close();
        w.0
    })
}

pub fn expr_sexpr(e: &Expr) -> String {
    let mut w = W::default();
    w.expr(e);
    w.0
}

pub fn condition_sexpr(c: &AccessCondition) -> String {
    with_deep_stack(c.max_depth, || {
        let mut w = W::default();
        match &c.form {
            AccessForm::Short { cond, .. } => w.list("where", |w| w.expr(cond)),
            AccessForm::Full {
                alias,
                from,
                where_,
            } => w.list("access", |w| {
                w.atom(&alias.text);
                w.from(from);
                w.clause("where", where_.as_ref());
            }),
        }
        w.0
    })
}

pub fn to_text(b: &Batch) -> String {
    with_deep_stack(b.max_depth, || {
        let mut t = T::default();
        for (i, s) in b.stmts.iter().enumerate() {
            if i > 0 {
                t.raw(";\n");
            }
            t.stmt(s);
        }
        t.s
    })
}

pub fn condition_text(c: &AccessCondition) -> String {
    with_deep_stack(c.max_depth, || {
        let mut t = T::default();
        match &c.form {
            AccessForm::Short { cond, .. } => {
                t.kw(Kw::Where);
                t.expr(cond);
            }
            AccessForm::Full {
                alias,
                from,
                where_,
            } => {
                t.tok(&alias.text);
                t.from(from);
                t.clause(Kw::Where, where_.as_ref());
            }
        }
        t.s
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    Or,
    And,
    Not,
    Cmp,
    Pred,
    Add,
    Mul,
    Unary,
    Primary,
}

fn prec(mut e: &Expr) -> Prec {
    while let ExprKind::Chain { first, rest } = &e.kind
        && rest.is_empty()
    {
        e = first;
    }
    match &e.kind {
        ExprKind::Chain { rest, .. } => match rest.first().map(|o| o.op) {
            Some(BinOp::Or) => Prec::Or,
            Some(BinOp::And) => Prec::And,
            Some(BinOp::Add | BinOp::Sub) => Prec::Add,
            _ => Prec::Mul,
        },
        ExprKind::Unary { op: UnOp::Not, .. } => Prec::Not,
        ExprKind::Unary { .. } => Prec::Unary,
        ExprKind::Compare { .. } => Prec::Cmp,
        ExprKind::In { .. }
        | ExprKind::Between { .. }
        | ExprKind::IsNull { .. }
        | ExprKind::Refs { .. }
        | ExprKind::Like { .. } => Prec::Pred,
        _ => Prec::Primary,
    }
}

fn chain_operand(level: Prec) -> Prec {
    match level {
        Prec::Or => Prec::And,
        Prec::And => Prec::Not,
        Prec::Add => Prec::Mul,
        _ => Prec::Unary,
    }
}

fn bare_not_operand(l: &Expr) -> bool {
    matches!(
        &l.kind,
        ExprKind::Unary { op: UnOp::Not, expr, .. }
            if !matches!(expr.kind, ExprKind::Unary { op: UnOp::Not, .. })
    )
}

fn starts_with_paren(mut e: &Expr) -> bool {
    loop {
        let (left, min) = match &e.kind {
            ExprKind::Paren(_) | ExprKind::Tuple(_) => return true,
            ExprKind::Chain { first, rest } if rest.is_empty() => {
                e = first;
                continue;
            }
            ExprKind::Chain { first, .. } => (first, chain_operand(prec(e))),
            ExprKind::Compare { l, .. } => (l, Prec::Cmp),
            ExprKind::Between { l, .. } if bare_not_operand(l) => return false,
            ExprKind::In { l, .. }
            | ExprKind::Between { l, .. }
            | ExprKind::IsNull { l, .. }
            | ExprKind::Refs { l, .. }
            | ExprKind::Like { l, .. } => (l, Prec::Cmp),
            _ => return false,
        };
        if prec(left) < min {
            return true;
        }
        e = left;
    }
}

#[derive(Default)]
struct T {
    s: String,
    glue: bool,
}

impl T {
    fn tok(&mut self, s: &str) {
        if !self.glue && !self.s.is_empty() && !self.s.ends_with(['(', '{', '.', ' ', '\n']) {
            self.s.push(' ');
        }
        self.glue = false;
        self.s.push_str(s);
    }
    fn raw(&mut self, s: &str) {
        self.glue = false;
        self.s.push_str(s);
    }
    fn kw(&mut self, k: Kw) {
        self.tok(k.name(ScriptVariant::Russian));
    }
    fn kws(&mut self, ks: &[Kw]) {
        for k in ks {
            self.kw(*k);
        }
    }
    fn list<I>(&mut self, items: &[I], mut f: impl FnMut(&mut Self, &I)) {
        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                self.raw(",");
            }
            f(self, it);
        }
    }
    fn path(&mut self, p: &Path) {
        for (i, part) in p.parts.iter().enumerate() {
            if i == 0 {
                self.tok(&part.text);
            } else {
                self.raw(".");
                self.raw(&part.text);
            }
        }
    }
    fn string(&mut self, s: &str) {
        let mut q = String::with_capacity(s.len() + 2);
        q.push('"');
        for c in s.chars() {
            if c == '"' {
                q.push('"');
            }
            q.push(c);
        }
        q.push('"');
        self.tok(&q);
    }
    fn param(&mut self, p: &Param) {
        self.tok("&");
        self.raw(&p.name.text);
    }
    fn alias(&mut self, a: Option<&Alias>, keep_implicit: bool) {
        if let Some(a) = a {
            if a.as_kw.is_some() || !keep_implicit {
                self.kw(Kw::As);
            }
            self.tok(&a.name.text);
        }
    }
    fn clause(&mut self, word: Kw, c: Option<&Clause>) {
        if let Some(c) = c {
            self.kw(word);
            self.expr(&c.expr);
        }
    }
    fn dir(&mut self, d: OrderDir, asc: bool) {
        match d {
            OrderDir::Asc if asc => self.kw(Kw::Asc),
            OrderDir::Asc => {}
            OrderDir::Desc => self.kw(Kw::Desc),
            OrderDir::Hierarchy => self.kw(Kw::Hierarchy),
            OrderDir::HierarchyDesc => self.kws(&[Kw::Hierarchy, Kw::Desc]),
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Query(q) => self.query(&q.query),
            Stmt::Drop(d) => {
                self.kw(Kw::Drop);
                self.tok(&d.name.text);
            }
        }
    }

    fn query(&mut self, q: &Query) {
        let late_index = q
            .order_by
            .as_ref()
            .and_then(|_| q.parts.last())
            .and_then(|p| p.index_by.as_ref());
        for (i, part) in q.parts.iter().enumerate() {
            if let Some(j) = i.checked_sub(1) {
                self.kw(Kw::Union);
                if q.unions.get(j).is_some_and(|u| u.all) {
                    self.kw(Kw::All);
                }
            }
            self.select(part, late_index.is_none() || i + 1 < q.parts.len());
        }
        if let Some(o) = &q.order_by {
            self.kws(&[Kw::Order, Kw::By]);
            self.list(&o.items, |t, it| {
                t.expr(&it.expr);
                t.dir(it.dir, false);
            });
        }
        if let Some(ix) = late_index {
            self.index_by(ix);
        }
        if q.autoorder.is_some() {
            self.kw(Kw::AutoOrder);
        }
        if let Some(tot) = &q.totals {
            self.totals(tot);
        }
        for b in &q.dcs {
            match &b.kind {
                DcsKind::Characteristics(c) => self.characteristics(c),
                _ => self.dcs_block(b),
            }
        }
    }

    fn select(&mut self, s: &Select, with_index: bool) {
        self.kw(Kw::Select);
        if s.allowed.is_some() {
            self.kw(Kw::Allowed);
        }
        if s.distinct.is_some() {
            self.kw(Kw::Distinct);
        }
        if let Some(top) = &s.top {
            self.kw(Kw::Top);
            self.tok(&top.count.digits);
        }
        self.items(&s.items);
        for b in &s.dcs {
            self.dcs_block(b);
        }
        if let Some(i) = &s.into {
            self.kw(match i.kind {
                IntoKind::Put => Kw::Into,
                IntoKind::Add => Kw::Add,
            });
            self.tok(&i.name.text);
        }
        if let Some(f) = &s.from {
            self.from(f);
        }
        self.clause(Kw::Where, s.where_.as_ref());
        if let Some(g) = &s.group_by {
            self.kws(&[Kw::Group, Kw::By]);
            match &g.kind {
                GroupKind::Exprs(es) => self.list(es, Self::expr),
                GroupKind::Sets(sets) => {
                    self.kws(&[Kw::Grouping, Kw::Sets]);
                    self.tok("(");
                    self.list(sets, |t, set| t.grouping_set(set));
                    self.raw(")");
                }
            }
        }
        self.clause(Kw::Having, s.having.as_ref());
        if let Some(f) = &s.for_update {
            self.kws(&[Kw::For, Kw::Update]);
            let first = f.tables.first().and_then(|p| p.parts.first());
            if first.is_some_and(|w| lookup(&w.text).0.contains(Kw::Of)) {
                self.kw(Kw::Of);
            }
            self.list(&f.tables, Self::path);
        }
        if let Some(ix) = s.index_by.as_ref().filter(|_| with_index) {
            self.index_by(ix);
        }
    }

    fn index_by(&mut self, ix: &IndexBy) {
        self.kws(&[Kw::Index, Kw::By]);
        match &ix.kind {
            IndexKind::Fields { fields, unique } => {
                self.list(fields, Self::expr);
                if unique.is_some() {
                    self.kw(Kw::Unique);
                }
            }
            IndexKind::Sets(sets) => {
                self.kw(Kw::Sets);
                self.tok("(");
                self.list(sets, |t, set| {
                    t.tok("(");
                    t.list(&set.fields, Self::expr);
                    t.raw(")");
                    if set.unique.is_some() {
                        t.kw(Kw::Unique);
                    }
                });
                self.raw(")");
            }
        }
    }

    fn grouping_set(&mut self, set: &[Expr]) {
        if let [e] = set
            && !starts_with_paren(e)
        {
            self.expr(e);
            return;
        }
        self.tok("(");
        self.list(set, Self::expr);
        self.raw(")");
    }

    fn items(&mut self, items: &[SelectItem]) {
        self.list(items, |t, it| {
            match &it.kind {
                ItemKind::Star => t.tok("*"),
                ItemKind::PathStar(p) => {
                    t.path(p);
                    t.raw(".*");
                }
                ItemKind::Expr(e) => t.expr(e),
                ItemKind::Nested { table, fields } => {
                    t.expr(table);
                    t.raw(".(");
                    t.items(fields);
                    t.raw(")");
                }
                ItemKind::EmptyTable { names, .. } => {
                    t.kw(Kw::EmptyTable);
                    t.raw(".(");
                    for (i, n) in names.iter().enumerate() {
                        if i > 0 {
                            t.raw(", ");
                        }
                        if let Some(n) = n {
                            t.raw(&n.text);
                        }
                    }
                    t.raw(")");
                }
            }
            t.alias(it.alias.as_ref(), true);
        });
    }

    fn from(&mut self, f: &FromClause) {
        self.kw(Kw::From);
        self.list(&f.sources, Self::source);
    }

    fn source(&mut self, s: &Source) {
        match &s.table {
            TableRef::Named { path, args } => {
                self.path(path);
                if let Some(a) = args {
                    self.vt_args(a);
                }
            }
            TableRef::Subquery { query, .. } => {
                self.tok("(");
                self.query(query);
                self.raw(")");
            }
            TableRef::Param(p) => self.param(p),
        }
        self.alias(s.alias.as_ref(), true);
        for j in &s.joins {
            if j.optional {
                self.tok("{");
            }
            self.kws(match j.kind {
                JoinKind::Inner => &[Kw::Inner, Kw::Join],
                JoinKind::Left => &[Kw::Left, Kw::Join],
                JoinKind::Right => &[Kw::Right, Kw::Join],
                JoinKind::Full => &[Kw::Full, Kw::Join],
            });
            self.source(&j.right);
            self.kw(Kw::On);
            self.expr(&j.on);
            if j.optional {
                self.raw("}");
            }
        }
    }

    fn vt_args(&mut self, a: &VtArgs) {
        self.raw("(");
        for (i, arg) in a.args.iter().enumerate() {
            if i > 0 {
                self.raw(",");
            }
            match arg {
                None if i > 0 => self.raw(" "),
                None => {}
                Some(arg) => {
                    self.expr(&arg.expr);
                    if let Some((d, _)) = arg.dir {
                        self.dir(d, true);
                    }
                    for g in &arg.dcs {
                        self.dcs_group(g);
                    }
                }
            }
        }
        self.raw(")");
    }

    fn totals(&mut self, tot: &Totals) {
        self.kw(Kw::Totals);
        self.list(&tot.items, |t, it| {
            t.expr(&it.expr);
            t.alias(it.alias.as_ref(), false);
        });
        self.kw(Kw::By);
        self.list(&tot.points, |t, cp| match cp {
            ControlPoint::Overall(_) => t.kw(Kw::Overall),
            ControlPoint::Field {
                expr,
                hierarchy,
                periods,
                alias,
                ..
            } => {
                t.expr(expr);
                match hierarchy {
                    Some(Hierarchy::All(_)) => t.kw(Kw::Hierarchy),
                    Some(Hierarchy::Only(_)) => t.kws(&[Kw::Only, Kw::Hierarchy]),
                    None => {}
                }
                if let Some(p) = periods {
                    t.kw(Kw::Periods);
                    t.raw("(");
                    t.tok(&p.unit.text);
                    for bound in [&p.begin, &p.end].into_iter().flatten() {
                        t.raw(",");
                        t.expr(bound);
                    }
                    t.raw(")");
                }
                t.alias(alias.as_ref(), false);
            }
        });
    }

    fn dcs_block(&mut self, b: &DcsBlock) {
        self.tok("{");
        match &b.kind {
            DcsKind::Select(l) => {
                self.kw(Kw::Select);
                self.dcs_items(l);
            }
            DcsKind::Where(l) => {
                self.kw(Kw::Where);
                self.dcs_items(l);
            }
            DcsKind::OrderBy(l) => {
                self.kws(&[Kw::Order, Kw::By]);
                self.dcs_items(l);
            }
            DcsKind::GroupBy(l) => {
                self.kws(&[Kw::Group, Kw::By]);
                self.dcs_items(l);
            }
            DcsKind::Totals(l) => {
                self.kws(&[Kw::Totals, Kw::By]);
                self.dcs_items(l);
            }
            DcsKind::Characteristics(c) => self.characteristics(c),
        }
        self.raw("}");
    }

    fn dcs_group(&mut self, l: &DcsList) {
        self.tok("{");
        self.dcs_items(l);
        self.raw("}");
    }

    fn dcs_items(&mut self, l: &DcsList) {
        self.list(&l.items, |t, it| {
            t.expr(&it.expr);
            if it.star.is_some() {
                t.raw(".*");
            }
            if let Some(n) = &it.nested {
                t.raw(".(");
                t.dcs_items(n);
                t.raw(")");
            }
            for group in &it.dcs {
                t.dcs_group(group);
            }
            if let Some((d, _)) = it.dir {
                t.dir(d, true);
            }
            t.alias(it.alias.as_ref(), false);
        });
        if let Some(DcsUnresolvedTail::AdjacentParam(parameter)) = &l.unresolved_tail {
            self.param(parameter);
        }
    }

    fn characteristics(&mut self, c: &Characteristics) {
        self.kws(&[Kw::Characteristics, Kw::Type]);
        self.raw("(");
        self.path(&c.type_name);
        self.raw(")");
        self.kw(Kw::CharacteristicTypes);
        self.char_source(&c.kinds);
        self.field(Kw::KeyField, Some(&c.key_field));
        self.field(Kw::NameField, Some(&c.name_field));
        self.field(Kw::ValueTypeField, c.value_type_field.as_ref());
        self.kw(Kw::CharacteristicValues);
        self.char_source(&c.values);
        self.field(Kw::ObjectField, Some(&c.object_field));
        self.field(Kw::TypeField, Some(&c.kind_field));
        self.field(Kw::ValueField, c.value_field.as_ref());
    }

    fn field(&mut self, word: Kw, p: Option<&Path>) {
        if let Some(p) = p {
            self.kw(word);
            self.path(p);
        }
    }

    fn char_source(&mut self, s: &CharSource) {
        match s {
            CharSource::Table(p) => self.path(p),
            CharSource::Query(q) => {
                self.tok("(");
                self.query(q);
                self.raw(")");
            }
        }
    }

    fn operand(&mut self, e: &Expr, min: Prec) {
        if prec(e) < min {
            self.tok("(");
            self.expr(e);
            self.raw(")");
        } else {
            self.expr(e);
        }
    }

    fn expr(&mut self, e: &Expr) {
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
                let min = chain_operand(prec(e));
                self.operand(first, min);
                for op in rest {
                    match op.op {
                        BinOp::Add => self.tok("+"),
                        BinOp::Sub => self.tok("-"),
                        BinOp::Mul => self.tok("*"),
                        BinOp::Div => self.tok("/"),
                        BinOp::And => self.kw(Kw::And),
                        BinOp::Or => self.kw(Kw::Or),
                    }
                    self.operand(&op.expr, min);
                }
            }
            ExprKind::Unary { op, expr, .. } => match op {
                UnOp::Not => {
                    self.kw(Kw::Not);
                    self.operand(expr, Prec::Not);
                }
                UnOp::Neg | UnOp::Plus => {
                    self.tok(if *op == UnOp::Neg { "-" } else { "+" });
                    self.glue = !matches!(
                        expr.kind,
                        ExprKind::Unary {
                            op: UnOp::Neg | UnOp::Plus,
                            ..
                        }
                    );
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
                self.operand(l, Prec::Cmp);
                if *not {
                    self.kw(Kw::Not);
                }
                self.kw(Kw::In);
                if *hierarchy {
                    self.kw(Kw::InHierarchy);
                }
                self.tok("(");
                match rhs {
                    InRhs::List(es) => self.list(es, Self::expr),
                    InRhs::Query(q) => self.query(q),
                }
                self.raw(")");
            }
            ExprKind::Between { l, not, lo, hi, .. } => {
                if bare_not_operand(l) {
                    self.expr(l);
                } else {
                    self.operand(l, Prec::Cmp);
                }
                if *not {
                    self.kw(Kw::Not);
                }
                self.kw(Kw::Between);
                self.operand(lo, Prec::Add);
                self.kw(Kw::And);
                self.operand(hi, Prec::Add);
            }
            ExprKind::IsNull { l, not, .. } => {
                self.operand(l, Prec::Cmp);
                self.kw(Kw::Is);
                if *not {
                    self.kw(Kw::Not);
                }
                self.kw(Kw::Null);
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
                self.operand(l, Prec::Cmp);
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
                self.list(es, Self::expr);
                self.raw(")");
            }
            ExprKind::Case {
                operand,
                whens,
                else_,
                ..
            } => {
                self.kw(Kw::Case);
                if let Some(x) = operand {
                    self.expr(x);
                }
                for w in whens {
                    self.kw(Kw::When);
                    self.expr(&w.cond);
                    self.kw(Kw::Then);
                    self.expr(&w.then);
                }
                if let Some(x) = else_ {
                    self.kw(Kw::Else);
                    self.expr(x);
                }
                self.kw(Kw::End);
            }
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
                self.raw("(");
                self.expr(expr);
                self.kw(Kw::As);
                self.cast_type(ty);
                self.raw(")");
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
                self.tok(func.name(ScriptVariant::Russian));
                self.raw("(");
                if distinct.is_some() {
                    self.kw(Kw::Distinct);
                }
                match args {
                    FuncArgs::Star(_) => self.tok("*"),
                    FuncArgs::List(es) => self.list(es, Self::expr),
                }
                self.raw(")");
            }
            ExprKind::TypeOf { name, .. } => self.word_call(Kw::Type, |t| t.path(name)),
            ExprKind::Value { path, .. } => self.word_call(Kw::Value, |t| t.path(path)),
            ExprKind::DateTime { args, .. } => {
                self.word_call(Kw::DateTime, |t| t.list(args, Self::expr))
            }
            ExprKind::Paren(inner) => {
                self.tok("(");
                self.expr(inner);
                self.raw(")");
            }
            ExprKind::Dcs(l) => self.dcs_group(l),
        }
    }

    fn word_call(&mut self, word: Kw, body: impl FnOnce(&mut Self)) {
        self.kw(word);
        self.raw("(");
        body(self);
        self.raw(")");
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
}

#[derive(Default)]
struct W(String);

impl W {
    fn sep(&mut self) {
        if !self.0.is_empty() && !self.0.ends_with('(') {
            self.0.push(' ');
        }
    }
    fn atom(&mut self, s: &str) {
        self.sep();
        self.0.push_str(s);
    }
    fn open(&mut self, head: &str) {
        self.sep();
        self.0.push('(');
        self.0.push_str(head);
    }
    fn close(&mut self) {
        self.0.push(')');
    }
    fn list(&mut self, head: &str, body: impl FnOnce(&mut Self)) {
        self.open(head);
        body(self);
        self.close();
    }
    fn path(&mut self, p: &Path) {
        self.sep();
        for (i, part) in p.parts.iter().enumerate() {
            if i > 0 {
                self.0.push('.');
            }
            self.0.push_str(&part.text);
        }
    }
    fn string(&mut self, s: &str) {
        self.sep();
        self.0.push('"');
        for c in s.chars() {
            if c == '"' {
                self.0.push('"');
            }
            self.0.push(c);
        }
        self.0.push('"');
    }
    fn param(&mut self, p: &Param) {
        self.sep();
        self.0.push('&');
        self.0.push_str(&p.name.text);
    }
    fn alias(&mut self, a: Option<&Alias>) {
        if let Some(a) = a {
            self.atom(if a.as_kw.is_some() { ":as" } else { ":alias" });
            self.atom(&a.name.text);
        }
    }
    fn as_alias(&mut self, a: Option<&Alias>) {
        if let Some(a) = a {
            self.atom(":as");
            self.atom(&a.name.text);
        }
    }
    fn exprs(&mut self, es: &[Expr]) {
        for e in es {
            self.expr(e);
        }
    }
    fn clause(&mut self, head: &str, c: Option<&Clause>) {
        if let Some(c) = c {
            self.list(head, |w| w.expr(&c.expr));
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Query(q) => self.query(&q.query),
            Stmt::Drop(d) => self.list("drop", |w| w.atom(&d.name.text)),
        }
    }

    fn query(&mut self, q: &Query) {
        self.open("query");
        for (i, part) in q.parts.iter().enumerate() {
            if let Some(u) = i.checked_sub(1).and_then(|j| q.unions.get(j)) {
                self.atom(if u.all { "union-all" } else { "union" });
            }
            self.select(part);
        }
        if let Some(o) = &q.order_by {
            self.list("order-by", |w| {
                for it in &o.items {
                    w.ordered(it.dir, &it.expr);
                }
            });
        }
        if q.autoorder.is_some() {
            self.atom("autoorder");
        }
        if let Some(t) = &q.totals {
            self.totals(t);
        }
        self.dcs_blocks(&q.dcs);
        self.close();
    }

    fn select(&mut self, s: &Select) {
        self.open("select");
        if s.allowed.is_some() {
            self.atom("allowed");
        }
        if s.distinct.is_some() {
            self.atom("distinct");
        }
        if let Some(t) = &s.top {
            self.list("top", |w| w.atom(&t.count.digits));
        }
        self.list("items", |w| w.items(&s.items));
        if let Some(i) = &s.into {
            let head = match i.kind {
                IntoKind::Put => "into",
                IntoKind::Add => "add",
            };
            self.list(head, |w| w.atom(&i.name.text));
        }
        if let Some(f) = &s.from {
            self.from(f);
        }
        self.clause("where", s.where_.as_ref());
        if let Some(g) = &s.group_by {
            match &g.kind {
                GroupKind::Exprs(es) => self.list("group-by", |w| w.exprs(es)),
                GroupKind::Sets(sets) => self.list("group-by-sets", |w| {
                    for set in sets {
                        w.list("", |w| w.exprs(set));
                    }
                }),
            }
        }
        self.clause("having", s.having.as_ref());
        if let Some(f) = &s.for_update {
            self.list("for-update", |w| {
                for p in &f.tables {
                    w.path(p);
                }
            });
        }
        if let Some(ix) = &s.index_by {
            match &ix.kind {
                IndexKind::Fields { fields, unique } => self.list("index-by", |w| {
                    if unique.is_some() {
                        w.atom("unique");
                    }
                    w.exprs(fields);
                }),
                IndexKind::Sets(sets) => self.list("index-by-sets", |w| {
                    for set in sets {
                        w.list("", |w| {
                            if set.unique.is_some() {
                                w.atom("unique");
                            }
                            w.exprs(&set.fields);
                        });
                    }
                }),
            }
        }
        self.dcs_blocks(&s.dcs);
        self.close();
    }

    fn items(&mut self, items: &[SelectItem]) {
        for it in items {
            self.open("item");
            match &it.kind {
                ItemKind::Star => self.atom("*"),
                ItemKind::PathStar(p) => self.list("star", |w| w.path(p)),
                ItemKind::Expr(e) => self.expr(e),
                ItemKind::Nested { table, fields } => self.list("nested", |w| {
                    w.expr(table);
                    w.items(fields);
                }),
                ItemKind::EmptyTable { names, .. } => self.list("empty-table", |w| {
                    for n in names {
                        w.atom(n.as_ref().map_or("_", |n| &*n.text));
                    }
                }),
            }
            self.alias(it.alias.as_ref());
            self.close();
        }
    }

    fn from(&mut self, f: &FromClause) {
        self.list("from", |w| {
            for s in &f.sources {
                w.source(s);
            }
        });
    }

    fn source(&mut self, s: &Source) {
        self.open("src");
        match &s.table {
            TableRef::Named { path, args: None } => self.path(path),
            TableRef::Named {
                path,
                args: Some(args),
            } => self.list("vt", |w| {
                w.path(path);
                for a in &args.args {
                    match a {
                        None => w.atom("_"),
                        Some(a) if a.dcs.is_empty() => w.vt_arg(a),
                        Some(a) => w.list("dcs-arg", |w| {
                            w.vt_arg(a);
                            for g in &a.dcs {
                                w.dcs_list("dcs", g);
                            }
                        }),
                    }
                }
            }),
            TableRef::Subquery { query, .. } => self.list("sub", |w| w.query(query)),
            TableRef::Param(p) => self.param(p),
        }
        self.alias(s.alias.as_ref());
        for j in &s.joins {
            self.list(if j.optional { "opt-join" } else { "join" }, |w| {
                w.atom(match j.kind {
                    JoinKind::Inner => "inner",
                    JoinKind::Left => "left",
                    JoinKind::Right => "right",
                    JoinKind::Full => "full",
                });
                w.source(&j.right);
                w.expr(&j.on);
            });
        }
        self.close();
    }

    fn vt_arg(&mut self, a: &VtArg) {
        match a.dir {
            None => self.expr(&a.expr),
            Some((d, _)) => self.ordered(d, &a.expr),
        }
    }

    fn ordered(&mut self, dir: OrderDir, e: &Expr) {
        self.list(order_dir(dir), |w| w.expr(e));
    }

    fn totals(&mut self, t: &Totals) {
        self.list("totals", |w| {
            w.list("items", |w| {
                for it in &t.items {
                    w.list("titem", |w| {
                        w.expr(&it.expr);
                        w.as_alias(it.alias.as_ref());
                    });
                }
            });
            w.list("by", |w| {
                for cp in &t.points {
                    match cp {
                        ControlPoint::Overall(_) => w.atom("overall"),
                        ControlPoint::Field {
                            expr,
                            hierarchy,
                            periods,
                            alias,
                            ..
                        } => w.list("cp", |w| {
                            w.expr(expr);
                            match hierarchy {
                                Some(Hierarchy::All(_)) => w.atom("hier"),
                                Some(Hierarchy::Only(_)) => w.atom("only-hier"),
                                None => {}
                            }
                            if let Some(p) = periods {
                                w.list("periods", |w| {
                                    w.atom(&p.unit.text);
                                    if let Some(b) = &p.begin {
                                        w.expr(b);
                                    }
                                    if let Some(e) = &p.end {
                                        w.expr(e);
                                    }
                                });
                            }
                            w.as_alias(alias.as_ref());
                        }),
                    }
                }
            });
        });
    }

    fn dcs_blocks(&mut self, blocks: &[DcsBlock]) {
        for b in blocks {
            match &b.kind {
                DcsKind::Select(l) => self.dcs_list("dcs-select", l),
                DcsKind::Where(l) => self.dcs_list("dcs-where", l),
                DcsKind::OrderBy(l) => self.dcs_list("dcs-order-by", l),
                DcsKind::GroupBy(l) => self.dcs_list("dcs-group-by", l),
                DcsKind::Totals(l) => self.dcs_list("dcs-totals", l),
                DcsKind::Characteristics(c) => self.characteristics(c),
            }
        }
    }

    fn dcs_list(&mut self, head: &str, l: &DcsList) {
        self.list(head, |w| {
            for it in &l.items {
                w.list("ditem", |w| {
                    w.expr(&it.expr);
                    if it.star.is_some() {
                        w.atom(".*");
                    }
                    if let Some(n) = &it.nested {
                        w.dcs_list("nested", n);
                    }
                    for group in &it.dcs {
                        w.dcs_list("dcs", group);
                    }
                    if let Some((d, _)) = it.dir {
                        w.atom(order_dir(d));
                    }
                    w.as_alias(it.alias.as_ref());
                });
            }
            if let Some(DcsUnresolvedTail::AdjacentParam(parameter)) = &l.unresolved_tail {
                w.list("unresolved-adjacent-param", |w| {
                    w.atom(&parameter.name.text)
                });
            }
        });
    }

    fn characteristics(&mut self, c: &Characteristics) {
        self.list("characteristics", |w| {
            w.path(&c.type_name);
            w.char_source(&c.kinds);
            w.path(&c.key_field);
            w.path(&c.name_field);
            w.opt_path(c.value_type_field.as_ref());
            w.char_source(&c.values);
            w.path(&c.object_field);
            w.path(&c.kind_field);
            w.opt_path(c.value_field.as_ref());
        });
    }

    fn char_source(&mut self, s: &CharSource) {
        match s {
            CharSource::Table(p) => self.path(p),
            CharSource::Query(q) => self.list("sub", |w| w.query(q)),
        }
    }

    fn opt_path(&mut self, p: Option<&Path>) {
        match p {
            Some(p) => self.path(p),
            None => self.atom("_"),
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Lit(l) => match l {
                Lit::Number(n) => self.atom(&n.digits),
                Lit::String(s) => self.string(s),
                Lit::Bool(true) => self.atom("true"),
                Lit::Bool(false) => self.atom("false"),
                Lit::Null => self.atom("null"),
                Lit::Undefined => self.atom("undefined"),
            },
            ExprKind::Param(p) => self.param(p),
            ExprKind::Path(p) => self.path(p),
            ExprKind::Chain { first, rest } => {
                for op in rest.iter().rev() {
                    self.open(bin_op(op.op));
                }
                self.expr(first);
                for op in rest {
                    self.expr(&op.expr);
                    self.close();
                }
            }
            ExprKind::Unary { op, expr, .. } => {
                let head = match op {
                    UnOp::Neg => "neg",
                    UnOp::Plus => "pos",
                    UnOp::Not => "not",
                };
                self.list(head, |w| w.expr(expr));
            }
            ExprKind::Compare { op, l, r, .. } => self.list(cmp_op(*op), |w| {
                w.expr(l);
                w.expr(r);
            }),
            ExprKind::In {
                l,
                not,
                hierarchy,
                rhs,
                ..
            } => {
                let head = match (not, hierarchy) {
                    (false, false) => "in",
                    (true, false) => "not-in",
                    (false, true) => "in-hier",
                    (true, true) => "not-in-hier",
                };
                self.list(head, |w| {
                    w.expr(l);
                    match rhs {
                        InRhs::List(es) => w.list("list", |w| w.exprs(es)),
                        InRhs::Query(q) => w.list("sub", |w| w.query(q)),
                    }
                });
            }
            ExprKind::Between { l, not, lo, hi, .. } => {
                self.list(if *not { "not-between" } else { "between" }, |w| {
                    w.expr(l);
                    w.expr(lo);
                    w.expr(hi);
                });
            }
            ExprKind::IsNull { l, not, .. } => {
                self.list(if *not { "is-not-null" } else { "is-null" }, |w| w.expr(l));
            }
            ExprKind::Refs { l, table, .. } => self.list("refs", |w| {
                w.expr(l);
                w.path(table);
            }),
            ExprKind::Like {
                l,
                not,
                pattern,
                escape,
                ..
            } => self.list(if *not { "not-like" } else { "like" }, |w| {
                w.expr(l);
                w.expr(pattern);
                if let Some(esc) = escape {
                    w.list("escape", |w| w.string(&esc.ch));
                }
            }),
            ExprKind::Tuple(es) => self.list("tuple", |w| w.exprs(es)),
            ExprKind::Case {
                operand,
                whens,
                else_,
                ..
            } => {
                self.open(if operand.is_some() { "case-of" } else { "case" });
                if let Some(x) = operand {
                    self.expr(x);
                }
                for wh in whens {
                    self.list("when", |w| {
                        w.expr(&wh.cond);
                        w.expr(&wh.then);
                    });
                }
                if let Some(e) = else_ {
                    self.list("else", |w| w.expr(e));
                }
                self.close();
            }
            ExprKind::Deref { expr, deref } => self.list("deref", |w| {
                w.expr(expr);
                w.sep();
                for (i, name) in deref.iter().enumerate() {
                    if i > 0 {
                        w.0.push('.');
                    }
                    w.0.push_str(&name.text);
                }
            }),
            ExprKind::Cast {
                expr, ty, deref, ..
            } => {
                if deref.is_empty() {
                    self.cast(expr, ty);
                } else {
                    self.list("deref", |w| {
                        w.cast(expr, ty);
                        w.sep();
                        for (i, name) in deref.iter().enumerate() {
                            if i > 0 {
                                w.0.push('.');
                            }
                            w.0.push_str(&name.text);
                        }
                    });
                }
            }
            ExprKind::Func {
                func,
                distinct,
                args,
                ..
            } => self.list(func.canonical(), |w| {
                if distinct.is_some() {
                    w.atom("distinct");
                }
                match args {
                    FuncArgs::Star(_) => w.atom("*"),
                    FuncArgs::List(es) => w.exprs(es),
                }
            }),
            ExprKind::TypeOf { name, .. } => self.list("type", |w| w.path(name)),
            ExprKind::Value { path, .. } => self.list("value", |w| w.path(path)),
            ExprKind::DateTime { args, .. } => self.list("datetime", |w| w.exprs(args)),
            ExprKind::Paren(inner) => self.expr(inner),
            ExprKind::Dcs(l) => self.dcs_list("dcs", l),
        }
    }

    fn cast(&mut self, e: &Expr, ty: &CastType) {
        self.list("cast", |w| {
            w.expr(e);
            match ty {
                CastType::Boolean => w.atom("bool"),
                CastType::Date => w.atom("date"),
                CastType::Number { len, scale } => w.list("number", |w| {
                    if let Some(l) = len {
                        w.atom(&l.digits);
                    }
                    if let Some(s) = scale {
                        w.atom(&s.digits);
                    }
                }),
                CastType::String { len } => w.list("string", |w| {
                    if let Some(l) = len {
                        w.atom(&l.digits);
                    }
                }),
                CastType::Table(p) => w.path(p),
            }
        });
    }
}

fn order_dir(d: OrderDir) -> &'static str {
    match d {
        OrderDir::Asc => "asc",
        OrderDir::Desc => "desc",
        OrderDir::Hierarchy => "hier",
        OrderDir::HierarchyDesc => "hier-desc",
    }
}

fn bin_op(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::And => "and",
        BinOp::Or => "or",
    }
}

fn cmp_op(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "=",
        CmpOp::Ne => "<>",
        CmpOp::Lt => "<",
        CmpOp::Gt => ">",
        CmpOp::Le => "<=",
        CmpOp::Ge => ">=",
    }
}
