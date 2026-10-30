mod expr;
pub(crate) mod piece;
pub(crate) mod prec;

use query_syntax::ast::*;
use query_syntax::kw::{Kw, lookup};
use query_syntax::{ScriptVariant, with_deep_stack};
use std::collections::HashSet;

const RULE: &str =
    "////////////////////////////////////////////////////////////////////////////////";

pub fn pretty(b: &Batch, script: ScriptVariant) -> String {
    pretty_with_blocks(b, script, HashSet::new())
}

pub(crate) fn pretty_with_blocks(
    b: &Batch,
    script: ScriptVariant,
    same_block: HashSet<usize>,
) -> String {
    with_deep_stack(b.max_depth, || {
        let mut l = L {
            script,
            same_block,
            ordinary_query: b.mode == query_syntax::ParseMode::Query,
            ..L::default()
        };
        for s in &b.stmts {
            if !l.out.is_empty() {
                l.raw("\n;\n\n");
                l.raw(RULE);
                l.nl(0);
            }
            match s {
                Stmt::Query(q) => l.query(&q.query, 0, 0),
                Stmt::Drop(d) => {
                    l.kw(Kw::Drop);
                    l.tok(&d.name.text);
                }
            }
        }
        l.flush();
        l.out
    })
}

#[derive(Default)]
pub(crate) struct L {
    pub(crate) out: String,
    pub(crate) script: ScriptVariant,
    pub(crate) ei: usize,
    pub(crate) expression_base: Option<usize>,
    pub(crate) glue: bool,
    pub(crate) pending: bool,
    pub(crate) tight: bool,
    pub(crate) embedded: usize,
    pub(crate) ordinary_query: bool,
    pub(crate) same_block: HashSet<usize>,
}

impl L {
    pub(crate) fn flush(&mut self) {
        if self.pending {
            self.out.push(' ');
            self.pending = false;
        }
    }
    pub(crate) fn tok(&mut self, s: &str) {
        let space = !self.glue
            && !self.out.is_empty()
            && !self.out.ends_with(['(', '{', '.', ' ', '\n', '\t']);
        self.flush();
        if space {
            self.out.push(' ');
        }
        self.glue = false;
        self.out.push_str(s);
    }
    pub(crate) fn raw(&mut self, s: &str) {
        self.flush();
        self.glue = false;
        self.out.push_str(s);
    }
    pub(crate) fn kw(&mut self, k: Kw) {
        let w = k.name(self.script);
        self.tok(w);
    }
    pub(crate) fn kws(&mut self, ks: &[Kw]) {
        for k in ks {
            self.kw(*k);
        }
    }
    pub(crate) fn nl(&mut self, indent: usize) {
        self.brk(indent);
        self.ei = indent;
    }
    pub(crate) fn sep(&self) -> usize {
        self.ei + 1
    }
    pub(crate) fn brk(&mut self, indent: usize) {
        self.flush();
        self.out.push('\n');
        self.out.extend(std::iter::repeat_n('\t', indent));
        self.glue = false;
    }
    fn blank(&mut self, b: usize) {
        self.flush();
        if !self.out.ends_with('\n') {
            self.out.push('\n');
        }
        self.out.extend(std::iter::repeat_n('\t', b));
    }
    pub(crate) fn lines<I>(
        &mut self,
        items: &[I],
        indent: usize,
        mut f: impl FnMut(&mut Self, &I),
    ) {
        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                self.raw(",");
            }
            self.nl(indent);
            f(self, it);
        }
    }

    pub(crate) fn path(&mut self, p: &Path) {
        for (i, part) in p.parts.iter().enumerate() {
            if i == 0 {
                self.tok(&part.text);
            } else {
                self.raw(".");
                self.raw(&part.text);
            }
        }
    }
    pub(crate) fn param(&mut self, p: &Param) {
        self.tok("&");
        self.raw(&p.name.text);
    }
    pub(crate) fn alias(&mut self, a: Option<&Alias>) {
        if let Some(a) = a {
            self.kw(Kw::As);
            self.tok(&a.name.text);
        }
    }
    pub(crate) fn dir(&mut self, d: OrderDir, asc: bool) {
        match d {
            OrderDir::Asc if asc => self.kw(Kw::Asc),
            OrderDir::Asc => {}
            OrderDir::Desc => self.kw(Kw::Desc),
            OrderDir::Hierarchy => self.kw(Kw::Hierarchy),
            OrderDir::HierarchyDesc => self.kws(&[Kw::Hierarchy, Kw::Desc]),
        }
    }

    pub(crate) fn query(&mut self, q: &Query, r: usize, b: usize) {
        if q.parts.first().is_none_or(|p| p.items.is_empty()) {
            return;
        }
        let expression_base = self.expression_base.take();
        let union_blank = if self.ordinary_query && r > b {
            expression_base.unwrap_or(b)
        } else {
            b
        };
        let last = q.parts.len().saturating_sub(1);
        let placement_index = q.parts.first().is_some_and(|p| p.into.is_some())
            && q.parts
                .last()
                .and_then(|p| p.index_by.as_ref())
                .is_some_and(|ix| match &ix.kind {
                    IndexKind::Fields { fields, .. } => !fields.is_empty(),
                    IndexKind::Sets(sets) => {
                        !sets.is_empty() && sets.iter().all(|set| !set.fields.is_empty())
                    }
                });
        let index_late = placement_index
            || (q.order_by.is_some() && q.autoorder.is_none() && q.totals.is_none());
        for (i, part) in q.parts.iter().enumerate() {
            if let Some(j) = i.checked_sub(1) {
                self.blank(union_blank);
                self.nl(r);
                self.kw(Kw::Union);
                if q.unions.get(j).is_some_and(|u| u.all) {
                    self.kw(Kw::All);
                }
                self.blank(union_blank);
                self.nl(r);
            }
            self.select(part, r, b, i == last && index_late);
        }
        if let Some(o) = &q.order_by {
            if !self.ordinary_query || r == b {
                self.blank(b);
            }
            self.nl(r);
            self.kws(&[Kw::Order, Kw::By]);
            self.lines(&o.items, r + 1, |t, it| {
                t.expr(&it.expr);
                t.dir(it.dir, false);
            });
        }
        if let Some(tot) = &q.totals {
            self.totals(tot, r);
        }
        if q.autoorder.is_some() {
            self.nl(r);
            self.kw(Kw::AutoOrder);
        }
        if let Some(ix) = q
            .parts
            .last()
            .and_then(|p| p.index_by.as_ref())
            .filter(|_| index_late)
        {
            self.index_by(ix, r, b);
        }
        for blk in &q.dcs {
            self.nl(r);
            self.dcs_block(blk, r);
        }
        self.expression_base = expression_base;
    }

    fn select(&mut self, s: &Select, r: usize, b: usize, index_late: bool) {
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
        self.lines(&s.items, r + 1, |t, it| t.item(it, r + 1));
        let blocks = DcsPlan::of(&s.dcs);
        if let Some(i) = &s.into {
            self.nl(r);
            self.kw(match i.kind {
                IntoKind::Put => Kw::Into,
                IntoKind::Add => Kw::Add,
            });
            self.tok(&i.name.text);
        }
        blocks.print(self, r, DcsSlot::Select);
        if let Some(f) = &s.from {
            self.nl(r);
            self.kw(Kw::From);
            for (i, src) in f.sources.iter().enumerate() {
                if i > 0 {
                    self.raw(",");
                }
                self.nl(r + 1);
                self.source(src, r + 1);
            }
        }
        if let Some(w) = &s.where_ {
            self.nl(r);
            self.kw(Kw::Where);
            self.elements(&w.expr, r + 1, false);
        }
        blocks.print(self, r, DcsSlot::Where);
        if let Some(g) = &s.group_by {
            if !self.ordinary_query || r == b {
                self.blank(b);
            }
            self.nl(r);
            self.kws(&[Kw::Group, Kw::By]);
            match &g.kind {
                GroupKind::Exprs(es) => self.lines(es, r + 1, |t, e| t.expr(e)),
                GroupKind::Sets(sets) => self.grouping_sets(sets, r),
            }
        }
        if let Some(h) = &s.having {
            if !self.ordinary_query || r == b {
                self.blank(b);
            }
            self.nl(r);
            self.kw(Kw::Having);
            self.elements(&h.expr, r + 1, true);
        }
        blocks.print(self, r, DcsSlot::Rest);
        if let Some(f) = &s.for_update {
            self.for_update(f, r, b);
        }
        if let Some(ix) = s.index_by.as_ref().filter(|_| !index_late) {
            self.index_by(ix, r, b);
        }
    }

    fn for_update(&mut self, f: &ForUpdate, r: usize, b: usize) {
        self.blank(b);
        self.nl(r);
        self.kws(&[Kw::For, Kw::Update]);
        let first = f.tables.first().and_then(|p| p.parts.first());
        if first.is_some_and(|w| lookup(&w.text).0.contains(Kw::Of)) {
            self.kw(Kw::Of);
        }
        self.lines(&f.tables, r + 1, |t, p| t.path(p));
    }

    fn index_by(&mut self, ix: &IndexBy, r: usize, b: usize) {
        self.blank(b);
        self.nl(r);
        self.kws(&[Kw::Index, Kw::By]);
        match &ix.kind {
            IndexKind::Fields { fields, unique } => {
                self.lines(fields, r + 1, |t, e| t.expr(e));
                if unique.is_some() {
                    self.kw(Kw::Unique);
                }
            }
            IndexKind::Sets(sets) => {
                self.kw(Kw::Sets);
                self.raw(" (");
                self.lines(sets, r + 1, |t, set| {
                    t.raw("(");
                    for (i, e) in set.fields.iter().enumerate() {
                        if i > 0 {
                            t.raw(",");
                        }
                        t.expr(e);
                    }
                    t.raw(")");
                    if set.unique.is_some() {
                        t.kw(Kw::Unique);
                    }
                });
                self.raw(")");
            }
        }
    }

    fn grouping_sets(&mut self, sets: &[Vec<Expr>], r: usize) {
        self.kws(&[Kw::Grouping, Kw::Sets]);
        self.nl(r);
        self.raw("(\t");
        for (i, set) in sets.iter().enumerate() {
            if i > 0 {
                if set.is_empty() {
                    self.nl(r);
                    self.raw(",");
                } else {
                    self.raw(",");
                }
            }
            self.nl(r + 1);
            self.raw("(");
            for (k, e) in set.iter().enumerate() {
                if k > 0 {
                    self.raw(",");
                }
                self.expr(e);
            }
            self.raw(")");
        }
        self.nl(r);
        self.raw(")\n");
    }

    fn item(&mut self, it: &SelectItem, line: usize) {
        match &it.kind {
            ItemKind::Star => self.tok("*"),
            ItemKind::PathStar(p) => {
                self.path(p);
                self.raw(".*");
            }
            ItemKind::Expr(e) => self.expr(e),
            ItemKind::Nested { table, fields } => {
                self.expr(table);
                self.raw(".(");
                self.lines(fields, line + 1, |t, f| t.item(f, line + 1));
                self.nl(line);
                self.raw(")");
            }
            ItemKind::EmptyTable { names, .. } => {
                self.kw(Kw::EmptyTable);
                self.raw(".(");
                for (i, n) in names.iter().enumerate() {
                    if i > 0 {
                        self.raw(", ");
                    }
                    self.raw(" ");
                    self.raw(Kw::As.name(self.script));
                    self.raw(" ");
                    if let Some(n) = n {
                        self.raw(&n.text);
                    }
                }
                self.raw(")");
            }
        }
        self.alias(it.alias.as_ref());
    }

    fn source(&mut self, s: &Source, line: usize) {
        match &s.table {
            TableRef::Named { path, args } => {
                self.path(path);
                if let Some(a) = args {
                    self.vt_args(a, line);
                }
            }
            TableRef::Subquery { query, .. } => {
                self.tok("(");
                self.query(query, line, line);
                self.raw(")");
            }
            TableRef::Param(p) => self.param(p),
        }
        self.alias(s.alias.as_ref());
        for (k, j) in s.joins.iter().enumerate() {
            self.nl(line + 1);
            let continues =
                |x: &Join| x.optional && self.same_block.contains(&(x as *const Join as usize));
            let open = j.optional && !(k > 0 && continues(j));
            let close = j.optional && !s.joins.get(k + 1).is_some_and(continues);
            if open {
                self.tok("{");
            }
            self.kws(match j.kind {
                JoinKind::Inner => &[Kw::Inner, Kw::Join],
                JoinKind::Left => &[Kw::Left, Kw::Join],
                JoinKind::Right => &[Kw::Right, Kw::Join],
                JoinKind::Full => &[Kw::Full, Kw::Join],
            });
            self.source(&j.right, line + 1);
            self.nl(line + 1);
            self.kw(Kw::On);
            if self.embedded > 0 {
                self.nl(line + 2);
                self.expr(&j.on);
            } else {
                self.on_conditions(&j.on, line + 1);
            }
            if close {
                self.raw("}");
            }
        }
    }

    fn on_conditions(&mut self, e: &Expr, line: usize) {
        for (i, el) in conjuncts(e).into_iter().enumerate() {
            if i > 0 {
                self.nl(line + 1);
                self.kw(Kw::And);
            }
            self.ei = line + 1;
            self.expr(el);
        }
    }

    fn elements(&mut self, e: &Expr, line: usize, having: bool) {
        for (i, el) in conjuncts(e).into_iter().enumerate() {
            if i > 0 && having {
                self.kw(Kw::And);
            }
            self.nl(line);
            if i > 0 && !having {
                self.kw(Kw::And);
            }
            self.ei = line + usize::from(!having && self.embedded == 0);
            self.tight = true;
            self.expr(el);
        }
    }

    fn totals(&mut self, tot: &Totals, r: usize) {
        self.nl(r);
        self.kw(Kw::Totals);
        if !tot.items.is_empty() {
            self.lines(&tot.items, r + 1, |t, it| {
                t.expr(&it.expr);
                t.alias(it.alias.as_ref());
            });
            self.nl(r);
        }
        self.kw(Kw::By);
        self.lines(&tot.points, r + 1, |t, cp| match cp {
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
                t.alias(alias.as_ref());
            }
        });
    }
}

fn conjuncts(e: &Expr) -> Vec<&Expr> {
    match &e.kind {
        ExprKind::Chain { first, rest } if rest.is_empty() => conjuncts(first),
        ExprKind::Chain { rest, .. }
            if rest
                .first()
                .is_some_and(|o| o.op_span == crate::to_ast::UNSPLIT) =>
        {
            vec![e]
        }
        ExprKind::Chain { first, rest } if rest.first().is_some_and(|o| o.op == BinOp::And) => {
            let mut out = Vec::with_capacity(rest.len() + 1);
            out.push(&**first);
            out.extend(rest.iter().map(|o| &o.expr));
            out
        }
        _ => vec![e],
    }
}

#[derive(Clone, Copy, PartialEq)]
enum DcsSlot {
    Select,
    Where,
    Rest,
}

struct DcsPlan<'a> {
    blocks: &'a [DcsBlock],
    sectioned: bool,
}

impl<'a> DcsPlan<'a> {
    fn of(blocks: &'a [DcsBlock]) -> Self {
        let rank = |b: &DcsBlock| match b.kind {
            DcsKind::Select(_) => 0,
            DcsKind::Where(_) => 1,
            _ => 2,
        };
        let sectioned = blocks.windows(2).all(|w| match w {
            [a, b] => rank(a) <= rank(b),
            _ => true,
        });
        DcsPlan { blocks, sectioned }
    }
    fn print(&self, l: &mut L, r: usize, slot: DcsSlot) {
        for b in self.blocks {
            let own = match b.kind {
                DcsKind::Select(_) => DcsSlot::Select,
                DcsKind::Where(_) => DcsSlot::Where,
                _ => DcsSlot::Rest,
            };
            let here = if self.sectioned {
                own == slot
            } else {
                slot == DcsSlot::Select
            };
            if here {
                l.nl(r);
                l.dcs_block(b, r);
            }
        }
    }
}
