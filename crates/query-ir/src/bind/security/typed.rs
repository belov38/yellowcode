mod order;
mod qr8;

use super::{Collector, Occurrence, Path, Root};
use crate::bind::BindCtx;
use crate::exec_error::ExecError;
use crate::host::Access;
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

struct DrCrVisit<'d> {
    dependency: &'d tir::DrCrArgDependency,
    binding: Option<Binding>,
    section: Option<Binding>,
}

struct Typed<'a> {
    collector: Collector<'a, 'a>,
    storage: &'a crate::StorageBinding,
    next: u32,
    static_analysis: bool,
    section: Option<(SrcId, crate::QFieldId, Binding)>,
    drcr_sections: Vec<(SrcId, Path)>,
    membership: bool,
}

impl Typed<'_> {
    fn drcr_dependencies(
        &mut self,
        dependencies: &[tir::DrCrArgDependency],
        parent: Binding,
    ) -> Result<(), ExecError> {
        let saved = self.drcr_sections.len();
        let result = (|| {
            let mut visits = Vec::new();
            self.drcr_visits(dependencies, parent, None, &mut visits)?;
            visits.sort_unstable_by_key(|v| (v.dependency.argument, v.dependency.ordinal));
            for visit in visits {
                let levels = vec![vec![visit.binding]];
                match &visit.dependency.read {
                    tir::DrCrArgRead::Expression(e) => self.expr(e, &levels)?,
                    tir::DrCrArgRead::Wildcard(w) => {
                        self.expr(&w.base, &levels)?;
                        let base = self.path(&w.base, &levels);
                        for target in &w.targets {
                            let mut path = base.clone();
                            path.steps
                                .extend(target.path.iter().map(|d| d.targets.clone()));
                            let occurrence = Occurrence::Target(path, target.table);
                            self.collector.register(
                                occurrence.clone(),
                                target.table,
                                target.all_fields,
                            )?;
                            for field in &target.fields {
                                self.collector
                                    .field(occurrence.clone(), target.table, *field)?;
                            }
                        }
                    }
                    tir::DrCrArgRead::Nested(n) => {
                        self.expr(&n.base, &levels)?;
                        let child = visit.section.ok_or_else(|| {
                            ExecError::Fatal("missing DrCr section binding".into())
                        })?;
                        let path = self
                            .drcr_sections
                            .iter()
                            .find(|(src, _)| *src == child.src)
                            .map(|(_, path)| path.clone())
                            .ok_or_else(|| ExecError::Fatal("missing DrCr section path".into()))?;
                        self.collector.register(
                            Occurrence::Target(path, n.table),
                            n.table,
                            false,
                        )?;
                    }
                }
            }
            Ok(())
        })();
        self.drcr_sections.truncate(saved);
        result
    }

    fn drcr_visits<'d>(
        &mut self,
        dependencies: &'d [tir::DrCrArgDependency],
        parent: Binding,
        nested: Option<Binding>,
        out: &mut Vec<DrCrVisit<'d>>,
    ) -> Result<(), ExecError> {
        for dependency in dependencies {
            let binding = match dependency.scope {
                tir::DrCrArgScope::NoSources => None,
                tir::DrCrArgScope::Condition => Some(parent),
                tir::DrCrArgScope::Account(role) => Some(Binding {
                    account: Some(role),
                    ..parent
                }),
                tir::DrCrArgScope::Nested(table) => {
                    Some(nested.filter(|b| b.table == Some(table)).ok_or_else(|| {
                        ExecError::Fatal("invalid DrCr nested security scope".into())
                    })?)
                }
            };
            let section = if let tir::DrCrArgRead::Nested(n) = &dependency.read {
                let levels = vec![vec![binding]];
                let path = self.path(&n.base, &levels);
                let child = Binding {
                    src: SrcId(self.next),
                    table: Some(n.table),
                    account: None,
                };
                self.next += 1;
                self.drcr_sections.push((child.src, path));
                self.drcr_visits(&n.dependencies, parent, Some(child), out)?;
                Some(child)
            } else {
                None
            };
            out.push(DrCrVisit {
                dependency,
                binding,
                section,
            });
        }
        Ok(())
    }

    fn binding(&mut self, table: Option<QTableId>, all: bool) -> Result<Binding, ExecError> {
        let src = SrcId(self.next);
        self.next += 1;
        if let Some(table) = table {
            self.collector
                .register(Occurrence::Direct(src), table, all)?;
            if self.membership
                && (crate::lower::is_accounting_balance(self.collector.schema, table)
                    || crate::lower::is_accounting_turnovers(self.collector.schema, table))
                && let Some(proof) = self.collector.proof.as_deref_mut()
            {
                proof.consumer(src);
            }
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

    fn drcr_section_path(&self, b: Binding, col: Col) -> Option<Path> {
        let (_, path) = self
            .drcr_sections
            .iter()
            .rev()
            .find(|(src, _)| *src == b.src)?;
        let Col::Field(field) = col else { return None };
        let mut path = path.clone();
        path.steps.push(vec![(b.table?, field)]);
        Some(path)
    }

    fn binding_occurrence(&self, b: Binding) -> Occurrence {
        match (
            self.drcr_sections
                .iter()
                .rev()
                .find(|(src, _)| *src == b.src),
            b.table,
        ) {
            (Some((_, path)), Some(table)) => Occurrence::Target(path.clone(), table),
            _ => Occurrence::Direct(b.src),
        }
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
                if let Some(mut path) = self.drcr_section_path(b, *col) {
                    path.steps.extend(derefs.iter().map(|d| d.targets.clone()));
                    return path;
                }
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
            From::Table {
                id,
                args,
                drcr,
                accounting_dependencies,
                ..
            } => {
                let periods = if crate::lower::is_accounting_balance(self.collector.schema, *id) {
                    1
                } else if crate::lower::is_accounting_turnovers(self.collector.schema, *id) {
                    2
                } else {
                    0
                };
                if let Some(binding) = binding {
                    for expression in args.iter().take(periods).flatten() {
                        match &expression.kind {
                            TKind::Const(crate::QValue::Date(d))
                                if *d != bsl_prim::BslDate::EMPTY =>
                            {
                                self.collector.role(
                                    Occurrence::Direct(binding.src),
                                    *id,
                                    StdRole::Period,
                                )?;
                            }
                            TKind::Param(name)
                                if tir::AccountingArguments::plain_date(expression) =>
                            {
                                self.collector.accounting_params.push((
                                    binding.src,
                                    *id,
                                    name.clone(),
                                ));
                            }
                            _ => {}
                        }
                    }
                }
                if self.collector.schema.table(*id).kind
                    == TableKind::Virtual(crate::VtKind::RecordsWithExtDimensions)
                    && args.iter().take(2).any(Option::is_some)
                    && let Some(binding) = binding
                {
                    for role in [StdRole::Period, StdRole::Recorder] {
                        self.collector
                            .role(Occurrence::Direct(binding.src), *id, role)?;
                    }
                }
                if let (Some(drcr), Some(binding)) = (drcr, binding) {
                    self.drcr_dependencies(&drcr.dependencies, binding)?;
                }
                if let (Some(capture), Some(binding)) = (accounting_dependencies, binding) {
                    self.drcr_dependencies(&capture.dependencies, binding)?;
                }
                if accounting_dependencies.is_none() {
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
            }
            From::Subquery(q) => {
                let outer = levels
                    .get(1..)
                    .ok_or_else(|| ExecError::Fatal("security source scope is missing".into()))?
                    .to_vec();
                self.query(q, &outer, binding.map(|b| b.src))?;
            }
            From::Temp(shape) => {
                self.collector.temp_scan_seen = true;
                if let (Some(binding), Some(proof)) = (binding, self.collector.proof.as_deref_mut())
                {
                    proof.import_temp(
                        binding.src,
                        &shape.name,
                        crate::bind::temp_provenance::TempShape::Typed(shape),
                        self.collector.temp_origins,
                        self.collector.schema.version(),
                    );
                }
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
            if item.nested.is_some()
                && let Some(proof) = self.collector.proof.as_deref_mut()
            {
                proof.disable();
            }
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

    fn proof_items(&mut self, output: SrcId, items: &[tir::Item], levels: &Levels) {
        use super::provenance::Key;
        let schema = self.collector.schema;
        let Some(proof) = self.collector.proof.as_deref_mut() else {
            return;
        };
        for (column, item) in items.iter().enumerate() {
            let Ok(column) = u16::try_from(column) else {
                proof.disable();
                return;
            };
            let out = Key::column(output, column);
            let mut e = &item.expr;
            loop {
                if !proof.spend(1) {
                    return;
                }
                match &e.kind {
                    TKind::Cast {
                        e: inner, derefs, ..
                    } if derefs.is_empty() => e = inner,
                    TKind::Field {
                        level,
                        source,
                        col,
                        derefs,
                    } if derefs.is_empty() => {
                        let Some(binding) = Self::lookup(levels, *level, *source) else {
                            proof.unknown(out);
                            break;
                        };
                        let col = match *col {
                            Col::Field(f) => ColSel::Field(f),
                            Col::Column(i) => ColSel::Column(i),
                        };
                        let input = Key::Column(binding.src, col);
                        if let (Some(table), ColSel::Field(field)) = (binding.table, col) {
                            proof.field(input, binding.src, table, field, schema);
                        }
                        proof.copy(out, input);
                        break;
                    }
                    TKind::Const(crate::QValue::Null) => {
                        proof.null(out);
                        break;
                    }
                    TKind::Const(_) | TKind::Param(_) | TKind::Value(_) => {
                        proof.safe(out);
                        break;
                    }
                    _ => {
                        proof.unknown(out);
                        break;
                    }
                }
            }
        }
    }

    fn order_with_proof(&mut self, query: &tir::Query, levels: &Levels) -> Result<(), ExecError> {
        let previous = self
            .collector
            .proof
            .as_deref_mut()
            .map(|proof| proof.set_ordering(true));
        let result = self.order(query, levels);
        if let Some(previous) = previous
            && let Some(proof) = self.collector.proof.as_deref_mut()
        {
            proof.set_ordering(previous);
        }
        result
    }

    fn query(
        &mut self,
        query: &tir::Query,
        outer: &Levels,
        output: Option<SrcId>,
    ) -> Result<(), ExecError> {
        if query.totals.is_some()
            && let Some(proof) = self.collector.proof.as_deref_mut()
        {
            proof.disable();
        }
        let arm_order = query.autoorder
            && query.parts.len() > 1
            && !query.order.is_empty()
            && query.order.iter().all(|key| key.col.is_some());
        let mut first = None;
        for select in &query.parts {
            let levels = self.select(select, outer)?;
            if let Some(output) = output {
                self.proof_items(output, &select.items, &levels);
            }
            if arm_order {
                let order = query
                    .order
                    .iter()
                    .map(|key| {
                        let item = key
                            .col
                            .and_then(|column| select.items.get(column))
                            .ok_or_else(|| {
                                ExecError::Fatal(
                                    "an order key of a union outside the select list".into(),
                                )
                            })?;
                        Ok(tir::OrderKey {
                            expr: item.expr.clone(),
                            col: key.col,
                            dir: key.dir,
                        })
                    })
                    .collect::<Result<Vec<_>, ExecError>>()?;
                let arm = tir::Query {
                    parts: Vec::new(),
                    unions: Vec::new(),
                    order,
                    autoorder: true,
                    totals: None,
                };
                self.order_with_proof(&arm, &levels)?;
            }
            if first.is_none() {
                first = Some(levels);
            }
        }
        let levels = first.unwrap_or_default();
        for key in &query.order {
            self.expr(&key.expr, &levels)?;
        }
        if !arm_order {
            self.order_with_proof(query, &levels)?;
        }
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
        let mut path = b
            .and_then(|b| self.drcr_section_path(b, col))
            .unwrap_or_else(|| match b {
                Some(b) => Path {
                    root: match (col, b.account) {
                        (Col::Field(f), _) => Root::Column(b.src, ColSel::Field(f)),
                        (Col::Column(_), Some(role)) => Root::Standard(b.src, role),
                        (Col::Column(i), None) => Root::Column(b.src, ColSel::Column(i)),
                    },
                    steps: Vec::new(),
                },
                None => self.collector.opaque(),
            });
        if let Some(b) = b
            && let Some(table) = b.table
        {
            let key = self.binding_occurrence(b);
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
        if let Some(proof) = self.collector.proof.as_deref_mut() {
            proof.disable();
        }
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
                    if let Some(proof) = self.collector.proof.as_deref_mut() {
                        proof.disable();
                    }
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
                    if let Some(proof) = self.collector.proof.as_deref_mut() {
                        proof.disable();
                    }
                    for x in l {
                        let path = self.path(x, levels);
                        self.collector.hierarchy(path, &x.ty, false)?;
                    }
                }
                for x in l {
                    self.expr(x, levels)?;
                }
                let old = self.membership;
                self.membership = true;
                let result = self.query(q, levels, None);
                self.membership = old;
                result?;
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
    preflight_with_origins(stmt, ctx, None)
}

pub(in crate::bind) fn preflight_with_origins(
    stmt: &crate::CompiledStmt,
    ctx: &BindCtx<'_>,
    temps: Option<&dyn crate::bind::temp_provenance::TempOriginProvider>,
) -> Result<(), ExecError> {
    let mut unsupported = false;
    let mut has_accounting = false;
    let mut has_calculation = false;
    let mut calculation_supplied = false;
    let mut calculation_unsupported = None;
    let mut drcr_supplied = false;
    let mut drcr_unsupported = false;
    let mut turnovers_supplied = false;
    let mut balance_supplied = false;
    let mut balance_unsupported = false;
    let mut turnovers_unsupported = false;
    let mut deferred: Option<(u32, crate::tir::RecordsDeferred)> = None;
    let mut composition = false;
    crate::sema::stats::visit_table_sources(stmt, &mut |source| {
        let From::Table {
            id,
            args,
            records,
            accounting,
            accounting_dependencies,
            drcr,
            span,
            ..
        } = source
        else {
            return;
        };
        if let TableKind::Virtual(kind) = ctx.schema.table(*id).kind
            && super::calculation::code(kind).is_some()
        {
            has_calculation = true;
            calculation_supplied |= !args.is_empty();
            if accounting_dependencies
                .as_ref()
                .is_none_or(|capture| !capture.complete)
            {
                calculation_unsupported = Some(if kind == crate::VtKind::ScheduleData {
                    "Q6c-ScheduleDataArguments"
                } else {
                    "Q6c-ActualActionPeriodArguments"
                });
            }
        }
        has_accounting |= crate::lower::is_accounting_balance(ctx.schema, *id)
            || crate::lower::is_accounting_turnovers(ctx.schema, *id);
        drcr_supplied |= drcr
            .as_ref()
            .is_some_and(|d| d.supplied.iter().any(|x| *x != 0));
        drcr_unsupported |= drcr
            .as_ref()
            .is_some_and(crate::tir::DrCrSource::has_unsupported_arguments);
        balance_supplied |=
            crate::lower::is_accounting_balance(ctx.schema, *id) && !args.is_empty();
        turnovers_supplied |=
            crate::lower::is_accounting_turnovers(ctx.schema, *id) && !args.is_empty();
        balance_unsupported |= crate::lower::is_accounting_balance(ctx.schema, *id)
            && !accounting.is_some_and(|a| a.supports(args, 1));
        turnovers_unsupported |= crate::lower::is_accounting_turnovers(ctx.schema, *id)
            && !accounting.is_some_and(|a| a.supports(args, 2));
        if let Some(reason) = records.as_ref().and_then(|r| r.deferred) {
            composition |= reason == crate::tir::RecordsDeferred::Composition;
            let at = span.map_or(u32::MAX, |s| s.start);
            if deferred.is_none_or(|(old, _)| at < old) {
                deferred = Some((at, reason));
            }
        }
        unsupported |= match ctx.schema.table(*id).kind {
            TableKind::Virtual(_) | TableKind::Changes => !ctx.schema.supports_native_vt(*id),
            _ => false,
        };
    });
    if composition {
        return Err(ExecError::Later {
            what: crate::tir::RecordsDeferred::Composition.what(),
            stage: "Q6a",
        });
    }
    if !(has_calculation && stmt.temp_out.is_some())
        && !calculation_supplied
        && calculation_unsupported.is_none()
        && !(has_accounting && stmt.temp_out.is_some())
        && !unsupported
        && deferred.is_none()
        && !drcr_supplied
        && !turnovers_supplied
        && !balance_supplied
        && !balance_unsupported
        && !turnovers_unsupported
    {
        return Ok(());
    }
    let schema = ctx.schema;
    let storage = ctx.storage;
    let mut proof = super::provenance::Workspace::new();
    let mut collector = query_syntax::with_deep_stack(stmt.typed.max_depth, || {
        let mut collector = Collector::new(schema);
        proof.enable_temp_origins(temps.is_some());
        collector.temp_origins = temps;
        collector.proof = Some(&mut proof.proof);
        let mut t = Typed {
            collector,
            storage,
            next: 0,
            static_analysis: stmt.typed.static_analysis,
            section: None,
            drcr_sections: Vec::new(),
            membership: stmt.temp_out.is_some(),
        };
        if let tir::StmtKind::Query(q) = &stmt.typed.kind {
            t.query(q, &Vec::new(), None)?;
        }
        Ok::<_, ExecError>(t.collector)
    })?;
    collector.accounting_period_params(ctx.params)?;
    let decisions = collector.finish(ctx)?.decisions;
    if stmt.temp_out.is_some() {
        for (key, table, access) in &decisions {
            if matches!(access, Access::Restricted(_))
                && let Some(what) = super::calculation::occurrence_code(key, table)
            {
                return Err(super::calculation::refusal(what));
            }
            let balance = crate::lower::is_accounting_balance(ctx.schema, table.table)
                || matches!(key, Occurrence::BalanceChart(_));
            if matches!(access, Access::Restricted(_))
                && (balance || crate::lower::is_accounting_turnovers(ctx.schema, table.table))
            {
                return Err(super::accounting::refusal(balance));
            }
        }
    }
    if let Some(what) = calculation_unsupported {
        return Err(ExecError::Later { what, stage: "Q6c" });
    }
    if balance_unsupported {
        return Err(ExecError::Later {
            what: "Q6a-AccountingBalanceArguments",
            stage: "Q6a",
        });
    }
    if turnovers_unsupported {
        return Err(ExecError::Later {
            what: "Q6b-TurnoversArguments",
            stage: "Q6b",
        });
    }
    if drcr_unsupported {
        return Err(ExecError::Later {
            what: "Q6b-DrCrTurnoversArguments",
            stage: "Q6b",
        });
    }
    if let Some((_, reason)) = deferred {
        return Err(ExecError::Later {
            what: reason.what(),
            stage: "Q6a",
        });
    }
    Ok(())
}
