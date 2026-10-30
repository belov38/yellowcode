use crate::model::*;
use bsl_prim::fold_name;
use query_ir::tir::{self, Col};
use query_ir::{CompiledStmt, QTableId, QuerySchema, TableKind};
use query_syntax::ScriptVariant;
use query_syntax::ast::{self, CastType, Expr, ExprKind, FuncArgs, Ident, InRhs, ItemKind};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

type Key = (u32, u32);

struct FieldName {
    alias: Option<String>,
    tail: Vec<Option<String>>,
    column_case_only: bool,
}

#[derive(Default)]
struct Renames {
    fields: HashMap<Key, FieldName>,
    casts: HashMap<Key, Vec<Option<String>>>,
}

fn key(s: query_syntax::Span) -> Key {
    (s.start, s.end)
}

pub(crate) fn apply(schema: &mut Schema, compiled: &[Arc<CompiledStmt>], qs: &QuerySchema) {
    let ru = qs.script() == ScriptVariant::Russian;
    let ordinary_query = schema.mode == query_syntax::ParseMode::Query;
    for (st, c) in schema.batch.iter_mut().zip(compiled) {
        let Stmt::Select(q) = st else { continue };
        let mut collect = Collect {
            qs,
            ru,
            ordinary_query,
            qualify_only: false,
            out: Renames::default(),
            implicit: HashSet::new(),
        };
        if let tir::StmtKind::Query(tq) = &c.typed.kind {
            crate::names::implicit_sources(q, tq, &mut collect.implicit);
            collect.query(tq, &mut Vec::new());
        }
        let sp = Spell {
            qs,
            ru,
            ordinary_query,
            qualify_only: false,
            r: &collect.out,
            columns: RefCell::new(None),
        };
        sp.select_query(q);
    }
}

fn table_name(qs: &QuerySchema, t: QTableId, ru: bool) -> String {
    let t = qs.table(t);
    if ru { &t.name_ru } else { &t.name_en }.clone()
}

pub(crate) fn spelled_alias(
    qs: &QuerySchema,
    ru: bool,
    src: &tir::Source,
    implicit: bool,
    ordinary: bool,
) -> String {
    let tir::From::Table { id, .. } = &src.from else {
        return src.alias.clone();
    };
    if implicit && let Some(a) = virtual_alias(qs, *id, ru) {
        return a;
    }
    let ru_name = table_name(qs, *id, true);
    let en_name = table_name(qs, *id, false);
    let a = fold_name(&src.alias);
    let configured = if ru { &ru_name } else { &en_name };
    if ordinary && !implicit && a == fold_name(last_segment(configured)) {
        return src.alias.clone();
    }
    if a == fold_name(last_segment(&ru_name)) || a == fold_name(last_segment(&en_name)) {
        last_segment(&table_name(qs, *id, ru)).to_string()
    } else {
        src.alias.clone()
    }
}

pub(crate) fn virtual_alias(qs: &QuerySchema, t: QTableId, ru: bool) -> Option<String> {
    if !matches!(qs.table(t).kind, TableKind::Virtual(_)) {
        return None;
    }
    let name = table_name(qs, t, ru);
    let mut parts = name.split('.');
    parts.next()?;
    Some(parts.collect())
}

fn last_segment(s: &str) -> &str {
    s.rsplit('.').next().unwrap_or(s)
}

struct Collect<'a> {
    qs: &'a QuerySchema,
    ru: bool,
    ordinary_query: bool,
    qualify_only: bool,
    out: Renames,
    implicit: HashSet<usize>,
}

fn flat<'a>(sources: &'a [tir::Source], out: &mut Vec<&'a tir::Source>) {
    for s in sources {
        out.push(s);
        for j in &s.joins {
            flat(std::slice::from_ref(&j.right), out);
        }
    }
}

type Stack<'a> = Vec<Vec<&'a tir::Source>>;

impl<'a> Collect<'a> {
    fn query(&mut self, q: &'a tir::Query, stack: &mut Stack<'a>) {
        for part in &q.parts {
            self.select(part, stack);
        }
        if let Some(first) = q.parts.first() {
            let mut sources = Vec::new();
            flat(&first.sources, &mut sources);
            stack.push(sources);
            for k in q.order.iter().filter(|k| k.col.is_none()) {
                self.expr(&k.expr, stack);
            }
            if let Some(t) = &q.totals {
                for (e, _) in &t.items {
                    self.expr(e, stack);
                }
                for p in &t.points {
                    if let tir::ControlPoint::Field(f) = p
                        && f.col.is_none()
                    {
                        self.expr(&f.expr, stack);
                    }
                }
            }
            stack.pop();
        }
    }

    fn select(&mut self, sel: &'a tir::Select, stack: &mut Stack<'a>) {
        let mut sources = Vec::new();
        flat(&sel.sources, &mut sources);
        for s in &sources {
            match &s.from {
                tir::From::Subquery(q) => self.query(q, &mut Vec::new()),
                tir::From::Table { args, .. } if self.ordinary_query => {
                    let outer = self.qualify_only;
                    self.qualify_only = true;
                    for arg in args.iter().flatten() {
                        self.expr(arg, &mut Vec::new());
                    }
                    self.qualify_only = outer;
                }
                _ => {}
            }
        }
        stack.push(sources.clone());
        for s in &sources {
            for j in &s.joins {
                self.expr(&j.on, stack);
            }
        }
        for it in &sel.items {
            self.expr(&it.expr, stack);
        }
        if let Some(w) = &sel.where_ {
            self.expr(w, stack);
        }
        for set in sel.group.iter().flatten() {
            for e in set {
                self.expr(e, stack);
            }
        }
        if let Some(h) = &sel.having {
            self.expr(h, stack);
        }
        stack.pop();
    }

    fn expr(&mut self, e: &'a tir::TExpr, stack: &mut Stack<'a>) {
        use tir::TKind::*;
        match &e.kind {
            Const(_) | Value(_) | Param(_) | Unit(_) => {}
            Field {
                level,
                source,
                col,
                derefs,
            } => self.field(e.span, *level, *source, *col, derefs, stack),
            Neg(a) | Not(a) => self.expr(a, stack),
            Arith { first, steps } => {
                self.expr(first, stack);
                for s in steps {
                    self.expr(&s.rhs, stack);
                }
            }
            And(v) | Or(v) => {
                for x in v {
                    self.expr(x, stack);
                }
            }
            Compare(_, a, b) => {
                self.expr(a, stack);
                self.expr(b, stack);
            }
            InList { l, rhs, .. } => {
                for x in l.iter().chain(rhs) {
                    self.expr(x, stack);
                }
            }
            InQuery { l, q, .. } => {
                for x in l {
                    self.expr(x, stack);
                }
                self.query(q, stack);
            }
            Between { l, lo, hi, .. } => {
                self.expr(l, stack);
                self.expr(lo, stack);
                self.expr(hi, stack);
            }
            IsNull { l, .. } | Refs { l, .. } => self.expr(l, stack),
            Like { l, pattern, .. } => {
                self.expr(l, stack);
                self.expr(pattern, stack);
            }
            Case {
                operand,
                whens,
                else_,
            } => {
                for x in operand.iter().chain(else_.iter()) {
                    self.expr(x, stack);
                }
                for (c, t) in whens {
                    self.expr(c, stack);
                    self.expr(t, stack);
                }
            }
            Cast {
                e: inner, derefs, ..
            } => {
                if !derefs.is_empty() {
                    let names = derefs.iter().map(|d| self.deref_name(d)).collect();
                    self.out.casts.insert(key(e.span), names);
                }
                self.expr(inner, stack);
            }
            Func { args, .. } => {
                for x in args {
                    self.expr(x, stack);
                }
            }
            Agg { arg, .. } => {
                if let Some(x) = arg {
                    self.expr(x, stack);
                }
            }
        }
    }

    fn deref_name(&self, d: &tir::Deref) -> Option<String> {
        let (t, f) = d.targets.first()?;
        let f = self.qs.field(*t, *f)?;
        Some(if self.ru { &f.name_ru } else { &f.name_en }.clone())
    }

    fn field(
        &mut self,
        span: query_syntax::Span,
        level: u16,
        source: u16,
        col: Col,
        derefs: &[tir::Deref],
        stack: &Stack<'a>,
    ) {
        let src = stack
            .len()
            .checked_sub(1 + usize::from(level))
            .and_then(|i| stack.get(i))
            .and_then(|s| s.get(usize::from(source)));
        let Some(src) = src else { return };
        let table = match &src.from {
            tir::From::Table { id, .. } => Some(*id),
            _ => None,
        };
        let own = match (col, table) {
            (Col::Field(f), Some(t)) => self
                .qs
                .field(t, f)
                .map(|f| if self.ru { &f.name_ru } else { &f.name_en }.clone()),
            (Col::Column(i), _) if self.ordinary_query => match &src.from {
                tir::From::Temp(t) => t.columns.get(usize::from(i)).map(|c| c.name.clone()),
                tir::From::Subquery(q) => q
                    .parts
                    .first()
                    .and_then(|p| p.items.get(usize::from(i)))
                    .map(|c| c.name.clone()),
                _ => None,
            },
            _ => None,
        };
        let column_case_only = matches!(col, Col::Column(_)) && own.is_some();
        let mut tail = vec![own];
        tail.extend(derefs.iter().map(|d| self.deref_name(d)));
        let implicit = self
            .implicit
            .contains(&(*src as *const tir::Source as usize));
        let alias = if self.qualify_only {
            src.alias.clone()
        } else {
            spelled_alias(self.qs, self.ru, src, implicit, self.ordinary_query)
        };
        self.out.fields.insert(
            key(span),
            FieldName {
                alias: (!alias.is_empty()).then_some(alias),
                tail,
                column_case_only,
            },
        );
    }
}

struct Spell<'a> {
    qs: &'a QuerySchema,
    ru: bool,
    ordinary_query: bool,
    qualify_only: bool,
    r: &'a Renames,
    columns: RefCell<Option<HashSet<String>>>,
}

impl Spell<'_> {
    fn select_query(&self, q: &mut SelectQuery) {
        for op in &mut q.operators {
            for it in &mut op.fields {
                self.item(it);
            }
            for s in &mut op.sources {
                self.source(s);
            }
            for e in op.filter.iter_mut().chain(op.having.iter_mut()) {
                self.expr(e);
            }
            match &mut op.group {
                Some(ast::GroupKind::Exprs(v)) => v.iter_mut().for_each(|e| self.expr(e)),
                Some(ast::GroupKind::Sets(sets)) => {
                    sets.iter_mut().flatten().for_each(|e| self.expr(e))
                }
                None => {}
            }
        }
        let names = q
            .operators
            .first()
            .into_iter()
            .flat_map(|op| &op.fields)
            .filter_map(|it| it.alias.as_ref())
            .map(|a| fold_name(&a.name.text))
            .collect();
        let outer = self.columns.replace(Some(names));
        for o in &mut q.order {
            self.expr(&mut o.expr);
        }
        if let Some(t) = &mut q.totals {
            self.totals(&mut t.items, &mut t.points);
        }
        self.columns.replace(outer);
        match &mut q.index {
            Some(ast::IndexKind::Fields { fields, .. }) => {
                fields.iter_mut().for_each(|e| self.expr(e))
            }
            Some(ast::IndexKind::Sets(sets)) => sets
                .iter_mut()
                .flat_map(|s| s.fields.iter_mut())
                .for_each(|e| self.expr(e)),
            None => {}
        }
    }

    fn totals(&self, items: &mut [ast::TotalsItem], points: &mut [ast::ControlPoint]) {
        for i in items {
            self.expr(&mut i.expr);
        }
        for p in points {
            if let ast::ControlPoint::Field { expr, .. } = p {
                self.expr(expr);
            }
        }
    }

    fn item(&self, it: &mut ast::SelectItem) {
        match &mut it.kind {
            ItemKind::Expr(e) => self.expr(e),
            ItemKind::Nested { table, fields } => {
                self.expr(table);
                fields.iter_mut().for_each(|f| self.item(f));
            }
            ItemKind::Star | ItemKind::PathStar(_) | ItemKind::EmptyTable { .. } => {}
        }
    }

    fn source(&self, s: &mut Source) {
        match &mut s.table {
            Table::Named { path, args } => {
                self.table_path(path);
                if self.ordinary_query
                    && let Some(args) = args
                {
                    self.arguments(args.iter_mut().flatten());
                }
            }
            Table::Subquery(q) => self.select_query(q),
            Table::Param(_) => {}
        }
        for j in &mut s.joins {
            self.source(&mut j.right);
            for c in &mut j.on {
                self.expr(&mut c.expr);
            }
        }
    }

    fn ast_query(&self, q: &mut ast::Query) {
        for p in &mut q.parts {
            p.items.iter_mut().for_each(|i| self.item(i));
            if let Some(f) = &mut p.from {
                f.sources.iter_mut().for_each(|s| self.ast_source(s));
            }
            for c in p.where_.iter_mut().chain(p.having.iter_mut()) {
                self.expr(&mut c.expr);
            }
            match p.group_by.as_mut().map(|g| &mut g.kind) {
                Some(ast::GroupKind::Exprs(v)) => v.iter_mut().for_each(|e| self.expr(e)),
                Some(ast::GroupKind::Sets(sets)) => {
                    sets.iter_mut().flatten().for_each(|e| self.expr(e))
                }
                None => {}
            }
        }
        if let Some(o) = &mut q.order_by {
            o.items.iter_mut().for_each(|i| self.expr(&mut i.expr));
        }
        if let Some(t) = &mut q.totals {
            self.totals(&mut t.items, &mut t.points);
        }
    }

    fn ast_source(&self, s: &mut ast::Source) {
        match &mut s.table {
            ast::TableRef::Named { path, args } => {
                self.table_path(path);
                if self.ordinary_query
                    && let Some(args) = args
                {
                    self.arguments(args.args.iter_mut().flatten());
                }
            }
            ast::TableRef::Subquery { query, .. } => self.ast_query(query),
            ast::TableRef::Param(_) => {}
        }
        for j in &mut s.joins {
            self.ast_source(&mut j.right);
            self.expr(&mut j.on);
        }
    }

    fn arguments<'a>(&self, args: impl Iterator<Item = &'a mut ast::VtArg>) {
        let spelling = Self {
            qs: self.qs,
            ru: self.ru,
            ordinary_query: self.ordinary_query,
            qualify_only: true,
            r: self.r,
            columns: RefCell::new(None),
        };
        for arg in args {
            spelling.expr(&mut arg.expr);
        }
    }

    fn expr(&self, e: &mut Expr) {
        let span = e.span;
        match &mut e.kind {
            ExprKind::Lit(_) | ExprKind::Param(_) | ExprKind::Dcs(_) => {}
            ExprKind::Path(p) => self.field_path(p),
            ExprKind::Chain { first, rest } => {
                self.expr(first);
                rest.iter_mut().for_each(|c| self.expr(&mut c.expr));
            }
            ExprKind::Unary { expr, .. } | ExprKind::Paren(expr) => self.expr(expr),
            ExprKind::Compare { l, r, .. } => {
                self.expr(l);
                self.expr(r);
            }
            ExprKind::In { l, rhs, .. } => {
                self.expr(l);
                match rhs {
                    InRhs::List(v) => v.iter_mut().for_each(|x| self.expr(x)),
                    InRhs::Query(q) => self.ast_query(q),
                }
            }
            ExprKind::Between { l, lo, hi, .. } => {
                self.expr(l);
                self.expr(lo);
                self.expr(hi);
            }
            ExprKind::IsNull { l, .. } => self.expr(l),
            ExprKind::Refs { l, table, .. } => {
                self.expr(l);
                self.table_path(table);
            }
            ExprKind::Like { l, pattern, .. } => {
                self.expr(l);
                self.expr(pattern);
            }
            ExprKind::Tuple(v) | ExprKind::DateTime { args: v, .. } => {
                v.iter_mut().for_each(|x| self.expr(x))
            }
            ExprKind::Case {
                operand,
                whens,
                else_,
                ..
            } => {
                if let Some(o) = operand {
                    self.expr(o);
                }
                for w in whens {
                    self.expr(&mut w.cond);
                    self.expr(&mut w.then);
                }
                if let Some(x) = else_ {
                    self.expr(x);
                }
            }
            ExprKind::Deref { expr, deref } => {
                self.expr(expr);
                if !self.qualify_only
                    && let Some(names) = self.r.casts.get(&key(span))
                {
                    for (id, n) in deref.iter_mut().zip(names) {
                        set_text(id, n.as_deref());
                    }
                }
            }
            ExprKind::Cast {
                expr, ty, deref, ..
            } => {
                self.expr(expr);
                if let CastType::Table(p) = ty {
                    self.table_path(p);
                }
                if !self.qualify_only
                    && let Some(names) = self.r.casts.get(&key(span))
                {
                    for (id, n) in deref.iter_mut().zip(names) {
                        set_text(id, n.as_deref());
                    }
                }
            }
            ExprKind::Func { args, .. } => {
                if let FuncArgs::List(v) = args {
                    v.iter_mut().for_each(|x| self.expr(x));
                }
            }
            ExprKind::TypeOf { name, .. } => {
                let written = name.clone();
                self.table_path(name);
                for (id, w) in name.parts.iter_mut().zip(&written.parts).skip(1) {
                    id.text = w.text.clone();
                }
            }
            ExprKind::Value { path, .. } => {
                if !self.ordinary_query {
                    self.value_path(path);
                }
            }
        }
    }

    fn field_path(&self, p: &mut ast::Path) {
        let Some(f) = self.r.fields.get(&key(p.span())) else {
            return;
        };
        if let [one] = p.parts.as_slice()
            && self
                .columns
                .borrow()
                .as_ref()
                .is_some_and(|c| c.contains(&fold_name(&one.text)))
        {
            return;
        }
        let Some(prefix) = p.parts.len().checked_sub(f.tail.len()) else {
            return;
        };
        if self.qualify_only && prefix != 0 {
            return;
        }
        if !self.qualify_only {
            for (i, n) in f.tail.iter().enumerate() {
                if let Some(id) = p.parts.get_mut(prefix + i) {
                    if i == 0
                        && f.column_case_only
                        && n.as_ref()
                            .is_some_and(|name| fold_name(name) != fold_name(&id.text))
                    {
                        continue;
                    }
                    set_text(id, n.as_deref());
                }
            }
        }
        match (&f.alias, prefix) {
            (Some(a), 0) => {
                if let Some(first) = p.parts.first() {
                    let id = Ident {
                        text: a.as_str().into(),
                        span: first.span,
                    };
                    p.parts.insert(0, id);
                }
            }
            (Some(a), 1) => {
                if let Some(first) = p.parts.first_mut() {
                    first.text = a.as_str().into();
                }
            }
            _ => {}
        }
    }

    fn table_path(&self, p: &mut ast::Path) {
        if self.qualify_only || p.parts.len() < 2 {
            return;
        }
        let parts: Vec<&str> = p.parts.iter().map(|x| &*x.text).collect();
        let Some(t) = self.qs.find_table(&parts) else {
            return;
        };
        let name = table_name(self.qs, t, self.ru);
        let new: Vec<&str> = name.split('.').collect();
        if new.len() != p.parts.len() {
            return;
        }
        for (id, n) in p.parts.iter_mut().zip(new) {
            set_text(id, Some(n));
        }
    }

    fn value_path(&self, p: &mut ast::Path) {
        let parts: Vec<&str> = p.parts.iter().map(|x| &*x.text).collect();
        if let query_ir::ValuePath::Predefined { name, .. } = self.qs.value_path(&parts)
            && let Some(id) = p.parts.get_mut(2)
        {
            set_text(id, Some(&name));
        }
        match p.parts.len() {
            2 => self.table_path(p),
            n if n >= 3 => {
                let mut head = ast::Path {
                    parts: p.parts.iter().take(2).cloned().collect(),
                };
                self.table_path(&mut head);
                for (id, h) in p.parts.iter_mut().zip(head.parts) {
                    id.text = h.text;
                }
            }
            _ => {}
        }
    }
}

fn set_text(id: &mut Ident, text: Option<&str>) {
    if let Some(t) = text {
        id.text = t.into();
    }
}
