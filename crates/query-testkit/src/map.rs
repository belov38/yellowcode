use std::sync::Arc;

use bsl_prim::{BslDate, Collation, IcuCollation, Number};
use ibschema::logical::TypeCode;
use ibschema::names::StdField;
use ibschema::rules::{self, RulesOptions};
use ibschema::storage::{FieldInfo, IndexId, StorageMap, TableId};
use ibschema::values::Uuid16;
use query_ir::{QStr, QValue};

pub fn synthetic_map(cfg: &md_model::Configuration) -> Arc<StorageMap> {
    let seed = infobase::seed::seed();
    let mut names = seed.dbnames.clone();
    let built = rules::build(cfg, &mut names, &seed.schema, &RulesOptions::default())
        .expect("rules::build of a synthetic model");
    let map = StorageMap::new(Arc::new(built.schema.clone()), &built).expect("StorageMap::new");
    Arc::new(map)
}

pub fn ru_collation() -> Arc<dyn Collation> {
    Arc::new(IcuCollation::new("ru_RU"))
}

pub fn md_of(cfg: &md_model::Configuration, qname: &str) -> Uuid16 {
    let (kind, name) = qname.split_once('.').expect("Kind.Name");
    cfg.objects
        .iter()
        .find(|o| o.kind.as_str() == kind && o.name == name)
        .unwrap_or_else(|| panic!("no object {qname}"))
        .id
}

fn empty_value(map: &StorageMap, f: &FieldInfo) -> QValue {
    let Some(ty) = f.types.first() else {
        return QValue::Undefined;
    };
    if f.nullable && f.types.len() > 1 {
        return QValue::Null;
    }
    match ty.code {
        TypeCode::B => QValue::Binary(Arc::<[u8]>::from(Vec::<u8>::new()).into()),
        TypeCode::L => QValue::Bool(false),
        TypeCode::N => QValue::Number(Number::zero()),
        TypeCode::S => QValue::String(QStr::from_str("")),
        TypeCode::T => QValue::Date(BslDate::from_ticks(0).expect("zero date")),
        TypeCode::R => QValue::Ref {
            table: map
                .table_by_name(&ty.ref_table)
                .and_then(|t| map.ref_number(t))
                .unwrap_or(0),
            id: [0; 16],
        },
        TypeCode::E | TypeCode::V => QValue::Undefined,
    }
}

pub fn catalog_row(map: &StorageMap, t: TableId, n: u8, descr: &str) -> Vec<QValue> {
    let info = map.table(t);
    let number = map.ref_number(t).unwrap_or(0);
    info.fields
        .iter()
        .map(|f| match f.std {
            Some(StdField::Id) => {
                let mut id = [0u8; 16];
                id[15] = n;
                QValue::Ref { table: number, id }
            }
            _ if f.name == "Description" => QValue::String(QStr::from_str(descr)),
            _ => empty_value(map, f),
        })
        .collect()
}

pub fn index_by_fields(map: &StorageMap, t: TableId, names: &[&str]) -> IndexId {
    let info = map.table(t);
    for (k, ix) in info.indexes.iter().enumerate() {
        let got: Vec<&str> = ix
            .fields
            .iter()
            .skip(ix.prefix)
            .filter_map(|f| info.field(*f).map(|f| f.name.as_str()))
            .collect();
        if got.len() >= names.len() && got[..names.len()] == *names {
            return IndexId::Named(u16::try_from(k).expect("index count"));
        }
    }
    panic!("no index over {names:?} in {}", info.logical)
}
