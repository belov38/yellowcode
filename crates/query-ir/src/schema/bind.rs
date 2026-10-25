use super::{
    FieldRole, QField, QFieldId, QTable, QTableId, QuerySchema, SchemaError, Side, StdRole,
    TableKind, VtKind,
};
use crate::types::RefAtom;
use brace::Guid;
use ibschema::names::{StdField, role};
use ibschema::storage::{FieldId, ObjectStorage, StorageMap, TableId};
use mdschema::MdKind;
use std::collections::HashMap;

pub trait StorageLookup {
    fn main_table(&self, object: Guid) -> Option<TableId>;
    fn tabular_table(&self, object: Guid, section: TabularKey<'_>) -> Option<TableId>;
    fn service_table(&self, object: Guid, purpose: &str, md: Option<Guid>) -> Option<TableId>;
    fn md_fields(&self, t: TableId) -> Vec<(Guid, Option<Side>, FieldId)>;
    fn field_by_std(&self, t: TableId, std: StdField) -> Option<FieldId>;
    fn field_by_name(&self, t: TableId, name: &str) -> Option<FieldId>;
    fn ref_number(&self, t: TableId) -> Option<u32>;
    fn enum_value(&self, value: Guid) -> Option<(TableId, u32)>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabularKey<'a> {
    Md(Guid),
    StdName(&'a str),
}

fn side_of(s: ibschema::names::Side) -> Side {
    match s {
        ibschema::names::Side::Dr => Side::Dr,
        ibschema::names::Side::Cr => Side::Cr,
    }
}

impl StorageLookup for StorageMap {
    fn main_table(&self, object: Guid) -> Option<TableId> {
        self.object(object).map(|o| o.main)
    }

    fn tabular_table(&self, object: Guid, section: TabularKey<'_>) -> Option<TableId> {
        let o: &ObjectStorage = self.object(object)?;
        match section {
            TabularKey::Md(g) => o.tabular.iter().find(|(m, _)| *m == g).map(|(_, t)| *t),
            TabularKey::StdName(n) => o
                .tabular
                .iter()
                .filter(|(m, _)| *m == object)
                .find(|(_, t)| {
                    self.try_table(*t).is_some_and(|i| {
                        i.query_name
                            .strip_suffix(n)
                            .is_some_and(|p| p.ends_with('.'))
                    })
                })
                .map(|(_, t)| *t),
        }
    }

    fn service_table(&self, object: Guid, purpose: &str, md: Option<Guid>) -> Option<TableId> {
        let o = self.object(object)?;
        o.service
            .iter()
            .filter(|(p, _)| p == purpose)
            .find(|(_, t)| md.is_none() || self.try_table(*t).and_then(|i| i.md) == md)
            .map(|(_, t)| *t)
    }

    fn md_fields(&self, t: TableId) -> Vec<(Guid, Option<Side>, FieldId)> {
        let Some(info) = self.try_table(t) else {
            return Vec::new();
        };
        info.fields
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                Some((f.md?, f.side.map(side_of), FieldId(u16::try_from(i).ok()?)))
            })
            .collect()
    }

    fn field_by_std(&self, t: TableId, std: StdField) -> Option<FieldId> {
        self.try_table(t)?.field_by_std(std)
    }

    fn field_by_name(&self, t: TableId, name: &str) -> Option<FieldId> {
        self.try_table(t)?.field_by_name(name)
    }

    fn ref_number(&self, t: TableId) -> Option<u32> {
        StorageMap::ref_number(self, t)
    }

    fn enum_value(&self, value: Guid) -> Option<(TableId, u32)> {
        StorageMap::enum_value(self, value)
    }
}

#[derive(Clone, Copy)]
enum Slot {
    Std(StdField),
    Numbered(StdField, u8),
}

fn slots(role: StdRole, kind: Option<MdKind>, list: Option<StdRole>) -> Vec<Slot> {
    use Slot::{Numbered as N, Std as S};
    use StdRole as R;
    if let Some(l) = list {
        let (line, calc, predefined) = match l {
            R::BaseCalculationTypes => (
                StdField::BaseCkLineNo,
                StdField::BaseCkBaseCk,
                StdField::PredefinedBaseCkTableLine,
            ),
            R::LeadingCalculationTypes => (
                StdField::LeadingCkLineNo,
                StdField::LeadingCkLeadingCk,
                StdField::PredefinedLeadingCkTableLine,
            ),
            _ => (
                StdField::DisplacedCkLineNo,
                StdField::DisplacedCkDisplCk,
                StdField::PredefinedDisplCkTableLine,
            ),
        };
        match role {
            R::LineNumber => return vec![S(line)],
            R::CalculationType => return vec![S(calc)],
            R::Predefined => return vec![S(predefined)],
            _ => {}
        }
    }
    match role {
        R::Ref if kind == Some(MdKind::DocumentJournal) => vec![S(StdField::Document)],
        R::Ref => vec![S(StdField::Id)],
        R::DataVersion if kind == Some(MdKind::CalculationRegister) => Vec::new(),
        R::DataVersion => vec![S(StdField::Version)],
        R::DeletionMark => vec![S(StdField::Marked)],
        R::Owner => vec![S(StdField::OwnerId)],
        R::Parent => vec![S(StdField::ParentId)],
        R::IsFolder => vec![S(StdField::Folder)],
        R::Code => vec![S(StdField::Code)],
        R::Description if kind == Some(MdKind::Task) => vec![S(StdField::Name)],
        R::Description => vec![S(StdField::Description)],
        R::Predefined => vec![S(StdField::PredefinedId), S(StdField::DimIsMetadata)],
        R::PredefinedDataName => vec![S(StdField::PredefinedId)],
        R::Number => vec![S(StdField::Number)],
        R::Date => vec![S(StdField::DateTime)],
        R::Posted => vec![S(StdField::Posted)],
        R::Order if kind == Some(MdKind::Enum) => vec![S(StdField::EnumOrder)],
        R::Order if kind == Some(MdKind::BusinessProcess) => vec![S(StdField::RoutePointOrder)],
        R::Order => vec![S(StdField::OrderField), S(StdField::EnumOrder)],
        R::Type if kind == Some(MdKind::ChartOfAccounts) => vec![S(StdField::Kind)],
        R::LineNumber => vec![S(StdField::LineNo)],
        R::Period | R::RegistrationPeriod => vec![S(StdField::Period)],
        R::Recorder | R::RecalculationObject => vec![S(StdField::Recorder)],
        R::Active => vec![S(StdField::Active)],
        R::RecordKind => vec![S(StdField::RecordKind), S(StdField::Correspond)],
        R::Account => vec![S(StdField::Account)],
        R::AccountDr => vec![S(StdField::AccountDt)],
        R::AccountCr => vec![S(StdField::AccountCt)],
        R::ExtDimension(n) => vec![N(StdField::Value, n)],
        R::ExtDimensionType(0) => vec![S(StdField::DimKind)],
        R::ExtDimensionType(n) => vec![N(StdField::Kind, n)],
        R::ExtDimensionDr(n) => vec![N(StdField::ValueDt, n)],
        R::ExtDimensionCr(n) => vec![N(StdField::ValueCt, n)],
        R::ExtDimensionTypeDr(n) => vec![N(StdField::KindDt, n)],
        R::ExtDimensionTypeCr(n) => vec![N(StdField::KindCt, n)],
        R::ExtDimensionKind => vec![S(StdField::Kind), S(StdField::DimKind)],
        R::ValueType => vec![S(StdField::Type)],
        R::OffBalance => vec![S(StdField::OffBalance)],
        R::TurnoversOnly => vec![S(StdField::TurnoverOnly)],
        R::ActionPeriodIsBasic => vec![S(StdField::BaseAsAct)],
        R::CalculationType => vec![S(StdField::CalcKind)],
        R::SentNo => vec![S(StdField::SentNo)],
        R::ReceivedNo => vec![S(StdField::ReceivedNo)],
        R::ExchangeDate => vec![S(StdField::LastUpdate)],
        R::HeadTask => vec![S(StdField::HeadTask)],
        R::Started => vec![S(StdField::Started)],
        R::Completed => vec![S(StdField::Completed)],
        R::BusinessProcess => vec![S(StdField::BusinessProcess)],
        R::RoutePoint => vec![S(StdField::Point)],
        R::Executed => vec![S(StdField::Executed)],
        R::ActionPeriod => vec![S(StdField::ActionPeriod)],
        R::BegOfActionPeriod => vec![S(StdField::ApDateFrom)],
        R::EndOfActionPeriod => vec![S(StdField::ApDateTill)],
        R::BegOfBasePeriod => vec![S(StdField::BpDateFrom)],
        R::EndOfBasePeriod => vec![S(StdField::BpDateTill)],
        R::ReversingEntry => vec![S(StdField::Storno)],
        R::Node => vec![S(StdField::Node)],
        R::MessageNo => vec![S(StdField::MessageNo)],
        R::Value => vec![S(StdField::Value)],
        R::Presentation
        | R::PointInTime
        | R::Type
        | R::BalancedAccount
        | R::BalancedExtDimension(_)
        | R::PeriodAdjustment
        | R::ExtDimensionTypes
        | R::LeadingCalculationTypes
        | R::BaseCalculationTypes
        | R::DisplacingCalculationTypes
        | R::ThisNode => Vec::new(),
    }
}

#[derive(Clone, Debug, Default)]
pub struct StorageBinding {
    tables: Vec<Option<TableId>>,
    fields: Vec<Vec<Option<FieldId>>>,
    refs: Vec<Option<u32>>,
    enum_orders: HashMap<(QTableId, Guid), u32>,
    unbound: Vec<QTableId>,
    unbound_fields: Vec<(QTableId, QFieldId)>,
}

impl StorageBinding {
    pub fn table(&self, t: QTableId) -> Option<TableId> {
        self.tables
            .get(usize::try_from(t.0).ok()?)
            .copied()
            .flatten()
    }

    pub fn field(&self, t: QTableId, f: QFieldId) -> Option<FieldId> {
        self.fields
            .get(usize::try_from(t.0).ok()?)?
            .get(usize::from(f.0))
            .copied()
            .flatten()
    }

    pub fn ref_number(&self, r: RefAtom) -> Option<u32> {
        self.refs.get(usize::try_from(r.0).ok()?).copied().flatten()
    }

    pub fn ref_atoms(&self) -> Vec<(u32, RefAtom)> {
        let mut v: Vec<(u32, RefAtom)> = self
            .refs
            .iter()
            .enumerate()
            .filter_map(|(i, n)| Some(((*n)?, RefAtom(u32::try_from(i).ok()?))))
            .collect();
        v.sort_by_key(|(n, _)| *n);
        v
    }

    pub fn enum_order(&self, table: QTableId, value: Guid) -> Option<u32> {
        self.enum_orders.get(&(table, value)).copied()
    }

    pub fn unbound(&self) -> &[QTableId] {
        &self.unbound
    }

    pub fn unbound_fields(&self) -> &[(QTableId, QFieldId)] {
        &self.unbound_fields
    }
}

impl QuerySchema {
    pub fn bind_storage(&self, map: &dyn StorageLookup) -> Result<StorageBinding, SchemaError> {
        let mut b = StorageBinding {
            tables: Vec::with_capacity(self.tables.len()),
            fields: Vec::with_capacity(self.tables.len()),
            ..StorageBinding::default()
        };
        for (i, t) in self.tables.iter().enumerate() {
            let q = QTableId(
                u32::try_from(i).map_err(|_| SchemaError::Config("too many tables".into()))?,
            );
            let id = self.storage_table(q, t, map);
            if id.is_none() && has_storage(t.kind) && !self.values.unstored.contains(&q) {
                b.unbound.push(q);
            }
            let fields = match id {
                Some(id) => self.bind_fields(q, t, id, map, &mut b.unbound_fields),
                None => vec![None; t.fields.len()],
            };
            b.tables.push(id);
            b.fields.push(fields);
        }
        for r in &self.refs {
            let n = match b.table(r.table) {
                Some(id) => Some(map.ref_number(id).ok_or_else(|| {
                    SchemaError::Config(format!(
                        "{}: the storage has no reference number for its main table",
                        r.xml_name
                    ))
                })?),
                None => None,
            };
            b.refs.push(n);
        }
        for (q, values) in &self.values.enums {
            let Some(id) = b.table(*q) else { continue };
            for g in values.values() {
                if let Some((t, n)) = map.enum_value(*g)
                    && t == id
                {
                    b.enum_orders.insert((*q, *g), n);
                }
            }
        }
        Ok(b)
    }

    fn storage_table(&self, q: QTableId, t: &QTable, map: &dyn StorageLookup) -> Option<TableId> {
        let owner_object = || t.owner.and_then(|o| self.table(o).object);
        match (t.kind, t.object) {
            (TableKind::Object(_), Some(o)) => map.main_table(o),
            (TableKind::Tabular, Some(section)) => {
                map.tabular_table(owner_object()?, TabularKey::Md(section))
            }
            (TableKind::Changes, Some(o)) => {
                let constant = t
                    .owner
                    .is_some_and(|q| self.table(q).kind == TableKind::Object(MdKind::Constant));
                let purpose = if constant {
                    role::CONSTANT_CHANGE_RECORDS
                } else {
                    role::CHANGE_RECORDS
                };
                map.service_table(o, purpose, None)
            }
            (TableKind::Tabular, None) => {
                let owner = self.table(t.owner?);
                let name = match owner.kind {
                    TableKind::Object(MdKind::ChartOfAccounts) => "ExtDimensionTypes",
                    TableKind::Object(MdKind::ChartOfCalculationTypes) => {
                        match self.calculation_list(q)? {
                            StdRole::LeadingCalculationTypes => "LeadingCalculationTypes",
                            StdRole::BaseCalculationTypes => "BaseCalculationTypes",
                            _ => "DisplacingCalculationTypes",
                        }
                    }
                    _ => return None,
                };
                map.tabular_table(owner.object?, TabularKey::StdName(name))
            }
            (TableKind::Virtual(VtKind::ExtDimensions), None) => {
                map.service_table(owner_object()?, role::EXT_DIMENSIONS_VALUES, None)
            }
            (TableKind::Virtual(VtKind::BusinessProcessPoints), None) => {
                map.service_table(owner_object()?, role::ROUTE_POINTS, None)
            }
            (TableKind::Recalculation, Some(o)) => {
                map.service_table(owner_object()?, role::RECALCULATION, Some(o))
            }
            _ => None,
        }
    }

    fn calculation_list(&self, q: QTableId) -> Option<StdRole> {
        let t = self.table(q);
        if t.kind != TableKind::Tabular || t.object.is_some() {
            return None;
        }
        let owner = self.table(t.owner?);
        if owner.kind != TableKind::Object(MdKind::ChartOfCalculationTypes) {
            return None;
        }
        owner.fields.iter().find_map(|f| match f.role {
            FieldRole::Std(
                r @ (StdRole::LeadingCalculationTypes
                | StdRole::BaseCalculationTypes
                | StdRole::DisplacingCalculationTypes),
            ) if f.nested == Some(q) => Some(r),
            _ => None,
        })
    }

    fn bind_fields(
        &self,
        q: QTableId,
        t: &QTable,
        id: TableId,
        map: &dyn StorageLookup,
        unbound: &mut Vec<(QTableId, QFieldId)>,
    ) -> Vec<Option<FieldId>> {
        let by_md: HashMap<(Guid, Option<Side>), FieldId> = map
            .md_fields(id)
            .into_iter()
            .map(|(g, s, f)| ((g, s), f))
            .collect();
        let object = t.object;
        let kind = match t.kind {
            TableKind::Object(k) => Some(k),
            _ => t.owner.and_then(|o| match self.table(o).kind {
                TableKind::Object(k) => Some(k),
                _ => None,
            }),
        };
        let list = self.calculation_list(q);
        t.fields
            .iter()
            .enumerate()
            .map(|(k, f)| {
                let found = self.bind_field(f, (kind, object, list), id, &by_md, map);
                if found.is_none()
                    && expects_storage(f, kind, list)
                    && let Ok(k) = u16::try_from(k)
                {
                    unbound.push((q, QFieldId(k)));
                }
                found
            })
            .collect()
    }

    fn bind_field(
        &self,
        f: &QField,
        (kind, object, list): (Option<MdKind>, Option<Guid>, Option<StdRole>),
        id: TableId,
        by_md: &HashMap<(Guid, Option<Side>), FieldId>,
        map: &dyn StorageLookup,
    ) -> Option<FieldId> {
        match f.role {
            FieldRole::Virtual | FieldRole::Tabular => None,
            FieldRole::Attribute
            | FieldRole::Graph
            | FieldRole::Dimension
            | FieldRole::Resource
            | FieldRole::Common => by_md.get(&(f.md?, f.side)).copied(),
            FieldRole::Std(StdRole::Value) if kind == Some(MdKind::Constant) => {
                by_md.get(&(object?, None)).copied()
            }
            FieldRole::Std(role) => slots(role, kind, list).into_iter().find_map(|s| match s {
                Slot::Std(std) => map.field_by_std(id, std),
                Slot::Numbered(std, n) => map.field_by_name(id, &std.numbered(u32::from(n))),
            }),
        }
    }
}

fn has_storage(k: TableKind) -> bool {
    match k {
        TableKind::Virtual(v) => matches!(v, VtKind::ExtDimensions | VtKind::BusinessProcessPoints),
        TableKind::Constants => false,
        _ => true,
    }
}

fn expects_storage(f: &QField, kind: Option<MdKind>, list: Option<StdRole>) -> bool {
    match f.role {
        FieldRole::Virtual | FieldRole::Tabular => false,
        FieldRole::Std(r) => !slots(r, kind, list).is_empty(),
        _ => true,
    }
}
