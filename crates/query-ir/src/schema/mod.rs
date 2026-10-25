pub mod atoms;
mod bind;
mod managed;
mod misc;
pub mod names;
mod objects;
mod registers;
pub mod templates;
mod typemap;

use crate::types::{NumQ, Other, QType, RefAtom, StrQ};
pub use bind::{StorageBinding, StorageLookup, TabularKey};
use brace::Guid;
use bsl_prim::fold_name;
use mdschema::MdKind;
use query_syntax::{Kw, ScriptVariant};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use typemap::{RefIndex, SysEnums, SysTypes};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QTableId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QFieldId(pub u16);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RefInfo {
    pub object: Guid,
    pub kind: MdKind,
    pub name: String,
    pub type_uuid: Guid,
    pub xml_name: String,
    pub table: QTableId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct QTable {
    pub name_ru: String,
    pub name_en: String,
    pub kind: TableKind,
    pub object: Option<Guid>,
    pub owner: Option<QTableId>,
    pub fields: Vec<QField>,
    pub star: Vec<QFieldId>,
    pub params: Vec<VtParam>,
    pub reference: Option<RefAtom>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct QField {
    pub name_ru: String,
    pub name_en: String,
    pub ty: QType,
    pub role: FieldRole,
    pub only: Only,
    pub nested: Option<QTableId>,
    pub md: Option<Guid>,
    pub side: Option<Side>,
    pub(crate) period: Option<PeriodField>,
}

impl QField {
    pub fn period(&self) -> Option<PeriodField> {
        self.period
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Side {
    Dr,
    Cr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Periodicity {
    Absent,
    Auto,
    Record,
    Recorder,
    Unit(Kw),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Complement {
    Movements,
    MovementsAndBoundaries,
}

impl Complement {
    pub fn from_word(w: &str) -> Option<Complement> {
        let lw: String = bsl_prim::lowercase(w).collect();
        match lw.as_str() {
            "движения" | "registerrecords" => Some(Complement::Movements),
            "движенияиграницыпериода" | "registerrecordsandperiodboundaries" => {
                Some(Complement::MovementsAndBoundaries)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PeriodField {
    Period,
    Unit(Kw),
    Recorder,
    LineNumber,
}

impl PeriodField {
    pub fn selected_by(self, p: Periodicity) -> bool {
        match p {
            Periodicity::Absent => false,
            Periodicity::Auto => !matches!(self, PeriodField::Period),
            Periodicity::Record => !matches!(self, PeriodField::Unit(_)),
            Periodicity::Recorder => matches!(self, PeriodField::Period | PeriodField::Recorder),
            Periodicity::Unit(_) => matches!(self, PeriodField::Period),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TableKind {
    Object(MdKind),
    Tabular,
    Virtual(VtKind),
    Changes,
    Constants,
    Recalculation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VtKind {
    Balance,
    Turnovers,
    BalanceAndTurnovers,
    SliceLast,
    SliceFirst,
    ExtDimensions,
    RecordsWithExtDimensions,
    DrCrTurnovers,
    ActualActionPeriod,
    ScheduleData,
    Base,
    SequenceBoundaries,
    TaskByPerformer,
    FilterCriterion,
    BusinessProcessPoints,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldRole {
    Std(StdRole),
    Attribute,
    Dimension,
    Resource,
    Tabular,
    Common,
    Virtual,
    Graph,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StdRole {
    Ref,
    DataVersion,
    DeletionMark,
    Owner,
    Parent,
    IsFolder,
    Code,
    Description,
    Predefined,
    PredefinedDataName,
    Presentation,
    Number,
    Date,
    Posted,
    PointInTime,
    Order,
    Type,
    LineNumber,
    Period,
    Recorder,
    Active,
    RecordKind,
    Account,
    ExtDimension(u8),
    ExtDimensionType(u8),
    ValueType,
    OffBalance,
    ExtDimensionTypes,
    TurnoversOnly,
    ActionPeriodIsBasic,
    LeadingCalculationTypes,
    BaseCalculationTypes,
    DisplacingCalculationTypes,
    CalculationType,
    RecalculationObject,
    SentNo,
    ReceivedNo,
    ExchangeDate,
    ThisNode,
    HeadTask,
    Started,
    Completed,
    BusinessProcess,
    RoutePoint,
    Executed,
    AccountDr,
    AccountCr,
    BalancedAccount,
    ExtDimensionDr(u8),
    ExtDimensionCr(u8),
    BalancedExtDimension(u8),
    ExtDimensionTypeDr(u8),
    ExtDimensionTypeCr(u8),
    ExtDimensionKind,
    PeriodAdjustment,
    RegistrationPeriod,
    ActionPeriod,
    BegOfActionPeriod,
    EndOfActionPeriod,
    BegOfBasePeriod,
    EndOfBasePeriod,
    ReversingEntry,
    Node,
    MessageNo,
    Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Only {
    Any,
    Folders,
    Items,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VtParam {
    pub name_ru: String,
    pub name_en: String,
    pub kind: VtParamKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VtParamKind {
    Period,
    Condition,
    Periodicity,
    AccountCondition,
    ExtDimensions,
    Order,
    Top,
    ComplementMethod,
    DimensionList,
    Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValuePath {
    Enum { table: QTableId, value: Guid },
    Predefined { table: QTableId, name: String },
    EmptyRef(QTableId),
    SysEnum(platform_catalog::TypeIdx, u32),
    BusinessProcessPoint { table: QTableId, name: String },
    TypeRef(QTableId),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaError {
    Config(String),
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaError::Config(s) => write!(f, "query schema: {s}"),
        }
    }
}

impl std::error::Error for SchemaError {}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Item {
    pub id: Guid,
    pub name: String,
    pub ty: QType,
    pub balance: bool,
    pub main_filter: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ObjMeta {
    pub kind: MdKind,
    pub dims: Vec<Item>,
    pub resources: Vec<Item>,
    pub attrs: Vec<Item>,
    pub commons: Vec<String>,
    pub balance_type: bool,
    pub periodic: bool,
    pub subordinate: bool,
    pub filter_on_period: bool,
    pub corr: bool,
    pub ext_dims: u8,
    pub adjustment: bool,
    pub action_period: bool,
    pub base_period: bool,
    pub schedule_resource: Option<Item>,
}

impl ObjMeta {
    pub fn new(kind: MdKind) -> ObjMeta {
        ObjMeta {
            kind,
            dims: Vec::new(),
            resources: Vec::new(),
            attrs: Vec::new(),
            commons: Vec::new(),
            balance_type: false,
            periodic: false,
            subordinate: false,
            filter_on_period: false,
            corr: false,
            ext_dims: 0,
            adjustment: false,
            action_period: false,
            base_period: false,
            schedule_resource: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct ValueIndex {
    pub enums: HashMap<QTableId, HashMap<String, Guid>>,
    pub predefined: HashMap<QTableId, HashMap<String, String>>,
    pub points: HashMap<QTableId, QTableId>,
    pub unstored: std::collections::HashSet<QTableId>,
}

pub struct QuerySchema {
    version: u64,
    script: ScriptVariant,
    tables: Vec<QTable>,
    by_name: HashMap<String, QTableId>,
    by_object: HashMap<Guid, QTableId>,
    refs: Vec<RefInfo>,
    ref_by_qname: HashMap<String, RefAtom>,
    field_index: Vec<HashMap<String, QFieldId>>,
    sys: SysTypes,
    sys_enums: SysEnums,
    route_points: HashMap<RefAtom, Guid>,
    values: ValueIndex,
    meta: HashMap<QTableId, ObjMeta>,
    diagnostics: Vec<String>,
}

static EMPTY_TABLE: QTable = QTable {
    name_ru: String::new(),
    name_en: String::new(),
    kind: TableKind::Tabular,
    object: None,
    owner: None,
    fields: Vec::new(),
    star: Vec::new(),
    params: Vec::new(),
    reference: None,
};

static EMPTY_REF: RefInfo = RefInfo {
    object: Guid::NIL,
    kind: MdKind::Catalog,
    name: String::new(),
    type_uuid: Guid::NIL,
    xml_name: String::new(),
    table: QTableId(u32::MAX),
};

pub(crate) const REF_KINDS: [MdKind; 9] = [
    MdKind::Catalog,
    MdKind::Document,
    MdKind::Enum,
    MdKind::ChartOfCharacteristicTypes,
    MdKind::ChartOfAccounts,
    MdKind::ChartOfCalculationTypes,
    MdKind::ExchangePlan,
    MdKind::BusinessProcess,
    MdKind::Task,
];

const PREDEFINED_KINDS: [MdKind; 4] = [
    MdKind::Catalog,
    MdKind::ChartOfCharacteristicTypes,
    MdKind::ChartOfAccounts,
    MdKind::ChartOfCalculationTypes,
];

const EMPTY_REF_NAMES: [&str; 2] = ["ПустаяСсылка", "EmptyRef"];
const ROUTE_POINT_NAMES: [&str; 2] = ["ТочкаМаршрута", "RoutePoint"];

impl QuerySchema {
    pub fn is_unstored(&self, t: QTableId) -> bool {
        self.values.unstored.contains(&t)
    }

    pub fn build(
        cfg: &md_model::Configuration,
        cat: &platform_catalog::Catalog,
    ) -> Result<Arc<QuerySchema>, SchemaError> {
        let mut b = Builder::new(cfg, cat);
        let commons = objects::build(&mut b)?;
        registers::build(&mut b, &commons)?;
        misc::build(&mut b, &commons)?;
        b.finish().map(Arc::new)
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn script(&self) -> ScriptVariant {
        self.script
    }

    pub fn tables(&self) -> &[QTable] {
        &self.tables
    }

    pub fn table(&self, t: QTableId) -> &QTable {
        usize::try_from(t.0)
            .ok()
            .and_then(|i| self.tables.get(i))
            .unwrap_or(&EMPTY_TABLE)
    }

    pub fn find_table(&self, parts: &[&str]) -> Option<QTableId> {
        self.by_name.get(&fold_name(&parts.join("."))).copied()
    }

    pub fn find_field(&self, t: QTableId, name: &str) -> Option<QFieldId> {
        self.find_field_folded(t, &fold_name(name))
    }

    pub fn find_field_folded(&self, t: QTableId, key: &str) -> Option<QFieldId> {
        let ix = self.field_index.get(usize::try_from(t.0).ok()?)?;
        ix.get(key).copied()
    }

    pub fn field(&self, t: QTableId, f: QFieldId) -> Option<&QField> {
        self.table(t).fields.get(usize::from(f.0))
    }

    pub fn object_table(&self, md: Guid) -> Option<QTableId> {
        self.by_object.get(&md).copied()
    }

    pub fn refs(&self) -> &[RefInfo] {
        &self.refs
    }

    pub fn ref_info(&self, r: RefAtom) -> &RefInfo {
        usize::try_from(r.0)
            .ok()
            .and_then(|i| self.refs.get(i))
            .unwrap_or(&EMPTY_REF)
    }

    pub fn hierarchical_table(&self, r: RefAtom) -> Option<QTableId> {
        let t = self.ref_info(r).table;
        self.table(t)
            .fields
            .iter()
            .any(|f| f.role == FieldRole::Std(StdRole::Parent))
            .then_some(t)
    }

    pub fn ref_by_name(&self, qname: &str) -> Option<RefAtom> {
        self.ref_by_qname.get(&fold_name(qname)).copied()
    }

    pub fn type_by_name(&self, parts: &[&str]) -> Option<QType> {
        if let [one] = parts
            && let Some(t) = self.sys.by_name(one)
        {
            return Some(t);
        }
        if let [one] = parts
            && let Some(idx) = self.sys_enums.find(one)
        {
            return Some(QType::other(Other::SysEnum(idx)));
        }
        let t = self.table(self.find_table(parts)?);
        match t.kind {
            TableKind::Object(_) => t.reference.map(QType::reference),
            TableKind::Virtual(VtKind::BusinessProcessPoints) => {
                let bp = self.table(t.owner?).reference?;
                Some(QType::other(Other::RoutePoint(bp)))
            }
            _ => None,
        }
    }

    pub fn atom_uuid(&self, t: &QType) -> Vec<(Guid, QType)> {
        let s = &self.sys;
        let mut out = Vec::with_capacity(t.atom_count() + 1);
        if t.null {
            out.push((s.null, QType::null_only()));
        }
        if t.boolean {
            out.push((s.boolean, QType::boolean()));
        }
        if let Some(q) = t.number {
            out.push((s.number, QType::num(q)));
        }
        if let Some(q) = t.string {
            out.push((s.string, QType::str_(q)));
        }
        if let Some(q) = t.date {
            out.push((s.date, QType::date(q)));
        }
        for r in &t.refs {
            out.push((self.ref_info(*r).type_uuid, QType::reference(*r)));
        }
        for o in &t.others {
            out.push((self.other_uuid(*o), QType::other(*o)));
        }
        out.sort_by_key(|(g, _)| *g);
        out
    }

    fn other_uuid(&self, o: Other) -> Guid {
        let s = &self.sys;
        match o {
            Other::Uuid => s.uuid,
            Other::Binary => s.binary,
            Other::Storage => s.storage,
            Other::Type => s.type_,
            Other::TypeDescription => s.type_description,
            Other::PointInTime => s.point_in_time,
            Other::Boundary => s.boundary,
            Other::QueryResult => s.query_result,
            Other::ValueTable => s.value_table,
            Other::RoutePoint(r) => self.route_points.get(&r).copied().unwrap_or(Guid::NIL),
            Other::SysEnum(idx) => self.sys_enums.guid(idx),
        }
    }

    pub fn value_path(&self, parts: &[&str]) -> ValuePath {
        match parts {
            [ty, value] => match self.sys_enums.value(ty, value) {
                Some((t, v)) => ValuePath::SysEnum(t, v),
                None => match self.find_table(parts) {
                    Some(id)
                        if matches!(self.table(id).kind, TableKind::Object(_))
                            && self.table(id).reference.is_some() =>
                    {
                        ValuePath::TypeRef(id)
                    }
                    _ => ValuePath::Unknown,
                },
            },
            [_, _, value] => {
                let Some(id) = self.find_table(parts.get(..2).unwrap_or(&[])) else {
                    return ValuePath::Unknown;
                };
                let t = self.table(id);
                let (TableKind::Object(kind), Some(_)) = (t.kind, t.reference) else {
                    return ValuePath::Unknown;
                };
                let v = fold_name(value);
                if EMPTY_REF_NAMES.iter().any(|n| fold_name(n) == v) {
                    return ValuePath::EmptyRef(id);
                }
                if kind == MdKind::Enum {
                    return match self.values.enums.get(&id).and_then(|m| m.get(&v)) {
                        Some(g) => ValuePath::Enum {
                            table: id,
                            value: *g,
                        },
                        None => ValuePath::Unknown,
                    };
                }
                if PREDEFINED_KINDS.contains(&kind)
                    && let Some(name) = self.values.predefined.get(&id).and_then(|m| m.get(&v))
                {
                    return ValuePath::Predefined {
                        table: id,
                        name: name.clone(),
                    };
                }
                ValuePath::Unknown
            }
            [_, _, point, name] => {
                let p = fold_name(point);
                let points = self
                    .find_table(parts.get(..2).unwrap_or(&[]))
                    .and_then(|bp| self.values.points.get(&bp));
                match points {
                    Some(t) if ROUTE_POINT_NAMES.iter().any(|n| fold_name(n) == p) => {
                        ValuePath::BusinessProcessPoint {
                            table: *t,
                            name: (*name).to_string(),
                        }
                    }
                    _ => ValuePath::Unknown,
                }
            }
            _ => ValuePath::Unknown,
        }
    }

    pub fn value_type(&self, v: &ValuePath) -> Option<QType> {
        match v {
            ValuePath::Enum { table, .. }
            | ValuePath::Predefined { table, .. }
            | ValuePath::EmptyRef(table) => self.table(*table).reference.map(QType::reference),
            ValuePath::SysEnum(idx, _) => Some(QType::other(Other::SysEnum(*idx))),
            ValuePath::TypeRef(_) => Some(QType::other(Other::Type)),
            ValuePath::BusinessProcessPoint { table, .. } => {
                let bp = self.table(self.table(*table).owner?).reference?;
                Some(QType::other(Other::RoutePoint(bp)))
            }
            ValuePath::Unknown => None,
        }
    }

    pub fn vt_fields(&self, t: QTableId, p: Periodicity) -> Vec<QFieldId> {
        self.vt_fields_iter(t, p).collect()
    }

    pub(crate) fn vt_fields_iter(
        &self,
        t: QTableId,
        p: Periodicity,
    ) -> impl Iterator<Item = QFieldId> + '_ {
        let tab = self.table(t);
        tab.star.iter().copied().filter(move |f| {
            tab.fields
                .get(usize::from(f.0))
                .and_then(|x| x.period)
                .is_none_or(|pf| pf.selected_by(p))
        })
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    pub(crate) fn meta(&self, t: QTableId) -> Option<&ObjMeta> {
        self.meta.get(&t)
    }

    pub(crate) fn root(&self, t: QTableId) -> QTableId {
        let mut cur = t;
        for _ in 0..8 {
            match self.table(cur).owner {
                Some(o) => cur = o,
                None => break,
            }
        }
        cur
    }
}

pub(crate) struct Builder<'a> {
    pub cfg: &'a md_model::Configuration,
    pub cat: &'a platform_catalog::Catalog,
    pub tables: Vec<QTable>,
    pub aliases: Vec<(String, QTableId)>,
    pub by_object: HashMap<Guid, QTableId>,
    pub by_full: HashMap<String, QTableId>,
    pub meta: HashMap<QTableId, ObjMeta>,
    pub refs: RefIndex,
    pub sys: SysTypes,
    pub sys_enums: SysEnums,
    pub values: ValueIndex,
    pub diagnostics: Vec<String>,
}

impl<'a> Builder<'a> {
    fn new(cfg: &'a md_model::Configuration, cat: &'a platform_catalog::Catalog) -> Builder<'a> {
        let mut diagnostics = Vec::new();
        let sys = SysTypes::new(cat, &mut diagnostics);
        Builder {
            cfg,
            cat,
            tables: Vec::new(),
            aliases: Vec::new(),
            by_object: HashMap::new(),
            by_full: HashMap::new(),
            meta: HashMap::new(),
            refs: RefIndex::default(),
            sys,
            sys_enums: SysEnums::new(cat),
            values: ValueIndex::default(),
            diagnostics,
        }
    }

    pub fn add_table(&mut self, t: QTable, extra: &[String]) -> Result<QTableId, SchemaError> {
        let id = QTableId(
            u32::try_from(self.tables.len())
                .map_err(|_| SchemaError::Config("too many tables".into()))?,
        );
        self.aliases.push((t.name_ru.clone(), id));
        self.aliases.push((t.name_en.clone(), id));
        for e in extra {
            self.aliases.push((e.clone(), id));
        }
        self.tables.push(t);
        Ok(id)
    }

    pub fn table_mut(&mut self, id: QTableId) -> Option<&mut QTable> {
        self.tables.get_mut(usize::try_from(id.0).ok()?)
    }

    pub fn table_ref(&self, id: QTableId) -> Option<&QTable> {
        self.tables.get(usize::try_from(id.0).ok()?)
    }

    pub fn add_object(&mut self, kind: MdKind, name: &str, id: Guid, t: QTableId) {
        self.by_object.insert(id, t);
        self.by_full
            .insert(fold_name(&format!("{}.{name}", kind.as_str())), t);
    }

    pub fn diag(&mut self, s: String) {
        self.diagnostics.push(s);
    }

    fn finish(self) -> Result<QuerySchema, SchemaError> {
        let mut by_name = HashMap::with_capacity(self.aliases.len());
        for (n, id) in self.aliases {
            by_name.entry(fold_name(&n)).or_insert(id);
        }
        let mut field_index = Vec::with_capacity(self.tables.len());
        for t in &self.tables {
            let mut ix = HashMap::with_capacity(t.fields.len() * 2);
            for (i, f) in t.fields.iter().enumerate() {
                let id = QFieldId(u16::try_from(i).map_err(|_| {
                    SchemaError::Config(format!("too many fields in {}", t.name_en))
                })?);
                ix.entry(fold_name(&f.name_ru)).or_insert(id);
                ix.entry(fold_name(&f.name_en)).or_insert(id);
            }
            field_index.push(ix);
        }
        let script = match self.cfg.settings.script_variant {
            md_model::ScriptVariant::Russian => ScriptVariant::Russian,
            md_model::ScriptVariant::English => ScriptVariant::English,
        };
        let RefIndex {
            refs,
            by_qname: ref_by_qname,
            rp_uuid: route_points,
            ..
        } = self.refs;
        let version = version(
            script,
            &self.tables,
            &refs,
            &self.values,
            &route_points,
            &self.meta,
        );
        Ok(QuerySchema {
            version,
            script,
            tables: self.tables,
            by_name,
            by_object: self.by_object,
            refs,
            ref_by_qname,
            field_index,
            sys: self.sys,
            sys_enums: self.sys_enums,
            route_points,
            values: self.values,
            meta: self.meta,
            diagnostics: self.diagnostics,
        })
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= u64::from(*b);
        *h = h.wrapping_mul(FNV_PRIME);
    }
}

struct Fnv(u64);

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        fnv1a(&mut self.0, bytes);
    }
}

fn hash_sorted<K: Ord + Hash, V: Hash>(m: &HashMap<K, V>, h: &mut Fnv) {
    let mut v: Vec<(&K, &V)> = m.iter().collect();
    v.sort_by(|a, b| a.0.cmp(b.0));
    v.hash(h);
}

fn hash_per_table<V: Hash>(m: &HashMap<QTableId, HashMap<String, V>>, h: &mut Fnv) {
    let mut v: Vec<(&QTableId, &HashMap<String, V>)> = m.iter().collect();
    v.sort_by_key(|(t, _)| **t);
    v.len().hash(h);
    for (t, inner) in v {
        t.hash(h);
        hash_sorted(inner, h);
    }
}

fn version(
    script: ScriptVariant,
    tables: &[QTable],
    refs: &[RefInfo],
    values: &ValueIndex,
    route_points: &HashMap<RefAtom, Guid>,
    meta: &HashMap<QTableId, ObjMeta>,
) -> u64 {
    let mut h = Fnv(FNV_OFFSET);
    script.hash(&mut h);
    tables.hash(&mut h);
    refs.hash(&mut h);
    hash_per_table(&values.enums, &mut h);
    hash_per_table(&values.predefined, &mut h);
    hash_sorted(&values.points, &mut h);
    hash_sorted(route_points, &mut h);
    hash_sorted(meta, &mut h);
    h.finish()
}

pub(crate) fn string(len: u32, fixed: bool) -> QType {
    QType::str_(StrQ { len, fixed })
}

pub(crate) fn number(digits: u8, scale: u8, nonneg: bool) -> QType {
    QType::num(NumQ {
        digits,
        scale,
        nonneg,
    })
}
