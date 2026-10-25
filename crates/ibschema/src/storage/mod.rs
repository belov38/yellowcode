mod info;

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use md_model::{MdKind, ScriptVariant};

use crate::columns::{self, Column, Origin};
use crate::error::{Error, Result};
use crate::logical::{LType, LogicalSchema, Table, TableKind, TypeCode};
use crate::names::{Side, StdField, role};
use crate::rules::Built;
pub use info::{StorageInfoField, StorageInfoIndex, StorageInfoTable};

use crate::values::Uuid16;

const SERVICE_PURPOSES: &str = include_str!("../../data/service_purposes.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TableId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FieldId(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexId {
    Id,
    Named(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    pub name: String,
    pub unique: bool,
    pub fields: Vec<FieldId>,
    pub prefix: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeparatorInfo {
    pub field: FieldId,
    pub common_attribute: Uuid16,
    pub use_field: Option<FieldId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInfo {
    pub name: String,
    pub md: Option<Uuid16>,
    pub std: Option<StdField>,
    pub types: Vec<LType>,
    pub nullable: bool,
    pub columns: Range<usize>,
    pub implicit: bool,
    pub md_name: String,
    pub side: Option<Side>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    pub logical: String,
    pub phys: String,
    pub owner: Option<TableId>,
    pub number: u32,
    pub purpose: String,
    pub md: Option<Uuid16>,
    pub fields: Vec<FieldInfo>,
    pub indexes: Vec<IndexInfo>,
    pub key_index: Option<IndexInfo>,
    pub columns: Vec<Column>,
    pub separators: Vec<SeparatorInfo>,
    pub query_name: String,
    pub md_name: String,
}

impl TableInfo {
    pub fn field(&self, id: FieldId) -> Option<&FieldInfo> {
        self.fields.get(usize::from(id.0))
    }

    pub fn field_by_name(&self, name: &str) -> Option<FieldId> {
        let i = self.fields.iter().position(|f| f.name == name)?;
        Some(FieldId(u16::try_from(i).ok()?))
    }

    pub fn field_by_std(&self, std: StdField) -> Option<FieldId> {
        let i = self.fields.iter().position(|f| f.std == Some(std))?;
        Some(FieldId(u16::try_from(i).ok()?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StdList {
    BaseCalculationTypes,
    LeadingCalculationTypes,
    DisplacingCalculationTypes,
    ExtDimensionTypes,
}

impl StdList {
    fn of_query_name(kind: MdKind, q: &str) -> Option<StdList> {
        let calc = kind == MdKind::ChartOfCalculationTypes;
        match q.rsplit('.').next()? {
            "BaseCalculationTypes" if calc => Some(StdList::BaseCalculationTypes),
            "LeadingCalculationTypes" if calc => Some(StdList::LeadingCalculationTypes),
            "DisplacingCalculationTypes" if calc => Some(StdList::DisplacingCalculationTypes),
            "ExtDimensionTypes" if kind == MdKind::ChartOfAccounts => {
                Some(StdList::ExtDimensionTypes)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectStorage {
    pub md: Uuid16,
    pub main: TableId,
    pub tabular: Vec<(Uuid16, TableId)>,
    pub lists: Vec<(StdList, TableId)>,
    pub service: Vec<(String, TableId)>,
}

impl ObjectStorage {
    pub fn list(&self, l: StdList) -> Option<TableId> {
        self.lists.iter().find(|(k, _)| *k == l).map(|(_, t)| *t)
    }
}

static NO_TABLE: TableInfo = TableInfo {
    logical: String::new(),
    phys: String::new(),
    owner: None,
    number: 0,
    purpose: String::new(),
    md: None,
    fields: Vec::new(),
    indexes: Vec::new(),
    key_index: None,
    columns: Vec::new(),
    separators: Vec::new(),
    query_name: String::new(),
    md_name: String::new(),
};

#[derive(Debug, Clone)]
pub struct StorageMap {
    schema: Arc<LogicalSchema>,
    script_variant: ScriptVariant,
    tables: Vec<TableInfo>,
    table_object: Vec<Option<Uuid16>>,
    by_name: HashMap<String, TableId>,
    by_ref_number: HashMap<u32, TableId>,
    objects: HashMap<Uuid16, ObjectStorage>,
    fields_by_md: HashMap<Uuid16, (TableId, FieldId)>,
    enums: HashMap<Uuid16, (TableId, u32)>,
}

fn bad(at: impl Into<String>, why: impl Into<String>) -> Error {
    Error::Parse {
        what: "DBSCHEMA",
        at: at.into(),
        why: why.into(),
    }
}

fn field_id(table: &str, i: usize) -> Result<FieldId> {
    u16::try_from(i)
        .map(FieldId)
        .map_err(|_| bad(table, "more than 65535 fields"))
}

fn table_id(i: usize) -> Result<TableId> {
    u32::try_from(i)
        .map(TableId)
        .map_err(|_| bad("DBSCHEMA", "too many tables"))
}

fn run_of(cols: &[Column], pred: impl Fn(&Origin) -> bool) -> Option<Range<usize>> {
    let start = cols.iter().position(|c| pred(&c.origin))?;
    let len = cols
        .iter()
        .skip(start)
        .take_while(|c| pred(&c.origin))
        .count();
    Some(start..start + len)
}

fn is_own_id(t: &Table) -> bool {
    t.fields.iter().any(|f| {
        f.name == "ID"
            && matches!(f.types.as_slice(), [ty] if ty.code == TypeCode::R && ty.ref_kind == 2)
    })
}

fn build_table(t: &Table, owner: Option<&Table>, owner_id: Option<TableId>) -> Result<TableInfo> {
    let logical = match owner {
        Some(o) => format!("{}.{}", o.name, t.name),
        None => t.name.clone(),
    };
    let columns = columns::of_table(t, owner)?;
    let missing = |f: &str| bad(logical.clone(), format!("no columns of field {f}"));
    let mut fields: Vec<FieldInfo> = Vec::with_capacity(t.fields.len() + 4);
    let mut separator_names: Vec<&str> = Vec::new();
    if let Some(o) = owner {
        let columns_of =
            run_of(&columns, |x| *x == Origin::OwnerId).ok_or_else(|| missing("ID"))?;
        fields.push(FieldInfo {
            name: "ID".into(),
            md: None,
            std: Some(StdField::Id),
            types: vec![LType::reference(o.name.clone(), 2)],
            nullable: false,
            columns: columns_of,
            implicit: true,
            md_name: String::new(),
            side: None,
        });
        for name in o.splitters[1].iter().flatten() {
            let f = o.field(name).ok_or_else(|| {
                bad(
                    format!("{}.splitters[1]", o.name),
                    format!("no field {name}"),
                )
            })?;
            let cols = run_of(
                &columns,
                |x| matches!(x, Origin::OwnerSplitter(s) if s == name),
            )
            .ok_or_else(|| missing(name))?;
            separator_names.push(name);
            fields.push(FieldInfo {
                name: name.clone(),
                md: None,
                std: StdField::of_name(name),
                types: f.types.clone(),
                nullable: f.nullable,
                columns: cols,
                implicit: true,
                md_name: String::new(),
                side: None,
            });
        }
        let cols =
            run_of(&columns, |x| *x == Origin::KeyField).ok_or_else(|| missing("KeyField"))?;
        fields.push(FieldInfo {
            name: "KeyField".into(),
            md: None,
            std: Some(StdField::KeyField),
            types: vec![LType::new(TypeCode::N, 10, 0)],
            nullable: false,
            columns: cols,
            implicit: true,
            md_name: String::new(),
            side: None,
        });
    } else {
        separator_names = t.splitters[0]
            .iter()
            .flatten()
            .map(String::as_str)
            .collect();
    }
    for (i, f) in t.fields.iter().enumerate() {
        let cols = run_of(&columns, |x| *x == Origin::Field(i)).ok_or_else(|| missing(&f.name))?;
        fields.push(FieldInfo {
            name: f.name.clone(),
            md: None,
            std: StdField::of_name(&f.name),
            types: f.types.clone(),
            nullable: f.nullable,
            columns: cols,
            implicit: false,
            md_name: String::new(),
            side: None,
        });
    }
    let find = |name: &str, place: &str| -> Result<FieldId> {
        let i = fields.iter().position(|f| f.name == name).ok_or_else(|| {
            bad(
                format!("{logical}.{place}"),
                format!("no field {name} in the table"),
            )
        })?;
        field_id(&logical, i)
    };
    let prefix = separator_names
        .iter()
        .map(|n| find(n, "splitters"))
        .collect::<Result<Vec<_>>>()?;
    let mut indexes = Vec::with_capacity(t.indexes.len());
    for ix in &t.indexes {
        let mut ids = prefix.clone();
        for n in &ix.fields {
            ids.push(find(n, &ix.name)?);
        }
        indexes.push(IndexInfo {
            name: ix.name.clone(),
            unique: ix.unique,
            fields: ids,
            prefix: prefix.len(),
        });
    }
    let key_index = if owner.is_some() {
        let mut ids = prefix.clone();
        ids.push(find("ID", "SK")?);
        ids.push(find("KeyField", "SK")?);
        Some(IndexInfo {
            name: "SK".into(),
            unique: true,
            fields: ids,
            prefix: prefix.len(),
        })
    } else if is_own_id(t) {
        let mut ids = prefix.clone();
        ids.push(find("ID", "ID")?);
        Some(IndexInfo {
            name: "ID".into(),
            unique: true,
            fields: ids,
            prefix: prefix.len(),
        })
    } else {
        None
    };
    let separators = prefix
        .iter()
        .map(|&field| SeparatorInfo {
            field,
            common_attribute: Uuid16::NIL,
            use_field: None,
        })
        .collect();
    Ok(TableInfo {
        phys: columns::phys_table_name(t, owner),
        owner: owner_id,
        number: t.number,
        purpose: String::new(),
        md: None,
        fields,
        indexes,
        key_index,
        columns,
        separators,
        query_name: String::new(),
        md_name: String::new(),
        logical,
    })
}

fn service_purposes() -> Result<HashMap<String, String>> {
    serde_json::from_str(SERVICE_PURPOSES).map_err(|e| Error::Parse {
        what: "service_purposes.json",
        at: String::new(),
        why: e.to_string(),
    })
}

impl StorageMap {
    pub fn from_schema(schema: Arc<LogicalSchema>) -> Result<StorageMap> {
        let mut tables: Vec<TableInfo> = Vec::new();
        let mut by_name = HashMap::new();
        let mut owners: HashMap<&str, TableId> = HashMap::new();
        for (t, owner) in schema.walk() {
            let id = table_id(tables.len())?;
            let owner_id = match owner {
                Some(o) => Some(*owners.get(o.name.as_str()).ok_or_else(|| {
                    bad(
                        &t.name,
                        format!("owner {} is not a top-level table", o.name),
                    )
                })?),
                None => {
                    owners.insert(t.name.as_str(), id);
                    None
                }
            };
            let info = build_table(t, owner, owner_id)?;
            if by_name.insert(info.logical.clone(), id).is_some() {
                return Err(bad(info.logical, "duplicate table"));
            }
            tables.push(info);
        }
        let mut by_ref_number = HashMap::new();
        for (i, (t, owner)) in schema.walk().into_iter().enumerate() {
            let reference =
                owner.is_none() && t.kind == TableKind::Main && is_own_id(t) && t.number != 0;
            if reference && by_ref_number.insert(t.number, table_id(i)?).is_some() {
                return Err(bad(
                    t.name.clone(),
                    format!("reference number {} is used by two tables", t.number),
                ));
            }
        }
        Ok(StorageMap {
            schema,
            script_variant: ScriptVariant::English,
            table_object: vec![None; tables.len()],
            tables,
            by_name,
            by_ref_number,
            objects: HashMap::new(),
            fields_by_md: HashMap::new(),
            enums: HashMap::new(),
        })
    }

    pub fn new(schema: Arc<LogicalSchema>, built: &Built) -> Result<StorageMap> {
        let mut m = StorageMap::from_schema(schema)?;
        m.script_variant = built.script_variant;
        m.service_tables(built)?;
        m.objects_of(built, None)?;
        m.nested_splitters();
        m.separators_of(built);
        Ok(m)
    }

    pub fn new_lenient(
        schema: Arc<LogicalSchema>,
        built: &Built,
    ) -> Result<(StorageMap, Vec<String>)> {
        let mut m = StorageMap::from_schema(schema)?;
        m.script_variant = built.script_variant;
        let mut notes = Vec::new();
        m.service_tables(built)?;
        m.objects_of(built, Some(&mut notes))?;
        m.nested_splitters();
        m.separators_of(built);
        Ok((m, notes))
    }

    fn service_tables(&mut self, built: &Built) -> Result<()> {
        let purposes = service_purposes()?;
        for t in &mut self.tables {
            if let Some(p) = purposes.get(&t.logical) {
                t.purpose.clone_from(p);
            } else if let Some(p) =
                purposes.get(t.logical.trim_end_matches(|c: char| c.is_ascii_digit()))
            {
                t.purpose.clone_from(p);
            }
            for f in &mut t.fields {
                let spec = built
                    .separators
                    .iter()
                    .find(|s| s.field == f.name || s.use_field.as_deref() == Some(f.name.as_str()));
                if let Some(s) = spec {
                    f.md_name.clone_from(&s.md_name);
                }
            }
        }
        Ok(())
    }

    fn objects_of(&mut self, built: &Built, mut notes: Option<&mut Vec<String>>) -> Result<()> {
        for o in &built.objects {
            let mut tabular = Vec::new();
            let mut lists = Vec::new();
            let mut service = Vec::new();
            let mut main = None;
            for (k, r) in o.tables.iter().enumerate() {
                let Some(&id) = self.by_name.get(&r.logical) else {
                    let why = format!("the table of {} is not in the schema", r.md_name);
                    match notes.as_deref_mut() {
                        Some(n) => {
                            n.push(format!("{}: {why}", r.logical));
                            continue;
                        }
                        None => return Err(bad(r.logical.clone(), why)),
                    }
                };
                let slot = usize::try_from(id.0).ok();
                let t = slot
                    .and_then(|s| self.tables.get_mut(s))
                    .ok_or_else(|| bad(r.logical.clone(), "table id out of range"))?;
                t.purpose.clone_from(&r.purpose);
                t.md = Some(r.md);
                t.md_name.clone_from(&r.md_name);
                t.query_name.clone_from(&r.query_name);
                for fr in &r.fields {
                    let i = t
                        .fields
                        .iter()
                        .position(|f| f.name == fr.field && !f.implicit);
                    let Some(f) = i.and_then(|i| t.fields.get_mut(i)) else {
                        let at = format!("{}.{}", r.logical, fr.field);
                        let why = format!("the field of {} is not in the schema", r.md_name);
                        match notes.as_deref_mut() {
                            Some(n) => {
                                n.push(format!("{at}: {why}"));
                                continue;
                            }
                            None => return Err(bad(at, why)),
                        }
                    };
                    f.md = fr.md;
                    f.std = fr.std;
                    f.md_name.clone_from(&fr.md_name);
                    f.side = fr.side;
                    if let Some(md) = fr.md
                        && !fr.md_name.starts_with("CommonAttribute.")
                        && let Some(i) = i
                    {
                        self.fields_by_md
                            .entry(md)
                            .or_insert((id, field_id(&r.logical, i)?));
                    }
                }
                if let Some(s) = slot
                    && let Some(x) = self.table_object.get_mut(s)
                {
                    *x = Some(o.md);
                }
                if k == 0 {
                    main = Some(id);
                } else if r.purpose == role::TABULAR_SECTION {
                    tabular.push((r.md, id));
                    if let Some(l) = StdList::of_query_name(o.kind, &r.query_name) {
                        lists.push((l, id));
                    }
                } else {
                    service.push((r.purpose.clone(), id));
                }
            }
            let Some(main) = main else { continue };
            for (i, v) in o.enum_values.iter().enumerate() {
                let n =
                    u32::try_from(i).map_err(|_| bad(o.full_name.clone(), "too many values"))?;
                self.enums.insert(*v, (main, n));
            }
            self.objects.insert(
                o.md,
                ObjectStorage {
                    md: o.md,
                    main,
                    tabular,
                    lists,
                    service,
                },
            );
        }
        Ok(())
    }

    fn nested_splitters(&mut self) {
        let mut updates = Vec::new();
        for (i, t) in self.tables.iter().enumerate() {
            let Some(owner) = t.owner else { continue };
            let o = self.table(owner);
            for (j, f) in t.fields.iter().enumerate().filter(|(_, f)| f.implicit) {
                if let Some(of) = o.fields.iter().find(|x| x.name == f.name && !x.implicit) {
                    updates.push((i, j, of.md_name.clone()));
                }
            }
        }
        for (i, j, name) in updates {
            if let Some(f) = self.tables.get_mut(i).and_then(|t| t.fields.get_mut(j)) {
                f.md_name = name;
            }
        }
    }

    fn separators_of(&mut self, built: &Built) {
        for i in 0..self.tables.len() {
            let Some(t) = self.tables.get(i) else {
                continue;
            };
            let owner = t
                .owner
                .and_then(|o| self.tables.get(usize::try_from(o.0).ok()?));
            let seps: Vec<SeparatorInfo> = t
                .separators
                .iter()
                .map(|s| {
                    let f = t.field(s.field);
                    let name = f.map(|f| f.name.as_str()).unwrap_or_default();
                    let spec = built.separators.iter().find(|x| x.field == name);
                    let md = f
                        .and_then(|f| f.md)
                        .or_else(|| owner.and_then(|o| o.field(o.field_by_name(name)?)?.md))
                        .or(spec.map(|x| x.md))
                        .unwrap_or(Uuid16::NIL);
                    let use_field = spec
                        .and_then(|x| x.use_field.as_deref())
                        .and_then(|u| t.field_by_name(u));
                    SeparatorInfo {
                        field: s.field,
                        common_attribute: md,
                        use_field,
                    }
                })
                .collect();
            if let Some(t) = self.tables.get_mut(i) {
                t.separators = seps;
            }
        }
    }

    pub fn schema(&self) -> &LogicalSchema {
        &self.schema
    }

    pub fn tables(&self) -> &[TableInfo] {
        &self.tables
    }

    pub fn table(&self, t: TableId) -> &TableInfo {
        self.try_table(t).unwrap_or(&NO_TABLE)
    }

    pub fn try_table(&self, t: TableId) -> Option<&TableInfo> {
        usize::try_from(t.0).ok().and_then(|i| self.tables.get(i))
    }

    pub fn table_by_name(&self, logical: &str) -> Option<TableId> {
        self.by_name.get(logical).copied()
    }

    pub fn object(&self, md: Uuid16) -> Option<&ObjectStorage> {
        self.objects.get(&md)
    }

    pub fn field(&self, md: Uuid16) -> Option<(TableId, FieldId)> {
        self.fields_by_md.get(&md).copied()
    }

    pub fn ref_table(&self, number: u32) -> Option<TableId> {
        self.by_ref_number.get(&number).copied()
    }

    pub fn ref_number(&self, t: TableId) -> Option<u32> {
        let info = self.tables.get(usize::try_from(t.0).ok()?)?;
        let n = info.number;
        (self.by_ref_number.get(&n) == Some(&t)).then_some(n)
    }

    pub fn enum_value(&self, value: Uuid16) -> Option<(TableId, u32)> {
        self.enums.get(&value).copied()
    }

    pub fn storage_info(
        &self,
        objects: Option<&[Uuid16]>,
        db_names: bool,
    ) -> Result<Vec<StorageInfoTable>> {
        info::storage_info(self, objects, db_names)
    }

    pub(crate) fn script_variant(&self) -> ScriptVariant {
        self.script_variant
    }

    pub(crate) fn table_object(&self, t: usize) -> Option<Uuid16> {
        self.table_object.get(t).copied().flatten()
    }
}
