use crate::values::{parse_uuid, uuid_string};
use bsl_vm::{Execution, TypeId};
use platform_catalog::{TypeIdx, catalog};

pub(crate) fn type_guid(ex: &Execution, t: TypeId) -> Option<[u8; 16]> {
    let types = ex.runtime().types();
    if let Some(app) = types.app_type(t) {
        return Some(app.guid);
    }
    catalog().type_info(types.catalog_type(t)?).guid
}

pub(crate) fn guid_text(g: &[u8; 16]) -> String {
    uuid_string(g)
}

pub(crate) fn parse_guid(text: &str) -> Option<[u8; 16]> {
    let units: Vec<u16> = text.encode_utf16().collect();
    parse_uuid(&units)
}

pub(crate) fn type_by_guid(ex: &Execution, g: &[u8; 16]) -> Option<TypeId> {
    if let Some(idx) = catalog().find_type_by_guid(g) {
        return Some(TypeId::of_catalog(idx));
    }
    let types = ex.runtime().types();
    (0..)
        .map(|n| TypeId(100_000 + n))
        .map_while(|t| types.app_type(t).map(|a| (t, a.guid)))
        .find_map(|(t, guid)| (guid == *g).then_some(t))
}

pub(crate) fn platform_type(ex: &Execution, t: TypeId) -> Option<TypeIdx> {
    let types = ex.runtime().types();
    if types.app_type(t).is_some() {
        return None;
    }
    types.catalog_type(t)
}

const MANAGERS: &[&str] = &[
    "CatalogsManager",
    "DocumentsManager",
    "EnumsManager",
    "ChartsOfCharacteristicTypesManager",
    "ChartsOfAccountsManager",
    "ChartsOfCalculationTypesManager",
    "InformationRegistersManager",
    "AccumulationRegistersManager",
    "AccountingRegistersManager",
    "CalculationRegistersManager",
    "BusinessProcessesManager",
    "TasksManager",
    "ExchangePlansManager",
    "ConstantsManager",
    "DataProcessorsManager",
    "ReportsManager",
    "SequencesManager",
    "DocumentJournalsManager",
    "FilterCriteriaManager",
    "SettingsStoragesManager",
];

pub(crate) fn is_manager(t: TypeIdx) -> bool {
    let id = catalog().type_info(t).id.as_str();
    MANAGERS.contains(&id)
}

pub(crate) const STORABLE_WITHOUT_DATA: &[&str] = &[
    "DateQualifiers",
    "BinaryDataQualifiers",
    "StringQualifiers",
    "NumberQualifiers",
    "DataCompositionTypeLink",
    "DataLock",
];

pub(crate) fn storable_without_data(t: TypeIdx) -> bool {
    let id = catalog().type_info(t).id.as_str();
    STORABLE_WITHOUT_DATA.contains(&id)
}
