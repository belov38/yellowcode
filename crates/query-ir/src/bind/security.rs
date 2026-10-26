use super::BindCtx;
use crate::exec_error::{ExecError, RuntimeError};
use crate::host::{Access, FieldSet, SourceTable, TablePart};
use crate::ir::{Rel, SrcId, SrcKind, StmtS};
use crate::schema::{QTableId, QuerySchema, TableKind};
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Occurrence {
    Direct(crate::ir::SrcId),
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

struct Collector<'a> {
    schema: &'a QuerySchema,
    uses: Vec<Use>,
    index: std::collections::HashMap<Occurrence, usize>,
    opaque: u64,
    aliases: std::collections::HashMap<SrcId, SrcId>,
}

impl<'a> Collector<'a> {
    fn new(schema: &'a QuerySchema) -> Self {
        Self {
            schema,
            uses: Vec::new(),
            index: std::collections::HashMap::new(),
            opaque: 0,
            aliases: Default::default(),
        }
    }

    fn semantic_source(&self, src: SrcId) -> SrcId {
        self.aliases.get(&src).copied().unwrap_or(src)
    }

    fn virtual_aliases(&mut self, rel: &Rel) {
        match rel {
            Rel::RegisterVt(v) | Rel::Slice(v) | Rel::RunningBalance(v) => {
                self.aliases.insert(v.inner, v.src);
            }
            Rel::Subquery { input, .. }
            | Rel::Distinct(input)
            | Rel::Top { input, .. }
            | Rel::Filter { input, .. }
            | Rel::Aggregate { input, .. }
            | Rel::Project { input, .. }
            | Rel::Sort { input, .. }
            | Rel::Hierarchy { input, .. } => self.virtual_aliases(input),
            Rel::Join { l, r, .. } => {
                self.virtual_aliases(l);
                self.virtual_aliases(r);
            }
            Rel::Union { parts, .. } => {
                for part in parts {
                    self.virtual_aliases(part);
                }
            }
            Rel::Empty | Rel::Scan { .. } | Rel::TempScan { .. } | Rel::ParamScan { .. } => {}
        }
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
            self.index.insert(key, i);
            i
        };
        if let Some(used) = self.uses.get_mut(i) {
            used.fields.all |= all;
        }
        Ok(Some(i))
    }

    fn role(
        &mut self,
        key: Occurrence,
        table: QTableId,
        role: crate::schema::StdRole,
    ) -> Result<(), ExecError> {
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
        let md = f.md.filter(|_| f.nested.is_none());
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
            E::OuterKey { orig, .. } => return self.ir_path(orig),
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
            E::OuterKey { orig, .. } => self.ir_walk(stmt, orig)?,
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

    fn finish(
        self,
        ctx: &BindCtx<'_>,
    ) -> Result<Vec<(Occurrence, SourceTable, Access)>, ExecError> {
        let accesses: Vec<_> = self
            .uses
            .into_iter()
            .map(|u| (u.source, u.fields))
            .collect();
        let decisions = check(&accesses, ctx)?;
        let mut keys: Vec<_> = self.index.into_iter().collect();
        keys.sort_by_key(|(_, index)| *index);
        Ok(keys
            .into_iter()
            .zip(accesses)
            .zip(decisions)
            .map(|(((key, _), (source, _)), decision)| (key, source, decision))
            .collect())
    }
}

fn discover(
    stmt: &StmtS,
    ctx: &BindCtx<'_>,
) -> Result<Vec<(Occurrence, SourceTable, Access)>, ExecError> {
    let schema = ctx.schema;
    let storage = ctx.storage;
    let collector = query_syntax::with_deep_stack(stmt.max_depth, || {
        let mut c = Collector::new(schema);
        c.virtual_aliases(&stmt.root);
        for rows in stmt.nested.iter().filter_map(|n| n.rows.as_ref()) {
            c.virtual_aliases(&rows.scan);
        }
        super::visit_stmt(stmt, &mut |e, _| match &e.kind {
            crate::ir::ExprKind::InQuery { q, .. } => c.virtual_aliases(q),
            crate::ir::ExprKind::SectionAgg { rel, .. } => c.virtual_aliases(rel),
            _ => {}
        });
        for s in &stmt.sources {
            if c.aliases.contains_key(&s.id) {
                continue;
            }
            if let SrcKind::Table(table) | SrcKind::Virtual { table, .. } = &s.kind {
                c.register(Occurrence::Direct(s.id), *table, s.all_fields)?;
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
    collector.finish(ctx)
}

mod typed;
pub(super) use typed::preflight_virtual;

mod prepare;
pub(super) use prepare::prepare;
pub(crate) use prepare::{Security, reanchor};
