use super::{ColSel, Expr, ExprKind, Rel, SrcKind, StmtS};
use crate::QuerySchema;
use crate::schema::ValuePath;
use crate::types::{DateQ, QType};
use crate::value::QValue;
use query_syntax::ast::{BinOp, CmpOp, JoinKind};
use query_syntax::with_deep_stack;
use std::fmt::Write;

pub fn print(s: &StmtS, schema: &QuerySchema) -> String {
    with_deep_stack(s.max_depth, || {
        let mut p = Printer {
            s,
            schema,
            out: String::new(),
        };
        p.rel(&s.root);
        for n in &s.nested {
            let _ = write!(p.out, " (nested out{}", n.col);
            if let Some(r) = &n.rows {
                p.w(" ");
                p.rel(&r.scan);
                p.w(" ");
                p.expr(&r.owner);
                p.w(" ");
                p.expr(&r.number);
                for e in &r.exprs {
                    p.w(" ");
                    p.expr(e);
                }
            }
            p.w(")");
        }
        p.out
    })
}

struct Printer<'a> {
    s: &'a StmtS,
    schema: &'a QuerySchema,
    out: String,
}

impl Printer<'_> {
    fn w(&mut self, t: &str) {
        self.out.push_str(t);
    }

    fn rel(&mut self, r: &Rel) {
        match r {
            Rel::Empty => self.w("(empty)"),
            Rel::Scan { src, table } => {
                let name = &self.schema.table(*table).name_ru;
                let _ = write!(self.out, "(scan s{} {name})", src.0);
            }
            Rel::TempScan { src, name } => {
                let _ = write!(self.out, "(temp s{} {name})", src.0);
            }
            Rel::ParamScan { src, param } => {
                let _ = write!(self.out, "(param-scan s{} p{param})", src.0);
            }
            Rel::Subquery { src, input } => {
                let _ = write!(self.out, "(subq s{} ", src.0);
                self.rel(input);
                self.w(")");
            }
            Rel::Join { kind, l, r, on } => {
                let k = match kind {
                    JoinKind::Inner => "inner",
                    JoinKind::Left => "left",
                    JoinKind::Right => "right",
                    JoinKind::Full => "full",
                };
                let _ = write!(self.out, "(join {k} ");
                self.rel(l);
                self.w(" ");
                self.rel(r);
                self.w(" ");
                self.expr(on);
                self.w(")");
            }
            Rel::Filter { input, pred } => {
                self.w("(filter ");
                self.rel(input);
                self.w(" ");
                self.expr(pred);
                self.w(")");
            }
            Rel::Aggregate {
                input,
                keys,
                sets,
                aggs,
            } => {
                self.w("(agg ");
                self.rel(input);
                self.w(" (keys");
                for k in keys {
                    self.w(" ");
                    self.expr(k);
                }
                self.w(")");
                if let Some(sets) = sets {
                    self.w(" (sets");
                    for set in sets {
                        self.w(" (");
                        for (i, k) in set.iter().enumerate() {
                            if i > 0 {
                                self.w(" ");
                            }
                            let _ = write!(self.out, "{k}");
                        }
                        self.w(")");
                    }
                    self.w(")");
                }
                self.w(" (aggs");
                for a in aggs {
                    let _ = write!(self.out, " ({}", a.f.ru());
                    if a.distinct {
                        self.w(" distinct");
                    }
                    match &a.arg {
                        Some(e) => {
                            self.w(" ");
                            self.expr(e);
                        }
                        None => self.w(" *"),
                    }
                    self.w(")");
                }
                self.w("))");
            }
            Rel::Project { input, exprs } => {
                self.w("(project ");
                self.rel(input);
                for e in exprs {
                    self.w(" ");
                    self.expr(e);
                }
                self.w(")");
            }
            Rel::Distinct(x) => {
                self.w("(distinct ");
                self.rel(x);
                self.w(")");
            }
            Rel::Sort { input, keys } => {
                self.w("(sort ");
                self.rel(input);
                for k in keys {
                    self.w(" (");
                    self.expr(&k.expr);
                    self.w(if k.desc { " desc)" } else { " asc)" });
                }
                self.w(")");
            }
            Rel::Hierarchy {
                input,
                id,
                parent,
                keys,
            } => {
                let _ = write!(self.out, "(hier out{id} out{parent} ");
                self.rel(input);
                for k in keys {
                    self.w(" (");
                    self.expr(&k.expr);
                    self.w(if k.desc { " desc)" } else { " asc)" });
                }
                self.w(")");
            }
            Rel::Top { input, n } => {
                let _ = write!(self.out, "(top {n} ");
                self.rel(input);
                self.w(")");
            }
            Rel::Union { parts, all } => {
                self.w("(union");
                if all.iter().all(|a| *a) {
                    self.w(" all");
                } else if all.iter().any(|a| *a) {
                    self.w(" mixed:");
                    for (i, a) in all.iter().enumerate() {
                        if i > 0 {
                            self.w(",");
                        }
                        self.w(if *a { "all" } else { "distinct" });
                    }
                }
                for p in parts {
                    self.w(" ");
                    self.rel(p);
                }
                self.w(")");
            }
            Rel::RegisterVt(v) | Rel::Slice(v) | Rel::RunningBalance(v) => {
                let name = &self.schema.table(v.table).name_ru;
                let _ = write!(
                    self.out,
                    "(register-vt s{} {name} inner s{}",
                    v.src.0, v.inner.0
                );
                for (i, b) in v.bounds.iter().enumerate() {
                    if let Some(b) = b {
                        let _ = write!(self.out, " bound{i}=");
                        self.expr(b);
                    }
                }
                let _ = write!(self.out, " periodicity={:?}", v.periodicity);
                if let Some(c) = v.complement {
                    let _ = write!(self.out, " complement={c:?}");
                }
                if let Some(c) = &v.cond {
                    self.w(" cond=");
                    self.expr(c);
                }
                self.w(")");
            }
        }
    }

    fn col(&mut self, src: u32, col: ColSel) {
        match col {
            ColSel::Column(i) => {
                let _ = write!(self.out, "s{src}#{i}");
            }
            ColSel::Field(f) => {
                let name = match self.s.sources.get(src as usize).map(|x| &x.kind) {
                    Some(SrcKind::Table(t)) => self.schema.field(*t, f).map(|x| x.name_ru.as_str()),
                    _ => None,
                };
                match name {
                    Some(n) => {
                        let _ = write!(self.out, "s{src}.{n}");
                    }
                    None => {
                        let _ = write!(self.out, "s{src}.#{}", f.0);
                    }
                }
            }
        }
    }

    fn list(&mut self, v: &[Expr]) {
        for (i, e) in v.iter().enumerate() {
            if i > 0 {
                self.w(" ");
            }
            self.expr(e);
        }
    }

    fn lhs(&mut self, l: &[Expr]) {
        if l.len() == 1 {
            self.list(l);
        } else {
            self.w("(");
            self.list(l);
            self.w(")");
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Col { src, col } => self.col(src.0, *col),
            ExprKind::Deref { base, steps } => {
                self.w("deref(");
                self.expr(base);
                for s in steps {
                    self.w(",");
                    self.w(&s.name);
                }
                self.w(")");
            }
            ExprKind::Lit(v) => self.lit(v),
            ExprKind::Param(i) => {
                let name = self
                    .s
                    .params
                    .get(usize::from(*i))
                    .map_or("?", |s| s.as_str());
                let _ = write!(self.out, "&{name}");
            }
            ExprKind::Value(i) => match self.s.values.get(usize::from(*i)) {
                Some(p) => self.value(*i, p),
                None => {
                    let _ = write!(self.out, "val{i}");
                }
            },
            ExprKind::KeyRef(i) => {
                let _ = write!(self.out, "key{i}");
            }
            ExprKind::AggRef(i) => {
                let _ = write!(self.out, "agg{i}");
            }
            ExprKind::OuterKey { src, key, .. } => {
                let _ = write!(self.out, "s{}.key{key}", src.0);
            }
            ExprKind::Out(i) => {
                let _ = write!(self.out, "out{i}");
            }
            ExprKind::Neg(x) => self.op("neg", &[x]),
            ExprKind::Not(x) => self.op("not", &[x]),
            ExprKind::Arith { first, steps } => {
                for (op, _, _) in steps.iter().rev() {
                    let _ = write!(self.out, "({} ", bin_op(*op));
                }
                self.expr(first);
                for (_, rhs, _) in steps {
                    self.w(" ");
                    self.expr(rhs);
                    self.w(")");
                }
            }
            ExprKind::And(v) => {
                self.w("(and ");
                self.list(v);
                self.w(")");
            }
            ExprKind::Or(v) => {
                self.w("(or ");
                self.list(v);
                self.w(")");
            }
            ExprKind::Compare(op, a, b) => self.op(cmp_op(*op), &[a, b]),
            ExprKind::InList {
                l,
                not,
                hierarchy,
                rhs,
            } => {
                self.w(match (*not, *hierarchy) {
                    (false, false) => "(in ",
                    (true, false) => "(not-in ",
                    (false, true) => "(in-hierarchy ",
                    (true, true) => "(not-in-hierarchy ",
                });
                self.lhs(l);
                self.w(" (list ");
                self.list(rhs);
                self.w("))");
            }
            ExprKind::InQuery {
                l,
                not,
                hierarchy,
                q,
            } => {
                self.w(match (*not, *hierarchy) {
                    (false, false) => "(in ",
                    (true, false) => "(not-in ",
                    (false, true) => "(in-hierarchy ",
                    (true, true) => "(not-in-hierarchy ",
                });
                self.lhs(l);
                self.w(" ");
                self.rel(q);
                self.w(")");
            }
            ExprKind::SectionAgg { f, owner, rel } => {
                let _ = write!(self.out, "(section-agg {} ", f.ru());
                self.expr(owner);
                self.w(" ");
                self.rel(rel);
                self.w(")");
            }
            ExprKind::Between { l, not, lo, hi } => {
                self.op(if *not { "not-between" } else { "between" }, &[l, lo, hi]);
            }
            ExprKind::IsNull { l, not } => {
                self.op(if *not { "not-null" } else { "is-null" }, &[l]);
            }
            ExprKind::Refs { l, table } => {
                self.w("(refs ");
                self.expr(l);
                let name = &self.schema.table(*table).name_ru;
                let _ = write!(self.out, " {name})");
            }
            ExprKind::Like { l, not, pat, esc } => {
                self.w(if *not { "(not-like " } else { "(like " });
                self.expr(l);
                self.w(" ");
                self.expr(pat);
                if let Some(c) = esc {
                    let _ = write!(self.out, " esc '{c}'");
                }
                self.w(")");
            }
            ExprKind::Case {
                operand,
                whens,
                else_,
            } => {
                self.w("(case");
                if let Some(o) = operand {
                    self.w(" ");
                    self.expr(o);
                }
                for (c, r) in whens {
                    self.w(" (when ");
                    self.expr(c);
                    self.w(" ");
                    self.expr(r);
                    self.w(")");
                }
                if let Some(x) = else_ {
                    self.w(" (else ");
                    self.expr(x);
                    self.w(")");
                }
                self.w(")");
            }
            ExprKind::Cast { x, to, num } => {
                self.w("(cast ");
                self.expr(x);
                self.w(" ");
                let t = self.type_name(to, *num);
                self.w(&t);
                self.w(")");
            }
            ExprKind::Func { f, args } => {
                let _ = write!(self.out, "({}", f.ru());
                for a in args {
                    self.w(" ");
                    self.expr(a);
                }
                self.w(")");
            }
            ExprKind::Unit(k) => self.w(k.ru()),
        }
    }

    fn op(&mut self, name: &str, args: &[&Expr]) {
        let _ = write!(self.out, "({name}");
        for a in args {
            self.w(" ");
            self.expr(a);
        }
        self.w(")");
    }

    fn value(&mut self, slot: u16, p: &ValuePath) {
        match p {
            ValuePath::Predefined { table, name } => {
                let t = &self.schema.table(*table).name_ru;
                let _ = write!(self.out, "ЗНАЧЕНИЕ({t}.{name})");
            }
            ValuePath::EmptyRef(table) => {
                let t = &self.schema.table(*table).name_ru;
                let _ = write!(self.out, "ЗНАЧЕНИЕ({t}.ПустаяСсылка)");
            }
            ValuePath::TypeRef(table) => {
                let t = &self.schema.table(*table).name_ru;
                let _ = write!(self.out, "ЗНАЧЕНИЕ({t})");
            }
            _ => {
                let _ = write!(self.out, "val{slot}");
            }
        }
    }

    fn lit(&mut self, v: &QValue) {
        let t = lit_text(self.schema, v);
        self.w(&t);
    }

    fn type_name(&self, t: &QType, num: Option<crate::types::NumQ>) -> String {
        type_text(self.schema, t, num)
    }
}

pub(crate) fn lit_text(schema: &QuerySchema, v: &QValue) -> String {
    let mut out = String::new();
    match v {
        QValue::Null => out.push_str("NULL"),
        QValue::Undefined => out.push_str("Неопределено"),
        QValue::Bool(b) => out.push_str(if *b { "Истина" } else { "Ложь" }),
        QValue::Number(n) => {
            let _ = write!(out, "{n}");
        }
        QValue::String(s) => {
            let t = s.to_string_lossy().replace('"', "\"\"");
            let _ = write!(out, "\"{t}\"");
        }
        QValue::Date(d) => {
            let p = d.parts();
            let _ = write!(
                out,
                "'{:04}-{:02}-{:02}T{:02}:{:02}:{:02}'",
                p.year, p.month, p.day, p.hour, p.minute, p.second
            );
        }
        QValue::Uuid(b) => {
            out.push_str("uuid:");
            for x in b {
                let _ = write!(out, "{x:02x}");
            }
        }
        QValue::Ref { table, id } => {
            let _ = write!(out, "ref{table}:");
            for x in id {
                let _ = write!(out, "{x:02x}");
            }
        }
        QValue::Type(t) => {
            let n = type_text(schema, t, None);
            let _ = write!(out, "Тип({n})");
        }
        QValue::Binary(b) => {
            let _ = write!(out, "binary[{}]", b.len());
        }
        QValue::Storage(b) => {
            let _ = write!(out, "storage[{}]", b.len());
        }
        QValue::SysEnum(t, i) => {
            let _ = write!(out, "sysenum({},{i})", t.0);
        }
        QValue::Nested(n) => {
            let _ = write!(out, "nested[{}x{}]", n.columns.len(), n.rows.len());
        }
        QValue::PointInTime(m) => {
            let date = lit_text(schema, &QValue::Date(m.date));
            match &m.reference {
                Some((table, id)) => {
                    let r = lit_text(
                        schema,
                        &QValue::Ref {
                            table: *table,
                            id: *id,
                        },
                    );
                    let _ = write!(out, "МоментВремени({date}, {r})");
                }
                None => {
                    let _ = write!(out, "МоментВремени({date})");
                }
            }
        }
        QValue::Boundary(b) => {
            let point = lit_text(schema, &b.point);
            let kind = if b.include {
                "Включая"
            } else {
                "Исключая"
            };
            let _ = write!(out, "Граница({point}, {kind})");
        }
    }
    out
}

pub(crate) fn type_text(
    schema: &QuerySchema,
    t: &QType,
    num: Option<crate::types::NumQ>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if t.boolean {
        parts.push("Булево".into());
    }
    if let Some(n) = num.or(t.number) {
        if n.digits == 0 && n.scale == 0 {
            parts.push("Число".into());
        } else {
            parts.push(format!("Число({},{})", n.digits, n.scale));
        }
    }
    if let Some(s) = t.string {
        if s.len == 0 {
            parts.push("Строка".into());
        } else {
            parts.push(format!("Строка({})", s.len));
        }
    }
    if let Some(d) = t.date {
        parts.push(
            match d {
                DateQ::Date => "Дата(Дата)",
                DateQ::Time => "Дата(Время)",
                DateQ::DateTime => "Дата",
            }
            .into(),
        );
    }
    for r in &t.refs {
        parts.push(schema.ref_info(*r).xml_name.clone());
    }
    for o in &t.others {
        parts.push(format!("{o:?}"));
    }
    if t.null {
        parts.push("Null".into());
    }
    parts.join("|")
}

pub(crate) fn bin_op(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::And => "and",
        BinOp::Or => "or",
    }
}

pub(crate) fn cmp_op(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "=",
        CmpOp::Ne => "<>",
        CmpOp::Lt => "<",
        CmpOp::Gt => ">",
        CmpOp::Le => "<=",
        CmpOp::Ge => ">=",
    }
}
