use super::{AppModel, FieldDef, FieldRole, ObjDef, ObjId, ObjKind, StdAttr};
use crate::error::{AppError, Result};
use ibschema::names::StdField;
use ibschema::storage::{FieldId, StdList, StorageMap, TableId};
use platform_catalog::messages::Lang;
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct ModelStorage {
    pub objects: Vec<Option<ObjStorage>>,
    pub by_ref_number: HashMap<u32, ObjId>,
}

#[derive(Clone, Debug)]
pub struct ObjStorage {
    pub main: TableId,
    pub ref_number: Option<u32>,
    pub fields: Vec<Option<FieldId>>,
    pub tabular: Vec<(TableId, Vec<Option<FieldId>>)>,
    pub service: Vec<(String, TableId)>,
}

pub fn std_fields(a: StdAttr) -> &'static [StdField] {
    use StdAttr as A;
    use StdField as F;
    match a {
        A::Ref => &[F::Id],
        A::DeletionMark => &[F::Marked],
        A::Predefined => &[],
        A::PredefinedDataName => &[F::PredefinedId],
        A::Code => &[F::Code],
        A::Description => &[F::Description, F::Name],
        A::IsFolder => &[F::Folder],
        A::Parent => &[F::ParentId],
        A::Owner => &[F::OwnerId],
        A::Date => &[F::DateTime],
        A::Number => &[F::Number],
        A::Posted => &[F::Posted],
        A::LineNumber => &[F::LineNo],
        A::Recorder => &[F::Recorder],
        A::Period | A::RegistrationPeriod => &[F::Period],
        A::Active => &[F::Active],
        A::RecordType => &[F::RecordKind, F::Correspond],
        A::Order => &[F::EnumOrder, F::OrderField],
        A::ValueType => &[F::Type],
        A::AccountType => &[F::Kind],
        A::OffBalance => &[F::OffBalance],
        A::ActionPeriodIsBasic => &[F::BaseAsAct],
        A::ExchangeDate => &[F::LastUpdate],
        A::SentNo => &[F::SentNo],
        A::ReceivedNo => &[F::ReceivedNo],
        A::ThisNode => &[],
        A::HeadTask => &[F::HeadTask],
        A::Completed => &[F::Completed],
        A::Started => &[F::Started],
        A::BusinessProcess => &[F::BusinessProcess],
        A::Executed => &[F::Executed],
        A::RoutePoint => &[F::Point],
        A::Account => &[F::Account],
        A::AccountDr => &[F::AccountDt],
        A::AccountCr => &[F::AccountCt],
        A::CalculationType => &[F::CalcKind],
        A::ActionPeriod => &[F::ActionPeriod],
        A::BegOfActionPeriod => &[F::ApDateFrom],
        A::EndOfActionPeriod => &[F::ApDateTill],
        A::BegOfBasePeriod => &[F::BpDateFrom],
        A::EndOfBasePeriod => &[F::BpDateTill],
        A::ReversingEntry => &[F::Storno],
        A::ActualActionPeriod => &[],
    }
}

fn field_in(map: &StorageMap, table: TableId, f: &FieldDef) -> Option<FieldId> {
    let t = map.try_table(table)?;
    if let Some(a) = f.std {
        return std_fields(a).iter().find_map(|s| t.field_by_std(*s));
    }
    let md = f.md?;
    if f.role == FieldRole::Common {
        let i = t
            .fields
            .iter()
            .position(|x| x.md == Some(md) && !x.implicit)?;
        return Some(FieldId(u16::try_from(i).ok()?));
    }
    map.field(md).filter(|(t, _)| *t == table).map(|(_, id)| id)
}

fn full_name(o: &ObjDef) -> String {
    format!("{}.{}", o.kind.name(Lang::En), o.name)
}

fn bind_object(o: &ObjDef, map: &StorageMap) -> Result<Option<ObjStorage>> {
    if matches!(o.kind, ObjKind::DataProcessor | ObjKind::Report) {
        return Ok(None);
    }
    let no_table = || AppError::Config(format!("{}: no table", full_name(o)));
    let st = map.object(o.md).ok_or_else(no_table)?;
    map.try_table(st.main).ok_or_else(no_table)?;
    let fields = o.fields.iter().map(|f| field_in(map, st.main, f)).collect();
    let mut tabular = Vec::with_capacity(o.tabular.len());
    for tab in &o.tabular {
        let id = match tab.std_list {
            Some(list) => st.list(list),
            None => st
                .tabular
                .iter()
                .find(|(u, id)| *u == tab.md && map.try_table(*id).is_some())
                .map(|(_, id)| *id),
        }
        .ok_or_else(|| AppError::Config(format!("{}.{}: no table", full_name(o), tab.name)))?;
        let ids = if let Some(list) = tab.std_list {
            let fields = match list {
                StdList::LeadingCalculationTypes => [
                    StdField::PredefinedLeadingCkTableLine,
                    StdField::LeadingCkLeadingCk,
                    StdField::LeadingCkLineNo,
                ],
                StdList::DisplacingCalculationTypes => [
                    StdField::PredefinedDisplCkTableLine,
                    StdField::DisplacedCkDisplCk,
                    StdField::DisplacedCkLineNo,
                ],
                StdList::BaseCalculationTypes => [
                    StdField::PredefinedBaseCkTableLine,
                    StdField::BaseCkBaseCk,
                    StdField::BaseCkLineNo,
                ],
                StdList::ExtDimensionTypes => {
                    return Err(AppError::Config("not a calculation list".into()));
                }
            };
            fields
                .iter()
                .map(|f| map.table(id).field_by_std(*f))
                .collect()
        } else {
            tab.fields.iter().map(|f| field_in(map, id, f)).collect()
        };
        tabular.push((id, ids));
    }
    Ok(Some(ObjStorage {
        main: st.main,
        ref_number: if o.kind.is_reference() {
            map.ref_number(st.main)
        } else {
            None
        },
        fields,
        tabular,
        service: st.service.clone(),
    }))
}

impl AppModel {
    pub fn bind(&mut self, map: &StorageMap) -> Result<()> {
        let mut definitions = self.objects.clone();
        for o in &mut definitions {
            if o.kind != ObjKind::ChartOfCalculationTypes {
                continue;
            }
            o.tabular.retain(|t| t.std_list.is_none());
            let Some(st) = map.object(o.md) else {
                continue;
            };
            for (list, ru, en) in [
                (
                    StdList::LeadingCalculationTypes,
                    "ВедущиеВидыРасчета",
                    "LeadingCalculationTypes",
                ),
                (
                    StdList::DisplacingCalculationTypes,
                    "ВытесняющиеВидыРасчета",
                    "DisplacingCalculationTypes",
                ),
                (
                    StdList::BaseCalculationTypes,
                    "БазовыеВидыРасчета",
                    "BaseCalculationTypes",
                ),
            ] {
                if st.list(list).is_none() || o.tabular.iter().any(|t| t.std_list == Some(list)) {
                    continue;
                }
                let field = |attr: StdAttr, types| super::FieldDef {
                    md: None,
                    name: attr.name(self.script).into(),
                    synonym: Default::default(),
                    std: Some(attr),
                    role: FieldRole::Standard,
                    types,
                    usage: super::Usage::ForFolderAndItem,
                    fill_check: false,
                    indexed: false,
                };
                o.tabular.push(super::TabDef {
                    std_list: Some(list),
                    md: o.md,
                    name: if self.script == md_model::ScriptVariant::Russian {
                        ru
                    } else {
                        en
                    }
                    .into(),
                    synonym: Default::default(),
                    fields: vec![
                        field(
                            StdAttr::Predefined,
                            super::TypeSet::of_atom(super::TypeAtom::Bool),
                        ),
                        field(
                            StdAttr::CalculationType,
                            super::TypeSet::of_atom(super::TypeAtom::AnyRef(Some(
                                ObjKind::ChartOfCalculationTypes,
                            ))),
                        ),
                        super::stdattrs::line_number_field(self.script, 5),
                    ],
                });
            }
        }
        let objects = definitions
            .iter()
            .map(|o| bind_object(o, map))
            .collect::<Result<Vec<_>>>()?;
        let by_ref_number = objects
            .iter()
            .enumerate()
            .filter_map(|(i, o)| Some((o.as_ref()?.ref_number?, ObjId(u32::try_from(i).ok()?))))
            .collect();
        self.objects = definitions;
        self.storage = Some(ModelStorage {
            objects,
            by_ref_number,
        });
        Ok(())
    }

    pub fn obj_of_ref_number(&self, number: u32) -> Option<ObjId> {
        self.storage.as_ref()?.by_ref_number.get(&number).copied()
    }

    pub fn storage_of(&self, id: ObjId) -> Option<&ObjStorage> {
        self.storage.as_ref()?.objects.get(id.0 as usize)?.as_ref()
    }
}
