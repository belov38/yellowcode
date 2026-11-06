pub(crate) mod accounting;
pub(crate) mod calculation;
mod payload;
pub(in crate::bind) mod provenance;
use super::BindCtx;
use crate::exec_error::{ExecError, RuntimeError};
use crate::host::{Access, FieldSet, SourceTable, TablePart};
use crate::ir::{Rel, SrcId, SrcKind, StmtS};
use crate::schema::{QTableId, QuerySchema, TableKind, VtKind};
use query_syntax::{MsgArg, MsgId, QueryError};

fn standard_section(
    schema: &QuerySchema,
    table: QTableId,
) -> Result<(TablePart, String), ExecError> {
    use crate::schema::{FieldRole, StdRole};
    use mdschema::MdKind;
    let unknown = || ExecError::Later {
        what: "неизвестная стандартная табличная часть",
        stage: "Q7",
    };
    let t = schema.table(table);
    let owner_id = t.owner.ok_or_else(unknown)?;
    let owner = schema.table(owner_id);
    let reference = schema.ref_info(owner.reference.ok_or_else(unknown)?);
    if reference.table != owner_id || Some(reference.object) != owner.object {
        return Err(unknown());
    }
    let mut fields = owner.fields.iter().filter(|f| f.nested == Some(table));
    let field = fields.next().ok_or_else(unknown)?;
    if fields.next().is_some() {
        return Err(unknown());
    }
    let FieldRole::Std(role) = field.role else {
        return Err(unknown());
    };
    let name = match (reference.kind, role) {
        (MdKind::ChartOfAccounts, StdRole::ExtDimensionTypes) => "ExtDimensionTypes",
        (MdKind::ChartOfCalculationTypes, StdRole::LeadingCalculationTypes) => {
            "LeadingCalculationTypes"
        }
        (MdKind::ChartOfCalculationTypes, StdRole::BaseCalculationTypes) => "BaseCalculationTypes",
        (MdKind::ChartOfCalculationTypes, StdRole::DisplacingCalculationTypes) => {
            "DisplacingCalculationTypes"
        }
        _ => return Err(unknown()),
    };
    if field.name_en != name || t.name_en != format!("{}.{name}", owner.name_en) {
        return Err(unknown());
    }
    Ok((
        TablePart::Standard(role),
        format!("{}.StandardTabularSection.{name}", owner.name_en),
    ))
}

fn source(schema: &QuerySchema, table: QTableId) -> Result<Option<SourceTable>, ExecError> {
    let t = schema.table(table);
    let owner = t.owner.map(|id| schema.table(id)).unwrap_or(t);
    let Some(object) = owner.object else {
        return Ok(None);
    };
    if t.kind == TableKind::Tabular && t.object.is_none() {
        let (part, full_name) = standard_section(schema, table)?;
        return Ok(Some(SourceTable {
            object,
            full_name,
            table,
            part,
        }));
    }
    let part = match t.kind {
        TableKind::Tabular => {
            let Some(object) = t.object else {
                return Ok(None);
            };
            TablePart::Tabular(object)
        }
        TableKind::Virtual(kind) => TablePart::Virtual(kind),
        TableKind::Changes => TablePart::Changes,
        _ => TablePart::Main,
    };
    Ok(Some(SourceTable {
        object,
        full_name: match schema.script() {
            query_syntax::ScriptVariant::English => t.name_en.clone(),
            _ => t.name_ru.clone(),
        },
        table,
        part,
    }))
}

fn check(
    accesses: &[(SourceTable, FieldSet)],
    ctx: &BindCtx<'_>,
    keys: &[Occurrence],
    temp_scan_seen: bool,
    proof: Option<&mut provenance::Proof>,
    aliases: &std::collections::HashMap<SrcId, SrcId>,
) -> Result<Vec<Access>, ExecError> {
    let mut decisions = Vec::with_capacity(accesses.len());
    let mut restricted_standard = false;
    for (table, fields) in accesses {
        let decision = ctx
            .security
            .access
            .read_policy(table, fields)
            .map_err(ExecError::Host)?;
        match &decision {
            Access::Deny => {
                let detail = QueryError::new(MsgId::SELECT_ERROR_ACCESSDENIEDTABLE, None)
                    .with_quoted(MsgArg::Text(table.full_name.clone().into()));
                return Err(ExecError::Rights(RuntimeError {
                    chain: vec![
                        QueryError::new(MsgId::SELECT_ERROR_RUNTIMEERROR, None),
                        detail,
                    ],
                }));
            }
            Access::Restricted(_) => {
                restricted_standard |= matches!(table.part, TablePart::Standard(_));
            }
            Access::Unrestricted => {}
        }
        decisions.push(decision);
    }
    if restricted_standard {
        return Err(ExecError::Later {
            what: "Q7a-StandardTabularSectionRights",
            stage: "Q7b",
        });
    }
    accounting::user_dereference_guard(
        accesses,
        keys,
        &decisions,
        ctx.schema,
        temp_scan_seen,
        proof,
        aliases,
    )?;
    for (index, ((table, _), decision)) in accesses.iter().zip(&decisions).enumerate() {
        if !matches!(decision, Access::Restricted(_)) {
            continue;
        }
        let code = if matches!(keys.get(index), Some(Occurrence::Schedule(_))) {
            Some("Q6c-ScheduleDataScheduleRls")
        } else if let TablePart::Virtual(kind) = table.part {
            calculation::code(kind)
        } else {
            None
        };
        if let Some(code) = code {
            calculation::validate(decision, table.table, code, ctx)?;
        }
    }
    for (index, ((table, fields), decision)) in accesses.iter().zip(&decisions).enumerate() {
        let balance = crate::lower::is_accounting_balance(ctx.schema, table.table);
        if (balance || matches!(keys.get(index), Some(Occurrence::BalanceChart(_))))
            && matches!(decision, Access::Restricted(_))
        {
            accounting::validate(decision, table.table, !balance, true, ctx)?;
        }
        if balance {
            crate::lower::balance_form(
                ctx.schema,
                table.table,
                fields.all,
                &fields.standard,
                &fields.fields,
            )?;
            let inputs =
                crate::lower::balance_inputs(ctx.schema, ctx.storage, ctx.map, table.table)?;
            if [inputs.main, inputs.options, inputs.chart]
                .iter()
                .any(|id| !ctx.map.table(*id).separators.is_empty())
            {
                return Err(ExecError::Later {
                    what: "Q6a-AccountingBalanceAreas",
                    stage: "Q7c",
                });
            }
            crate::lower::balance_resources(
                ctx.schema,
                ctx.map,
                table.table,
                &inputs,
                &fields.fields,
            )?;
        }
        if ctx.schema.table(table.table).kind == TableKind::Recalculation
            && ctx
                .storage
                .table(table.table)
                .and_then(|id| ctx.map.try_table(id))
                .is_some_and(|physical| !physical.separators.is_empty())
        {
            return Err(ExecError::Later {
                what: "Q6d-RecalculationAreas",
                stage: "Q7c",
            });
        }
        let (rls, areas) = match table.part {
            TablePart::Virtual(VtKind::ExtDimensions) => {
                ("Q6a-ExtDimensionsRls", "Q6a-ExtDimensionsAreas")
            }
            TablePart::Virtual(VtKind::RecordsWithExtDimensions) => (
                "Q6a-RecordsWithExtDimensionsRls",
                "Q6a-RecordsWithExtDimensionsAreas",
            ),
            TablePart::Virtual(VtKind::ScheduleData) => {
                ("Q6c-ScheduleDataRls", "Q6c-ScheduleDataAreas")
            }
            TablePart::Virtual(VtKind::ActualActionPeriod) => {
                ("Q6c-ActualActionPeriodRls", "Q6c-ActualActionPeriodAreas")
            }
            _ => continue,
        };
        if matches!(decision, Access::Restricted(_))
            && !matches!(
                table.part,
                TablePart::Virtual(VtKind::ScheduleData | VtKind::ActualActionPeriod)
            )
        {
            return Err(ExecError::Later {
                what: rls,
                stage: "Q7b",
            });
        }
        if table.part == TablePart::Virtual(VtKind::ScheduleData) {
            let inputs =
                crate::lower::schedule_inputs(ctx.schema, ctx.storage, ctx.map, table.table)?;
            if inputs
                .tables()
                .iter()
                .any(|id| !ctx.map.table(*id).separators.is_empty())
            {
                return Err(ExecError::Later {
                    what: areas,
                    stage: "Q7c",
                });
            }
        } else if table.part == TablePart::Virtual(VtKind::ActualActionPeriod) {
            let (main, act) =
                crate::lower::actual_inputs(ctx.schema, ctx.storage, ctx.map, table.table)?;
            if [main, act]
                .iter()
                .any(|id| !ctx.map.table(*id).separators.is_empty())
            {
                return Err(ExecError::Later {
                    what: areas,
                    stage: "Q7c",
                });
            }
        } else if ctx
            .storage
            .table(table.table)
            .and_then(|id| ctx.map.try_table(id))
            .is_some_and(|physical| !physical.separators.is_empty())
        {
            return Err(ExecError::Later {
                what: areas,
                stage: "Q7c",
            });
        }
    }
    let drcr = accesses
        .iter()
        .zip(&decisions)
        .filter(|((table, _), _)| table.part == TablePart::Virtual(VtKind::DrCrTurnovers));
    if drcr.clone().next().is_some() {
        if keys.iter().zip(&decisions).any(|(key, access)| {
            matches!(access, Access::Restricted(_))
                && match key {
                    Occurrence::Presentation(..) | Occurrence::HierarchyTarget(..) => true,
                    Occurrence::Target(path, _) => matches!(path.root, Root::Opaque(_)),
                    _ => false,
                }
        }) {
            return Err(ExecError::Later {
                what: "Q7b-DeferredSecurityRead",
                stage: "Q7c",
            });
        }
        if drcr
            .clone()
            .any(|(_, access)| matches!(access, Access::Restricted(_)))
        {
            return Err(ExecError::Later {
                what: "Q6b-DrCrTurnoversRls",
                stage: "Q7b",
            });
        }
        let unscoped_companion =
            drcr.clone().any(|((table, _), _)| {
                crate::lower::drcr_inputs(ctx.schema, ctx.storage, ctx.map, table.table)
                    .has_physical_separators()
            }) && accesses.iter().zip(&decisions).any(|((table, _), access)| {
                if table.part == TablePart::Virtual(VtKind::DrCrTurnovers) {
                    return false;
                }
                if matches!(access, Access::Restricted(_))
                    || !matches!(table.part, TablePart::Main | TablePart::Tabular(_))
                {
                    return true;
                }
                ctx.storage
                    .table(table.table)
                    .and_then(|id| ctx.map.try_table(id))
                    .is_none_or(|physical| !physical.separators.is_empty())
            });
        if unscoped_companion
            || drcr.into_iter().any(|((table, _), _)| {
                let inputs =
                    crate::lower::drcr_inputs(ctx.schema, ctx.storage, ctx.map, table.table);
                inputs.has_physical_separators() && inputs.area_mapping().is_err()
            })
        {
            return Err(ExecError::Later {
                what: "Q6b-DrCrTurnoversAreas",
                stage: "Q7c",
            });
        }
    }
    let turnovers = accesses
        .iter()
        .zip(&decisions)
        .filter(|((table, _), _)| crate::lower::is_accounting_turnovers(ctx.schema, table.table));
    if turnovers.clone().next().is_some() {
        if keys.iter().zip(&decisions).any(|(key, access)| {
            matches!(access, Access::Restricted(_))
                && match key {
                    Occurrence::Presentation(..) | Occurrence::HierarchyTarget(..) => true,
                    Occurrence::Target(path, _) => matches!(path.root, Root::Opaque(_)),
                    _ => false,
                }
        }) {
            return Err(ExecError::Later {
                what: "Q7b-DeferredSecurityRead",
                stage: "Q7c",
            });
        }
        for ((table, _), access) in turnovers.clone() {
            accounting::validate(access, table.table, false, false, ctx)?;
        }
        if turnovers.into_iter().any(|((table, _), _)| {
            crate::lower::turnovers_inputs(ctx.schema, ctx.storage, ctx.map, table.table)
                .has_physical_separators()
        }) {
            return Err(ExecError::Later {
                what: "Q6b-TurnoversAreas",
                stage: "Q7c",
            });
        }
    }
    Ok(decisions)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Root {
    Column(crate::ir::SrcId, crate::ir::ColSel),
    Standard(crate::ir::SrcId, crate::schema::StdRole),
    Param(u16),
    Value(u16),
    Reference(u32, [u8; 16]),
    Opaque(u64),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Path {
    root: Root,
    steps: Vec<Vec<(QTableId, crate::schema::QFieldId)>>,
}

impl Security {
    pub(crate) fn slice_condition_has_deferred_reference(
        &self,
        schema: &QuerySchema,
        slice: &crate::ir::SliceS,
    ) -> bool {
        use crate::ir::ColSel;
        use crate::schema::FieldRole;

        let Some(condition) = &slice.cond else {
            return false;
        };
        if self.conditions.is_empty() {
            return false;
        }
        let mut collector = Collector::new(schema);
        collector.aliases = self.aliases.clone();
        let source = collector.semantic_source(slice.inner);
        let mut paths = Vec::new();
        collector.ir_deref_paths(condition, &mut paths);
        for path in paths {
            if matches!(path.root, Root::Opaque(_)) {
                continue;
            }
            let supported = path.steps.len() == 1
                && match &path.root {
                    Root::Column(src, ColSel::Field(field)) if *src == source => {
                        schema.field(slice.table, *field).is_some_and(|field| {
                            field.role == FieldRole::Dimension
                                && field.nested.is_none()
                                && field.ty.refs.len() == 1
                                && field.ty.atom_count() == 1
                        })
                    }
                    _ => false,
                };
            if supported {
                continue;
            }
            let mut prefix = Path {
                root: path.root,
                steps: Vec::new(),
            };
            for step in path.steps {
                if step.iter().any(|(table, _)| {
                    self.targets
                        .get(&(prefix.clone(), *table))
                        .is_some_and(|source| self.conditions.contains_key(source))
                }) {
                    return true;
                }
                prefix.steps.push(step);
            }
        }
        false
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Occurrence {
    Direct(crate::ir::SrcId),
    Schedule(Box<Occurrence>),
    BalanceChart(Box<Occurrence>),
    Target(Path, QTableId),
    Presentation(Path, QTableId),
    HierarchyTarget(Path, QTableId),
    Hierarchy(usize, QTableId),
    Constant(Box<Occurrence>, QTableId),
}

struct Use {
    source: SourceTable,
    fields: FieldSet,
    metadata: std::collections::HashSet<brace::Guid>,
    standard: std::collections::HashSet<crate::schema::StdRole>,
}

struct Collector<'a, 'p> {
    schema: &'a QuerySchema,
    uses: Vec<Use>,
    index: std::collections::HashMap<Occurrence, usize>,
    opaque: u64,
    aliases: std::collections::HashMap<SrcId, SrcId>,
    record_bounds: Vec<(SrcId, QTableId)>,
    record_bounds_seen: std::collections::HashSet<(SrcId, QTableId)>,
    accounting_params: Vec<(SrcId, QTableId, String)>,
    temp_scan_seen: bool,
    proof: Option<&'p mut provenance::Proof>,
    temp_origins: Option<&'p dyn crate::bind::temp_provenance::TempOriginProvider>,
}

impl<'a, 'p> Collector<'a, 'p> {
    fn new(schema: &'a QuerySchema) -> Self {
        Self {
            schema,
            uses: Vec::new(),
            index: std::collections::HashMap::new(),
            opaque: 0,
            aliases: Default::default(),
            record_bounds: Vec::new(),
            record_bounds_seen: Default::default(),
            accounting_params: Vec::new(),
            temp_scan_seen: false,
            proof: None,
            temp_origins: None,
        }
    }

    fn semantic_source(&self, src: SrcId) -> SrcId {
        self.aliases.get(&src).copied().unwrap_or(src)
    }

    fn record_bound(&mut self, src: SrcId, table: QTableId) {
        if self.record_bounds_seen.insert((src, table)) {
            self.record_bounds.push((src, table));
        }
    }

    fn virtual_aliases(&mut self, rel: &Rel, stmt: &StmtS) {
        match rel {
            Rel::RegisterVt(v) | Rel::Slice(v) | Rel::RunningBalance(v) => {
                self.aliases.insert(v.inner, v.src);
                if v.kind == VtKind::RecordsWithExtDimensions
                    && v.bounds.iter().any(Option::is_some)
                {
                    self.record_bound(v.src, v.table);
                }
                if crate::lower::is_accounting_balance(self.schema, v.table)
                    || crate::lower::is_accounting_turnovers(self.schema, v.table)
                {
                    for expression in v.bounds.iter().flatten() {
                        match &expression.kind {
                            crate::ir::ExprKind::Lit(crate::QValue::Date(d))
                                if *d != bsl_prim::BslDate::EMPTY =>
                            {
                                self.record_bound(v.src, v.table);
                            }
                            crate::ir::ExprKind::Param(slot) => {
                                if let Some(name) = stmt.params.get(usize::from(*slot)) {
                                    self.accounting_params.push((v.src, v.table, name.clone()));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Rel::Subquery { input, .. }
            | Rel::Distinct(input)
            | Rel::Top { input, .. }
            | Rel::Filter { input, .. }
            | Rel::Aggregate { input, .. }
            | Rel::Project { input, .. }
            | Rel::Sort { input, .. }
            | Rel::Hierarchy { input, .. } => self.virtual_aliases(input, stmt),
            Rel::Join { l, r, .. } => {
                self.virtual_aliases(l, stmt);
                self.virtual_aliases(r, stmt);
            }
            Rel::Union { parts, .. } => {
                for part in parts {
                    self.virtual_aliases(part, stmt);
                }
            }
            Rel::TempScan { .. } => self.temp_scan_seen = true,
            Rel::Empty | Rel::Scan { .. } | Rel::ParamScan { .. } => {}
        }
    }

    fn accounting_period_params(
        &mut self,
        params: &dyn crate::ParamValues,
    ) -> Result<(), ExecError> {
        for (src, table, name) in std::mem::take(&mut self.accounting_params) {
            if matches!(params.get(&name), Some(crate::ParamValue::Value(crate::QValue::Date(d))) if d != bsl_prim::BslDate::EMPTY)
            {
                self.role(
                    Occurrence::Direct(src),
                    table,
                    crate::schema::StdRole::Period,
                )?;
            }
        }
        Ok(())
    }

    fn register(
        &mut self,
        key: Occurrence,
        table: QTableId,
        all: bool,
    ) -> Result<Option<usize>, ExecError> {
        if self.schema.table(table).kind == TableKind::Constants {
            return Ok(None);
        }
        let i = if let Some(i) = self.index.get(&key) {
            *i
        } else {
            let source = source(self.schema, table)?.ok_or_else(|| {
                ExecError::Fatal(format!(
                    "security source has no metadata identity: {}",
                    self.schema.table(table).name_ru
                ))
            })?;
            let i = self.uses.len();
            self.uses.push(Use {
                source,
                fields: FieldSet::default(),
                metadata: Default::default(),
                standard: Default::default(),
            });
            self.index.insert(key.clone(), i);
            i
        };
        if let Some(used) = self.uses.get_mut(i) {
            used.fields.all |= all;
            if self.schema.table(table).kind == TableKind::Virtual(VtKind::ActualActionPeriod) {
                for role in [
                    crate::StdRole::Recorder,
                    crate::StdRole::LineNumber,
                    crate::StdRole::BegOfActionPeriod,
                ] {
                    if used.standard.insert(role) {
                        used.fields.standard.push(role);
                    }
                }
            }
            if self.schema.table(table).kind == TableKind::Virtual(VtKind::DrCrTurnovers) {
                for role in [crate::StdRole::Period, crate::StdRole::Active] {
                    if used.standard.insert(role) {
                        used.fields.standard.push(role);
                    }
                }
            }
            if crate::lower::is_accounting_turnovers(self.schema, table) {
                for role in [
                    crate::StdRole::Active,
                    crate::StdRole::AccountDr,
                    crate::StdRole::AccountCr,
                ] {
                    if used.standard.insert(role) {
                        used.fields.standard.push(role);
                    }
                }
            }
        }
        let balance_key =
            crate::lower::is_accounting_balance(self.schema, table).then(|| key.clone());
        if self.schema.table(table).kind == TableKind::Virtual(VtKind::ScheduleData) {
            let roles = self.schema.schedule_roles(table);
            let facts = self.schema.schedule(table).cloned();
            if let Some(used) = self.uses.get_mut(i) {
                for role in roles {
                    if used.standard.insert(role) {
                        used.fields.standard.push(role);
                    }
                }
                for field in facts.iter().flat_map(|f| &f.owner_fields) {
                    if used.metadata.insert(*field) {
                        used.fields.fields.push(*field);
                    }
                }
            }
            if let Some(facts) = facts
                && let Some(graph) = facts.graph
            {
                let dep = self.register(Occurrence::Schedule(Box::new(key)), graph, false)?;
                if let Some(used) = dep.and_then(|n| self.uses.get_mut(n)) {
                    for field in &facts.graph_fields {
                        if used.metadata.insert(*field) {
                            used.fields.fields.push(*field);
                        }
                    }
                }
            }
        }
        if let Some(key) = balance_key {
            if let Some(used) = self.uses.get_mut(i) {
                for role in [
                    crate::StdRole::Active,
                    crate::StdRole::AccountDr,
                    crate::StdRole::AccountCr,
                ] {
                    if used.standard.insert(role) {
                        used.fields.standard.push(role);
                    }
                }
            }
            let chart = crate::lower::balance_chart(self.schema, table)?;
            let dependency = Occurrence::BalanceChart(Box::new(key));
            self.role(dependency.clone(), chart, crate::StdRole::Ref)?;
            self.role(dependency, chart, crate::StdRole::Type)?;
        }
        Ok(Some(i))
    }

    fn role(
        &mut self,
        key: Occurrence,
        table: QTableId,
        role: crate::schema::StdRole,
    ) -> Result<(), ExecError> {
        if let Some(proof) = self.proof.as_deref_mut() {
            proof.order_role(role);
        }
        if let Some(i) = self.register(key, table, false)?
            && let Some(used) = self.uses.get_mut(i)
            && used.standard.insert(role)
        {
            used.fields.standard.push(role);
        }
        Ok(())
    }

    fn field(
        &mut self,
        key: Occurrence,
        table: QTableId,
        field: crate::schema::QFieldId,
    ) -> Result<(), ExecError> {
        use crate::schema::{FieldRole, StdRole};
        let f = self
            .schema
            .field(table, field)
            .ok_or_else(|| ExecError::Fatal("unknown security field".into()))?;
        if self.schema.table(table).kind == TableKind::Constants {
            let target =
                f.md.and_then(|md| self.schema.object_table(md))
                    .ok_or_else(|| ExecError::Fatal("constant has no security identity".into()))?;
            return self.role(
                Occurrence::Constant(Box::new(key), target),
                target,
                StdRole::Value,
            );
        }
        let md = f.md.filter(|_| {
            f.nested.is_none()
                && !(self.schema.table(table).kind == TableKind::Virtual(VtKind::ScheduleData)
                    && f.role == FieldRole::Virtual)
        });
        let role = f.role;
        if let Some(i) = self.register(key, table, false)?
            && let Some(used) = self.uses.get_mut(i)
        {
            if let Some(md) = md
                && used.metadata.insert(md)
            {
                used.fields.fields.push(md);
            }
            if let FieldRole::Std(role) = role
                && used.standard.insert(role)
            {
                used.fields.standard.push(role);
            }
        }
        Ok(())
    }

    fn opaque(&mut self) -> Path {
        let id = self.opaque;
        self.opaque += 1;
        Path {
            root: Root::Opaque(id),
            steps: Vec::new(),
        }
    }

    fn ir_path(&mut self, e: &crate::ir::Expr) -> Path {
        use crate::ir::ExprKind as E;
        let root = match &e.kind {
            E::Col { src, col } => Root::Column(self.semantic_source(*src), *col),
            E::Param(i) => Root::Param(*i),
            E::Value(i) => Root::Value(*i),
            E::Lit(crate::QValue::Ref { table, id }) => Root::Reference(*table, *id),
            E::Cast { x, .. } => return self.ir_path(x),
            E::LocalKey { orig, .. } | E::OuterKey { orig, .. } => return self.ir_path(orig),
            E::Deref { base, steps } => {
                let mut p = self.ir_path(base);
                p.steps.extend(steps.iter().map(|s| s.targets.clone()));
                return p;
            }
            _ => return self.opaque(),
        };
        Path {
            root,
            steps: Vec::new(),
        }
    }

    fn ir_deref_paths(&mut self, e: &crate::ir::Expr, paths: &mut Vec<Path>) {
        use crate::ir::ExprKind as E;
        crate::ir::visit_expr_in_place(e, true, &mut |expr| match &expr.kind {
            E::Deref { .. } => paths.push(self.ir_path(expr)),
            E::LocalKey { orig, .. } | E::OuterKey { orig, .. } => {
                self.ir_deref_paths(orig, paths);
            }
            _ => {}
        });
    }

    fn derefs(&mut self, mut path: Path, steps: &[crate::tir::Deref]) -> Result<(), ExecError> {
        for step in steps {
            for &(table, field) in &step.targets {
                if self
                    .schema
                    .field(table, field)
                    .is_some_and(|f| f.nested.is_some())
                {
                    continue;
                }
                self.field(Occurrence::Target(path.clone(), table), table, field)?;
            }
            path.steps.push(step.targets.clone());
        }
        Ok(())
    }

    fn ir_expr(&mut self, stmt: &StmtS, e: &crate::ir::Expr) -> Result<(), ExecError> {
        use crate::ir::{ColSel, ExprKind as E};
        match &e.kind {
            E::Col { src, col } => {
                let source = stmt.sources.get(src.0 as usize);
                let identity = match (source.map(|s| &s.kind), col) {
                    (
                        Some(SrcKind::Table(table) | SrcKind::Virtual { table, .. }),
                        ColSel::Field(field),
                    ) => Some((*table, *field)),
                    (Some(SrcKind::Virtual { table, cols }), ColSel::Column(i)) => cols
                        .get(usize::from(*i))
                        .copied()
                        .flatten()
                        .map(|field| (*table, field)),
                    _ => None,
                };
                if let Some((table, field)) = identity {
                    self.field(Occurrence::Direct(self.semantic_source(*src)), table, field)?;
                }
            }
            E::Deref { base, steps } => {
                let path = self.ir_path(base);
                self.derefs(path, steps)?;
            }
            E::InList {
                l, hierarchy: true, ..
            }
            | E::InQuery {
                l, hierarchy: true, ..
            } => {
                for e in l {
                    let path = self.ir_path(e);
                    self.hierarchy(path, &e.ty, false)?;
                }
            }
            E::LocalKey { orig, .. } | E::OuterKey { orig, .. } => self.ir_walk(stmt, orig)?,
            _ => {}
        }
        Ok(())
    }

    fn ir_walk(&mut self, stmt: &StmtS, e: &crate::ir::Expr) -> Result<(), ExecError> {
        let mut result = Ok(());
        super::visit_expr(e, false, &mut |e, _| {
            if result.is_ok() {
                result = self.ir_expr(stmt, e);
            }
        });
        result
    }

    fn present(
        &mut self,
        path: Path,
        ty: &crate::QType,
        unconstrained: bool,
        storage: &crate::StorageBinding,
    ) -> Result<(), ExecError> {
        for (_, atom) in storage.ref_atoms() {
            if !unconstrained && !ty.refs.contains(&atom) {
                continue;
            }
            let info = self.schema.ref_info(atom);
            if storage.table(info.table).is_none() {
                continue;
            }
            let key = Occurrence::Presentation(path.clone(), info.table);
            self.register(key.clone(), info.table, false)?;
            for &role in crate::lower::presentation_roles(info.kind) {
                let field = self
                    .schema
                    .table(info.table)
                    .fields
                    .iter()
                    .enumerate()
                    .find(|(_, f)| f.role == crate::schema::FieldRole::Std(role))
                    .and_then(|(i, _)| u16::try_from(i).ok())
                    .map(crate::schema::QFieldId);
                if field.is_some_and(|f| storage.field(info.table, f).is_some()) {
                    self.role(key.clone(), info.table, role)?;
                }
            }
        }
        Ok(())
    }

    fn ir_present(
        &mut self,
        stmt: &StmtS,
        storage: &crate::StorageBinding,
    ) -> Result<(), ExecError> {
        for &col in &stmt.present {
            let e = crate::lower::presentation_expr(stmt, col)
                .ok_or_else(|| ExecError::Fatal("missing presentation expression".into()))?;
            let path = self.ir_path(e);
            self.present(
                path,
                &e.ty,
                crate::lower::unconstrained_presentation(stmt, e),
                storage,
            )?;
        }
        Ok(())
    }

    fn hierarchy(&mut self, path: Path, ty: &crate::QType, folders: bool) -> Result<(), ExecError> {
        use crate::schema::{FieldRole, StdRole};
        for &atom in &ty.refs {
            if let Some(table) = self.schema.hierarchical_table(atom) {
                let key = Occurrence::HierarchyTarget(path.clone(), table);
                self.role(key.clone(), table, StdRole::Ref)?;
                self.role(key.clone(), table, StdRole::Parent)?;
                if folders
                    && self
                        .schema
                        .table(table)
                        .fields
                        .iter()
                        .any(|f| f.role == FieldRole::Std(StdRole::IsFolder))
                {
                    self.role(key, table, StdRole::IsFolder)?;
                }
            }
        }
        Ok(())
    }

    fn ir_totals(&mut self, stmt: &StmtS) -> Result<(), ExecError> {
        use crate::schema::{FieldRole, StdRole};
        if let Some(totals) = &stmt.totals {
            for (i, point) in totals.points.iter().enumerate() {
                if let Some(periods) = &point.periods {
                    for e in periods.begin.iter().chain(&periods.end) {
                        self.ir_walk(stmt, e)?;
                    }
                }
                for h in &point.hier {
                    let key = Occurrence::Hierarchy(i, h.table);
                    self.role(key.clone(), h.table, StdRole::Ref)?;
                    self.role(key.clone(), h.table, StdRole::Parent)?;
                    if self
                        .schema
                        .table(h.table)
                        .fields
                        .iter()
                        .any(|f| f.role == FieldRole::Std(StdRole::IsFolder))
                    {
                        self.role(key.clone(), h.table, StdRole::IsFolder)?;
                    }
                    for &(field, _) in h.keys.iter().flatten() {
                        self.field(key.clone(), h.table, field)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn finish(self, ctx: &BindCtx<'_>) -> Result<Discovered, ExecError> {
        let accesses: Vec<_> = self
            .uses
            .into_iter()
            .map(|u| (u.source, u.fields))
            .collect();
        let mut keys: Vec<_> = self.index.into_iter().collect();
        keys.sort_by_key(|(_, index)| *index);
        let decisions = check(
            &accesses,
            ctx,
            &keys.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(),
            self.temp_scan_seen,
            self.proof,
            &self.aliases,
        )?;
        Ok(Discovered {
            decisions: keys
                .into_iter()
                .zip(accesses)
                .zip(decisions)
                .map(|(((key, _), (source, _)), decision)| (key, source, decision))
                .collect(),
            aliases: self.aliases,
        })
    }
}

struct Discovered {
    decisions: Vec<(Occurrence, SourceTable, Access)>,
    aliases: std::collections::HashMap<SrcId, SrcId>,
}

fn drcr_views<'r>(rel: &'r Rel, out: &mut Vec<&'r crate::ir::RegisterVtS>) {
    match rel {
        Rel::RegisterVt(v) if v.kind == VtKind::DrCrTurnovers => out.push(v),
        Rel::Join { l, r, .. } => {
            drcr_views(l, out);
            drcr_views(r, out);
        }
        Rel::Subquery { input, .. }
        | Rel::Filter { input, .. }
        | Rel::Aggregate { input, .. }
        | Rel::Project { input, .. }
        | Rel::Distinct(input)
        | Rel::Sort { input, .. }
        | Rel::Top { input, .. }
        | Rel::Hierarchy { input, .. } => drcr_views(input, out),
        Rel::Union { parts, .. } => {
            for part in parts {
                drcr_views(part, out);
            }
        }
        Rel::Empty
        | Rel::Scan { .. }
        | Rel::TempScan { .. }
        | Rel::ParamScan { .. }
        | Rel::RegisterVt(_)
        | Rel::Slice(_)
        | Rel::RunningBalance(_) => {}
    }
}

fn drcr_ir_guards(
    stmt: &StmtS,
    schema: &QuerySchema,
    storage: &crate::StorageBinding,
    map: &ibschema::storage::StorageMap,
    aliases: &std::collections::HashMap<SrcId, SrcId>,
) -> Result<(), ExecError> {
    let mut views = Vec::new();
    drcr_views(&stmt.root, &mut views);
    for rows in stmt.nested.iter().filter_map(|n| n.rows.as_ref()) {
        drcr_views(&rows.scan, &mut views);
    }
    super::visit_stmt(stmt, &mut |e, _| match &e.kind {
        crate::ir::ExprKind::InQuery { q, .. } => drcr_views(q, &mut views),
        crate::ir::ExprKind::SectionAgg { rel, .. } => drcr_views(rel, &mut views),
        _ => {}
    });
    views.sort_by_key(|v| (v.span.map_or(u32::MAX, |s| s.start), v.src.0));
    views.dedup_by_key(|v| v.src);
    if views.is_empty() {
        return Ok(());
    }
    let mut used: std::collections::HashMap<SrcId, Vec<crate::QFieldId>> = Default::default();
    super::visit_stmt(stmt, &mut |e, _| {
        if let crate::ir::ExprKind::Col {
            src,
            col: crate::ir::ColSel::Field(field),
        } = &e.kind
        {
            let src = aliases.get(src).copied().unwrap_or(*src);
            let fields = used.entry(src).or_default();
            if !fields.contains(field) {
                fields.push(*field);
            }
        }
    });
    let rank = |what| match what {
        "Q6b-DrCrTurnoversStar" => 0,
        "Q6b-DrCrTurnoversPeriodFields" => 1,
        "Q6b-DrCrTurnoversDimensions" => 2,
        "Q6b-DrCrTurnoversNonBalancedResource" => 3,
        "Q6b-DrCrTurnoversColumns" => 4,
        _ => 5,
    };
    let mut form: Option<&'static str> = None;
    for view in &views {
        let all = stmt
            .sources
            .get(view.src.0 as usize)
            .is_some_and(|s| s.all_fields);
        if let Some(what) = crate::lower::drcr_inputs(schema, storage, map, view.table)
            .form_refusal(used.get(&view.src).map_or(&[], Vec::as_slice), all)
            && form.is_none_or(|old| rank(what) < rank(old))
        {
            form = Some(what);
        }
    }
    if let Some(what) = form {
        return Err(ExecError::Later { what, stage: "Q6b" });
    }
    let mut storage_later = None;
    for view in views {
        let resolver = crate::lower::drcr_inputs(schema, storage, map, view.table);
        let inputs = resolver.resolve()?;
        for (index, field) in schema.table(view.table).fields.iter().enumerate() {
            let field_id = crate::QFieldId(u16::try_from(index).map_err(|_| {
                ExecError::Fatal("accounting DrCrTurnovers: field index overflow".into())
            })?);
            if used.get(&view.src).is_some_and(|u| u.contains(&field_id))
                && let Some(md) = field.md
            {
                match resolver.resource(&inputs, md, field_id) {
                    Ok(_) => {}
                    Err(
                        e @ ExecError::Later {
                            what: "Q6b-DrCrTurnoversStorageShape",
                            ..
                        },
                    ) => {
                        if storage_later.is_none() {
                            storage_later = Some(e);
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
    match storage_later {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn turnovers_views<'r>(
    rel: &'r Rel,
    schema: &QuerySchema,
    out: &mut Vec<&'r crate::ir::RegisterVtS>,
) {
    match rel {
        Rel::RegisterVt(view) if crate::lower::is_accounting_turnovers(schema, view.table) => {
            out.push(view)
        }
        Rel::Join { l, r, .. } => {
            turnovers_views(l, schema, out);
            turnovers_views(r, schema, out);
        }
        Rel::Subquery { input, .. }
        | Rel::Filter { input, .. }
        | Rel::Aggregate { input, .. }
        | Rel::Project { input, .. }
        | Rel::Distinct(input)
        | Rel::Sort { input, .. }
        | Rel::Top { input, .. }
        | Rel::Hierarchy { input, .. } => turnovers_views(input, schema, out),
        Rel::Union { parts, .. } => {
            for part in parts {
                turnovers_views(part, schema, out);
            }
        }
        Rel::Empty
        | Rel::Scan { .. }
        | Rel::TempScan { .. }
        | Rel::ParamScan { .. }
        | Rel::RegisterVt(_)
        | Rel::Slice(_)
        | Rel::RunningBalance(_) => {}
    }
}

fn turnovers_ir_guards(
    stmt: &StmtS,
    schema: &QuerySchema,
    storage: &crate::StorageBinding,
    map: &ibschema::storage::StorageMap,
    aliases: &std::collections::HashMap<SrcId, SrcId>,
) -> Result<(), ExecError> {
    let mut views = Vec::new();
    turnovers_views(&stmt.root, schema, &mut views);
    for rows in stmt.nested.iter().filter_map(|nested| nested.rows.as_ref()) {
        turnovers_views(&rows.scan, schema, &mut views);
    }
    super::visit_stmt(stmt, &mut |expr, _| match &expr.kind {
        crate::ir::ExprKind::InQuery { q, .. } => turnovers_views(q, schema, &mut views),
        crate::ir::ExprKind::SectionAgg { rel, .. } => turnovers_views(rel, schema, &mut views),
        _ => {}
    });
    views.sort_by_key(|view| (view.span.map_or(u32::MAX, |span| span.start), view.src.0));
    views.dedup_by_key(|view| view.src);
    if views.is_empty() {
        return Ok(());
    }
    let mut used: std::collections::HashMap<SrcId, Vec<crate::QFieldId>> = Default::default();
    super::visit_stmt(stmt, &mut |expr, _| {
        if let crate::ir::ExprKind::Col {
            src,
            col: crate::ir::ColSel::Field(field),
        } = &expr.kind
        {
            let src = aliases.get(src).copied().unwrap_or(*src);
            let fields = used.entry(src).or_default();
            if !fields.contains(field) {
                fields.push(*field);
            }
        }
    });
    let rank = |what| match what {
        "Q6b-TurnoversStar" => 0,
        "Q6b-TurnoversPeriodFields" => 1,
        "Q6b-TurnoversDimensions" => 2,
        "Q6b-TurnoversNonBalancedResource" => 3,
        "Q6b-TurnoversColumns" => 4,
        _ => 5,
    };
    let mut refusal = None;
    for view in &views {
        let all = stmt
            .sources
            .get(view.src.0 as usize)
            .is_some_and(|source| source.all_fields);
        let resolver = crate::lower::turnovers_inputs(schema, storage, map, view.table);
        if let Some(what) =
            resolver.form_refusal(used.get(&view.src).map_or(&[], Vec::as_slice), all)
            && refusal.is_none_or(|old| rank(what) < rank(old))
        {
            refusal = Some(what);
        }
    }
    if let Some(what) = refusal {
        return Err(ExecError::Later { what, stage: "Q6b" });
    }
    for view in views {
        let resolver = crate::lower::turnovers_inputs(schema, storage, map, view.table);
        let inputs = resolver.resolve()?;
        for field in used.get(&view.src).into_iter().flatten() {
            if let Some(metadata) = schema.field(view.table, *field).and_then(|field| field.md) {
                resolver.resource(&inputs, metadata)?;
            }
        }
    }
    Ok(())
}

fn discover(
    stmt: &StmtS,
    ctx: &BindCtx<'_>,
    temps: Option<&dyn crate::bind::temp_provenance::TempOriginProvider>,
) -> Result<Discovered, ExecError> {
    let schema = ctx.schema;
    let storage = ctx.storage;
    let mut proof = provenance::Workspace::new();
    proof.set_temp_origins(temps, schema.version());
    let mut collector = query_syntax::with_deep_stack(stmt.max_depth, || {
        let mut c = Collector::new(schema);
        c.temp_origins = temps;
        if stmt.totals.is_some() || !stmt.nested.is_empty() {
            proof.disable();
        }
        proof.record_ir(&stmt.root, stmt, schema, false);
        c.virtual_aliases(&stmt.root, stmt);
        for rows in stmt.nested.iter().filter_map(|n| n.rows.as_ref()) {
            c.virtual_aliases(&rows.scan, stmt);
        }
        super::visit_stmt(stmt, &mut |e, _| match &e.kind {
            crate::ir::ExprKind::InQuery { q, hierarchy, .. } => {
                if *hierarchy {
                    proof.disable();
                }
                proof.record_ir(q, stmt, schema, true);
                c.virtual_aliases(q, stmt);
            }
            crate::ir::ExprKind::SectionAgg { rel, .. } => {
                proof.disable();
                c.virtual_aliases(rel, stmt);
            }
            crate::ir::ExprKind::InList {
                hierarchy: true, ..
            } => proof.disable(),
            _ => {}
        });
        c.proof = Some(&mut proof.proof);
        for s in &stmt.sources {
            if c.aliases.contains_key(&s.id) {
                continue;
            }
            if let SrcKind::Table(table) | SrcKind::Virtual { table, .. } = &s.kind {
                c.register(Occurrence::Direct(s.id), *table, s.all_fields)?;
            }
        }
        for (src, table) in c.record_bounds.clone() {
            c.role(
                Occurrence::Direct(src),
                table,
                crate::schema::StdRole::Period,
            )?;
            if schema.table(table).kind == TableKind::Virtual(VtKind::RecordsWithExtDimensions) {
                c.role(
                    Occurrence::Direct(src),
                    table,
                    crate::schema::StdRole::Recorder,
                )?;
            }
        }
        let mut result = Ok(());
        super::visit_stmt(stmt, &mut |e, _| {
            if result.is_ok() {
                result = c.ir_expr(stmt, e);
            }
        });
        result?;
        c.ir_totals(stmt)?;
        c.ir_present(stmt, storage)?;
        Ok::<_, ExecError>(c)
    })?;
    collector.accounting_period_params(ctx.params)?;
    collector.finish(ctx)
}

pub(in crate::bind) mod typed;
pub(super) use typed::preflight_virtual;

pub(in crate::bind) mod prepare;
pub(super) use prepare::prepare;
pub(crate) use prepare::{Security, reanchor};
