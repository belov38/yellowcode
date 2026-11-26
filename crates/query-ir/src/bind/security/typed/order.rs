use super::{Binding, Levels, Typed};
use crate::exec_error::ExecError;
use crate::schema::{FieldRole, QFieldId, StdRole, TableKind};
use crate::tir::{self, TExpr, TKind};
use mdschema::MdKind;
use query_syntax::ast::OrderDir;

impl Typed<'_> {
    fn order_source(e: &TExpr, levels: &Levels) -> Option<Binding> {
        match &e.kind {
            TKind::Field {
                level,
                source,
                derefs,
                ..
            } if derefs.is_empty() => Self::lookup(levels, *level, *source),
            TKind::Cast { e, derefs, .. } if derefs.is_empty() => Self::order_source(e, levels),
            _ => None,
        }
    }

    fn order_hierarchy(&mut self, binding: Binding) -> Result<(), ExecError> {
        if let Some(table) = binding.table {
            let fields = &self.collector.schema.table(table).fields;
            if fields
                .iter()
                .any(|f| f.role == FieldRole::Std(StdRole::Parent))
                && fields
                    .iter()
                    .any(|f| f.role == FieldRole::Std(StdRole::Ref))
            {
                for role in [StdRole::Ref, StdRole::Parent] {
                    self.collector
                        .role(super::Occurrence::Direct(binding.src), table, role)?;
                }
            }
        }
        Ok(())
    }

    fn chart_code_order(&mut self, e: &TExpr, levels: &Levels) -> Result<(), ExecError> {
        let has_order = |table: crate::QTableId| {
            let qt = self.collector.schema.table(table);
            qt.auto_order_by_code
                && qt
                    .fields
                    .iter()
                    .any(|f| f.role == FieldRole::Std(StdRole::Order))
        };
        match &e.kind {
            TKind::Field {
                level,
                source,
                col: tir::Col::Field(field),
                derefs,
            } if derefs.is_empty() => {
                if let Some(binding) = Self::lookup(levels, *level, *source)
                    && let Some(table) = binding.table
                    && has_order(table)
                    && self
                        .collector
                        .schema
                        .field(table, *field)
                        .is_some_and(|f| f.role == FieldRole::Std(StdRole::Code))
                {
                    self.collector.role(
                        super::Occurrence::Direct(binding.src),
                        table,
                        StdRole::Order,
                    )?;
                }
            }
            TKind::Field { derefs, .. } | TKind::Cast { derefs, .. } if !derefs.is_empty() => {
                if let Some(last) = derefs.last()
                    && !last.targets.is_empty()
                    && last.targets.iter().all(|(t, f)| {
                        self.collector
                            .schema
                            .field(*t, *f)
                            .is_some_and(|fi| fi.role == FieldRole::Std(StdRole::Code))
                    })
                    && last.targets.iter().any(|(t, _)| has_order(*t))
                {
                    let mut prefix = self.path(e, levels);
                    prefix.steps.pop();
                    for &(t, _) in &last.targets {
                        if has_order(t) {
                            self.collector.role(
                                super::Occurrence::Target(prefix.clone(), t),
                                t,
                                StdRole::Order,
                            )?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn auto_order(&mut self, e: &TExpr, levels: &Levels) -> Result<(), ExecError> {
        let mixed = e.ty.refs.len() > 1
            || e.ty.boolean
            || e.ty.number.is_some()
            || e.ty.string.is_some()
            || e.ty.date.is_some()
            || !e.ty.others.is_empty();
        let path = self.path(e, levels);
        for atom in &e.ty.refs {
            let info = self.collector.schema.ref_info(*atom);
            let table = info.table;
            let mut roles = match info.kind {
                MdKind::Catalog => vec![StdRole::IsFolder, StdRole::Description],
                MdKind::Document => vec![StdRole::Date],
                MdKind::Enum => vec![StdRole::Order],
                MdKind::ChartOfAccounts => {
                    if self.collector.schema.table(table).auto_order_by_code {
                        vec![StdRole::Order, StdRole::Code]
                    } else {
                        vec![StdRole::Code]
                    }
                }
                _ => Vec::new(),
            };
            if mixed {
                roles.push(StdRole::Ref);
            }
            for role in roles {
                if self
                    .collector
                    .schema
                    .table(table)
                    .fields
                    .iter()
                    .any(|f| f.role == FieldRole::Std(role))
                {
                    self.collector.role(
                        super::Occurrence::Target(path.clone(), table),
                        table,
                        role,
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn order(&mut self, query: &tir::Query, levels: &Levels) -> Result<(), ExecError> {
        for key in &query.order {
            if matches!(key.dir, OrderDir::Hierarchy | OrderDir::HierarchyDesc) {
                let mut direct_table = None;
                if let Some(binding) = Self::order_source(&key.expr, levels) {
                    direct_table = binding.table;
                    self.order_hierarchy(binding)?;
                }
                if !key.expr.ty.refs.is_empty() {
                    let path = self.path(&key.expr, levels);
                    for &atom in &key.expr.ty.refs {
                        if let Some(table) = self.collector.schema.hierarchical_table(atom)
                            && direct_table != Some(table)
                        {
                            self.collector.role(
                                super::Occurrence::Target(path.clone(), table),
                                table,
                                StdRole::Parent,
                            )?;
                        }
                    }
                }
            }
            if query.autoorder {
                self.auto_order(&key.expr, levels)?;
            }
            self.chart_code_order(&key.expr, levels)?;
        }
        if !query.autoorder || !query.order.is_empty() {
            return Ok(());
        }
        let Some(select) = query.parts.first() else {
            return Ok(());
        };
        let aggregated = select.group.is_some()
            || select.having.is_some()
            || select
                .items
                .iter()
                .any(|i| crate::sema::select::has_agg(&i.expr));
        if aggregated {
            for e in select.group.iter().flatten().flatten() {
                self.auto_order(e, levels)?;
            }
            return Ok(());
        }
        if select.distinct {
            return Ok(());
        }
        let Some(binding) = levels.first().and_then(|l| l.first()).copied().flatten() else {
            return Ok(());
        };
        let Some(table) = binding.table else {
            return Ok(());
        };
        let qt = self.collector.schema.table(table);
        let keys: Vec<_> = match qt.kind {
            TableKind::Object(kind) => {
                let roles: &[StdRole] = match kind {
                    MdKind::Catalog | MdKind::Document | MdKind::Enum | MdKind::ChartOfAccounts => {
                        &[StdRole::Ref]
                    }
                    MdKind::AccumulationRegister => {
                        &[StdRole::Period, StdRole::Recorder, StdRole::LineNumber]
                    }
                    MdKind::InformationRegister => &[StdRole::Period],
                    _ => &[],
                };
                if matches!(kind, MdKind::Catalog | MdKind::ChartOfAccounts) {
                    self.order_hierarchy(binding)?;
                }
                qt.fields
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| {
                        matches!(f.role, FieldRole::Std(r) if roles.contains(&r))
                            || (kind == MdKind::InformationRegister
                                && f.role == FieldRole::Dimension)
                    })
                    .filter_map(|(i, f)| u16::try_from(i).ok().map(|i| (QFieldId(i), f.ty.clone())))
                    .collect()
            }
            _ if crate::sema::select::is_accum_balance(self.collector.schema, table) => {
                let mut extra = Vec::new();
                if let Some(totals) = &query.totals {
                    for (e, _) in &totals.items {
                        extra.push(e);
                    }
                    for point in &totals.points {
                        if let tir::ControlPoint::Field(p) = point {
                            extra.push(&p.expr);
                            if let Some(periods) = &p.periods {
                                extra.extend(periods.begin.iter().chain(&periods.end));
                            }
                        }
                    }
                }
                qt.fields
                    .iter()
                    .enumerate()
                    .filter_map(|(i, f)| {
                        if f.role != FieldRole::Dimension {
                            return None;
                        }
                        let id = QFieldId(u16::try_from(i).ok()?);
                        crate::sema::select::balance_dim_used(select, &extra, id)
                            .then(|| (id, f.ty.clone()))
                    })
                    .collect()
            }
            _ => return Ok(()),
        };
        for (field, ty) in keys {
            self.collector
                .field(super::Occurrence::Direct(binding.src), table, field)?;
            let e = TExpr {
                kind: TKind::Field {
                    level: 0,
                    source: 0,
                    col: tir::Col::Field(field),
                    derefs: Vec::new(),
                },
                ty,
                span: query_syntax::Span::default(),
            };
            self.auto_order(&e, levels)?;
        }
        Ok(())
    }
}
