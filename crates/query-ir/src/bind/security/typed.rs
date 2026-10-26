mod order;
mod qr8;

use super::{Collector, Occurrence, Path, Root};
use crate::bind::BindCtx;
use crate::exec_error::ExecError;
use crate::ir::{ColSel, SrcId};
use crate::schema::{QTableId, StdRole, TableKind};
use crate::tir::{self, Col, From, TExpr, TKind};

#[derive(Clone, Copy)]
struct Binding {
    src: SrcId,
    table: Option<QTableId>,
    account: Option<StdRole>,
}

type Levels = Vec<Vec<Option<Binding>>>;

struct Typed<'a> {
    collector: Collector<'a>,
    storage: &'a crate::StorageBinding,
    next: u32,
    static_analysis: bool,
    section: Option<(SrcId, crate::QFieldId, Binding)>,
}

impl Typed<'_> {
    fn binding(&mut self, table: Option<QTableId>, all: bool) -> Result<Binding, ExecError> {
        let src = SrcId(self.next);
        self.next += 1;
        if let Some(table) = table {
            self.collector
                .register(Occurrence::Direct(src), table, all)?;
        }
        Ok(Binding {
            src,
            table,
            account: None,
        })
    }

    fn lookup(levels: &Levels, level: u16, source: u16) -> Option<Binding> {
        levels
            .get(usize::from(level))?
            .get(usize::from(source))
            .copied()
            .flatten()
    }

    fn path(&mut self, e: &TExpr, levels: &Levels) -> Path {
        let root = match &e.kind {
            TKind::Field {
                level,
                source,
                col,
                derefs,
            } => {
                let Some(b) = Self::lookup(levels, *level, *source) else {
                    return self.collector.opaque();
                };
                let root = match (col, b.account) {
                    (Col::Field(f), _) => Root::Column(b.src, ColSel::Field(*f)),
                    (Col::Column(_), Some(role)) => Root::Standard(b.src, role),
                    (Col::Column(i), None) => Root::Column(b.src, ColSel::Column(*i)),
                };
                return Path {
                    root,
                    steps: derefs.iter().map(|d| d.targets.clone()).collect(),
                };
            }
            TKind::Const(crate::QValue::Ref { table, id }) => Root::Reference(*table, *id),
            TKind::Cast { e, derefs, .. } => {
                let mut path = self.path(e, levels);
                path.steps.extend(derefs.iter().map(|d| d.targets.clone()));
                return path;
            }
            _ => return self.collector.opaque(),
        };
        Path {
            root,
            steps: Vec::new(),
        }
    }

    fn source(
        &mut self,
        s: &tir::Source,
        level: &mut Vec<Option<Binding>>,
    ) -> Result<(), ExecError> {
        let table = match &s.from {
            From::Table { id, .. } => Some(*id),
            _ => None,
        };
        let binding = self.binding(table, s.all_fields)?;
        if let Some(table) = table {
            self.qr8_dependencies(binding, table)?;
        }
        level.push(Some(binding));
        for j in &s.joins {
            self.source(&j.right, level)?;
        }
        Ok(())
    }

    fn source_exprs(
        &mut self,
        s: &tir::Source,
        levels: &Levels,
        position: &mut usize,
    ) -> Result<(), ExecError> {
        let binding = levels
            .first()
            .and_then(|level| level.get(*position))
            .copied()
            .ok_or_else(|| ExecError::Fatal("security source position is missing".into()))?;
        *position += 1;
        match &s.from {
            From::Table { id, args, .. } => {
                for (i, arg) in args.iter().enumerate() {
                    if let Some(e) = arg {
                        let mut b = binding;
                        if let Some(b) = &mut b {
                            b.account = crate::sema::vt::account_condition(
                                self.collector
                                    .schema
                                    .table(*id)
                                    .params
                                    .get(i)
                                    .map(|p| p.name_en.as_str()),
                            )
                            .map(|x| x.0);
                        }
                        self.expr(e, &vec![vec![b]])?;
                    }
                }
            }
            From::Subquery(q) => {
                let outer = levels
                    .get(1..)
                    .ok_or_else(|| ExecError::Fatal("security source scope is missing".into()))?
                    .to_vec();
                self.query(q, &outer)?;
            }
            _ => {}
        }
        for j in &s.joins {
            self.source_exprs(&j.right, levels, position)?;
            self.expr(&j.on, levels)?;
        }
        Ok(())
    }

    fn items(&mut self, items: &[tir::Item], levels: &Levels) -> Result<(), ExecError> {
        for item in items {
            self.expr(&item.expr, levels)?;
            if let Some(items) = &item.nested {
                if matches!(item.expr.kind, TKind::Const(_)) {
                    continue;
                }
                let target = match &item.expr.kind {
                    TKind::Field {
                        level,
                        source,
                        col,
                        derefs,
                    } => {
                        if let Some(d) = derefs.last() {
                            d.targets.first().copied()
                        } else if let Col::Field(f) = col {
                            Self::lookup(levels, *level, *source)
                                .and_then(|b| b.table.map(|t| (t, *f)))
                        } else {
                            None
                        }
                    }
                    TKind::Cast { derefs, .. } => {
                        derefs.last().and_then(|d| d.targets.first()).copied()
                    }
                    _ => None,
                };
                let table = target
                    .and_then(|(t, f)| self.collector.schema.field(t, f))
                    .and_then(|f| f.nested);
                if let Some(table) = table {
                    if let TKind::Field {
                        level,
                        source,
                        derefs,
                        ..
                    } = &item.expr.kind
                        && derefs.is_empty()
                        && let Some(owner) = Self::lookup(levels, *level, *source)
                        && let Some(owner_table) = owner.table
                    {
                        self.collector.role(
                            Occurrence::Direct(owner.src),
                            owner_table,
                            StdRole::Ref,
                        )?;
                    }
                    let b = self.binding(Some(table), items.iter().any(|i| i.star))?;
                    for role in [StdRole::Ref, StdRole::LineNumber] {
                        self.collector
                            .role(Occurrence::Direct(b.src), table, role)?;
                    }
                    let mut nested = vec![vec![Some(b)]];
                    nested.extend(levels.iter().cloned());
                    self.items(items, &nested)?;
                } else {
                    return Err(ExecError::Fatal(
                        "security nested source has no identity".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn select(&mut self, select: &tir::Select, outer: &Levels) -> Result<Levels, ExecError> {
        let mut level = Vec::new();
        for s in &select.sources {
            self.source(s, &mut level)?;
        }
        let mut levels = vec![level];
        levels.extend(outer.iter().cloned());
        let mut position = 0;
        for s in &select.sources {
            self.source_exprs(s, &levels, &mut position)?;
        }
        self.items(&select.items, &levels)?;
        for e in select.where_.iter().chain(&select.having) {
            self.expr(e, &levels)?;
        }
        for e in select.group.iter().flatten().flatten() {
            self.expr(e, &levels)?;
        }
        Ok(levels)
    }

    fn query(&mut self, query: &tir::Query, outer: &Levels) -> Result<(), ExecError> {
        let mut first = None;
        for select in &query.parts {
            let levels = self.select(select, outer)?;
            if first.is_none() {
                first = Some(levels);
            }
        }
        let levels = first.unwrap_or_default();
        for key in &query.order {
            self.expr(&key.expr, &levels)?;
        }
        self.order(query, &levels)?;
        if let Some(totals) = &query.totals {
            for (e, _) in &totals.items {
                self.expr(e, &levels)?;
            }
            for point in &totals.points {
                if let tir::ControlPoint::Field(p) = point {
                    self.expr(&p.expr, &levels)?;
                    if p.hierarchy != tir::TotalsHierarchy::No {
                        let path = self.path(&p.expr, &levels);
                        self.collector.hierarchy(path, &p.expr.ty, true)?;
                    }
                    if let Some(periods) = &p.periods {
                        for e in periods.begin.iter().chain(&periods.end) {
                            self.expr(e, &levels)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn field_expr(
        &mut self,
        level: u16,
        source: u16,
        col: Col,
        derefs: &[tir::Deref],
        levels: &Levels,
    ) -> Result<(), ExecError> {
        let b = Self::lookup(levels, level, source);
        if let Some((owner, section_field, section)) = self.section
            && b.is_some_and(|b| b.src == owner)
            && col == Col::Field(section_field)
            && let Some((first, rest)) = derefs.split_first()
        {
            for &(_, field) in &first.targets {
                self.field_expr(0, 0, Col::Field(field), rest, &vec![vec![Some(section)]])?;
            }
            return Ok(());
        }
        let mut path = match b {
            Some(b) => Path {
                root: match (col, b.account) {
                    (Col::Field(f), _) => Root::Column(b.src, ColSel::Field(f)),
                    (Col::Column(_), Some(role)) => Root::Standard(b.src, role),
                    (Col::Column(i), None) => Root::Column(b.src, ColSel::Column(i)),
                },
                steps: Vec::new(),
            },
            None => self.collector.opaque(),
        };
        if let Some(b) = b
            && let Some(table) = b.table
        {
            let key = Occurrence::Direct(b.src);
            if let Col::Field(f) = col {
                if derefs.is_empty()
                    && self.collector.schema.is_presentation_field(table, f)
                    && let Some((i, f)) = self
                        .collector
                        .schema
                        .table(table)
                        .fields
                        .iter()
                        .enumerate()
                        .find(|(_, f)| f.role == crate::schema::FieldRole::Std(StdRole::Ref))
                {
                    let field = crate::QFieldId(
                        u16::try_from(i)
                            .map_err(|_| ExecError::Fatal("security field index".into()))?,
                    );
                    let ty = f.ty.clone();
                    self.collector.field(key, table, field)?;
                    path.root = Root::Column(b.src, ColSel::Field(field));
                    return self.collector.present(path, &ty, false, self.storage);
                }
                self.collector.field(key, table, f)?;
            } else if let Some(role) = b.account {
                self.collector.role(key, table, role)?;
            }
        }
        if let Some((last, head)) = derefs.split_last()
            && !last.targets.is_empty()
            && last
                .targets
                .iter()
                .all(|&(t, f)| self.collector.schema.is_presentation_field(t, f))
        {
            self.collector.derefs(path.clone(), head)?;
            path.steps.extend(head.iter().map(|d| d.targets.clone()));
            let mut ty = crate::QType::default();
            for &(t, _) in &last.targets {
                if let Some(atom) = self.collector.schema.table(t).reference {
                    ty = ty.merge(&crate::QType::reference(atom));
                }
            }
            self.collector.present(path, &ty, false, self.storage)?;
        } else {
            self.collector.derefs(path, derefs)?;
        }
        Ok(())
    }

    fn section_aggregate(&mut self, arg: &TExpr, levels: &Levels) -> Result<(), ExecError> {
        let mut stack = vec![arg];
        let mut owner = None;
        while let Some(e) = stack.pop() {
            if let TKind::Field {
                level,
                source,
                col: Col::Field(field),
                ..
            } = &e.kind
            {
                owner = Self::lookup(levels, *level, *source).map(|b| (b, *field));
                break;
            }
            crate::sema::select::each_child(e, &mut |child| stack.push(child));
        }
        let Some((owner, field)) = owner else {
            return Err(ExecError::Fatal(
                "security section aggregate has no owner".into(),
            ));
        };
        let Some(owner_table) = owner.table else {
            return self.expr(arg, levels);
        };
        let Some(table) = self
            .collector
            .schema
            .field(owner_table, field)
            .and_then(|f| f.nested)
        else {
            return Err(ExecError::Fatal(
                "security aggregate field has no section".into(),
            ));
        };
        self.collector
            .role(Occurrence::Direct(owner.src), owner_table, StdRole::Ref)?;
        let section = self.binding(Some(table), false)?;
        self.collector
            .role(Occurrence::Direct(section.src), table, StdRole::Ref)?;
        let previous = self.section.replace((owner.src, field, section));
        let result = self.expr(arg, levels);
        self.section = previous;
        result
    }

    fn expr(&mut self, e: &TExpr, levels: &Levels) -> Result<(), ExecError> {
        match &e.kind {
            TKind::Field {
                level,
                source,
                col,
                derefs,
            } => {
                self.field_expr(*level, *source, *col, derefs, levels)?;
            }
            TKind::Neg(x)
            | TKind::Not(x)
            | TKind::IsNull { l: x, .. }
            | TKind::Refs { l: x, .. } => self.expr(x, levels)?,
            TKind::Arith { first, steps } => {
                self.expr(first, levels)?;
                for step in steps {
                    self.expr(&step.rhs, levels)?;
                }
            }
            TKind::And(xs) | TKind::Or(xs) => {
                for x in xs {
                    self.expr(x, levels)?;
                }
            }
            TKind::Compare(_, l, r) => {
                self.expr(l, levels)?;
                self.expr(r, levels)?;
            }
            TKind::InList {
                l, rhs, hierarchy, ..
            } => {
                if *hierarchy {
                    for x in l {
                        let path = self.path(x, levels);
                        self.collector.hierarchy(path, &x.ty, false)?;
                    }
                }
                for x in l.iter().chain(rhs) {
                    self.expr(x, levels)?;
                }
            }
            TKind::InQuery {
                l, q, hierarchy, ..
            } => {
                if *hierarchy {
                    for x in l {
                        let path = self.path(x, levels);
                        self.collector.hierarchy(path, &x.ty, false)?;
                    }
                }
                for x in l {
                    self.expr(x, levels)?;
                }
                self.query(q, levels)?;
            }
            TKind::Between { l, lo, hi, .. } => {
                for x in [l, lo, hi] {
                    self.expr(x, levels)?;
                }
            }
            TKind::Like { l, pattern, .. } => {
                self.expr(l, levels)?;
                self.expr(pattern, levels)?;
            }
            TKind::Case {
                operand,
                whens,
                else_,
            } => {
                for x in operand.iter().chain(else_) {
                    self.expr(x, levels)?;
                }
                for (when, then) in whens {
                    self.expr(when, levels)?;
                    self.expr(then, levels)?;
                }
            }
            TKind::Cast { e: x, derefs, .. } => {
                self.expr(x, levels)?;
                let path = self.path(x, levels);
                self.collector.derefs(path, derefs)?;
            }
            TKind::Func { f, args } => {
                for x in args {
                    self.expr(x, levels)?;
                }
                if matches!(
                    f,
                    query_syntax::Func::Presentation | query_syntax::Func::RefPresentation
                ) {
                    for x in args {
                        let path = self.path(x, levels);
                        self.collector.present(
                            path,
                            &x.ty,
                            self.static_analysis
                                && x.ty == crate::QType::default()
                                && !matches!(x.kind, TKind::Const(_)),
                            self.storage,
                        )?;
                    }
                }
            }
            TKind::Agg { arg, nested, .. } => {
                if let Some(x) = arg {
                    if *nested {
                        self.section_aggregate(x, levels)?;
                    } else {
                        self.expr(x, levels)?;
                    }
                }
            }
            TKind::Const(_) | TKind::Value(_) | TKind::Param(_) | TKind::Unit(_) => {}
        }
        Ok(())
    }
}

pub(in crate::bind) fn preflight_virtual(
    stmt: &crate::CompiledStmt,
    ctx: &BindCtx<'_>,
) -> Result<(), ExecError> {
    let mut unsupported = false;
    crate::sema::stats::visit_schema_sources(stmt, &mut |id| {
        unsupported |= match ctx.schema.table(id).kind {
            TableKind::Virtual(_) | TableKind::Changes => !ctx.schema.supports_native_vt(id),
            _ => false,
        };
    });
    if !unsupported {
        return Ok(());
    }
    let schema = ctx.schema;
    let storage = ctx.storage;
    let collector = query_syntax::with_deep_stack(stmt.typed.max_depth, || {
        let mut t = Typed {
            collector: Collector::new(schema),
            storage,
            next: 0,
            static_analysis: stmt.typed.static_analysis,
            section: None,
        };
        if let tir::StmtKind::Query(q) = &stmt.typed.kind {
            t.query(q, &Vec::new())?;
        }
        Ok::<_, ExecError>(t.collector)
    })?;
    collector.finish(ctx).map(|_| ())
}
