use super::RefInfo;
use crate::types::{DateQ, NumQ, Other, QType, RefAtom, StrQ};
use brace::Guid;
use bsl_prim::fold_name;
use md_model::{DateFractions, TypeDescription, TypeRef};
use mdschema::MdKind;
use platform_catalog::{Catalog, TypeIdx, TypeKind};
use std::collections::HashMap;

pub(crate) struct SysTypes {
    pub null: Guid,
    pub boolean: Guid,
    pub number: Guid,
    pub string: Guid,
    pub date: Guid,
    pub uuid: Guid,
    pub binary: Guid,
    pub storage: Guid,
    pub type_: Guid,
    pub type_description: Guid,
    pub point_in_time: Guid,
    pub boundary: Guid,
    pub query_result: Guid,
    pub value_table: Guid,
    pub account_type: Option<TypeIdx>,
    names: HashMap<String, QType>,
}

fn named_types() -> [(&'static str, QType); 13] {
    [
        ("Null", QType::null_only()),
        ("Boolean", QType::boolean()),
        (
            "Number",
            QType::num(NumQ {
                digits: 0,
                scale: 0,
                nonneg: false,
            }),
        ),
        (
            "String",
            QType::str_(StrQ {
                len: 0,
                fixed: false,
            }),
        ),
        ("Date", QType::date(DateQ::DateTime)),
        ("UUID", QType::other(Other::Uuid)),
        ("BinaryData", QType::other(Other::Binary)),
        ("ValueStorage", QType::other(Other::Storage)),
        ("Type", QType::other(Other::Type)),
        ("TypeDescription", QType::other(Other::TypeDescription)),
        ("PointInTime", QType::other(Other::PointInTime)),
        ("Boundary", QType::other(Other::Boundary)),
        ("QueryResult", QType::other(Other::QueryResult)),
    ]
}

fn guid_of(cat: &Catalog, idx: TypeIdx) -> Option<Guid> {
    let info = cat.types().get(usize::from(idx.0))?;
    info.guid.map(Guid::from_bytes)
}

impl SysTypes {
    pub fn new(cat: &Catalog, diags: &mut Vec<String>) -> SysTypes {
        let mut guids = HashMap::new();
        let mut names = HashMap::new();
        for (en, ty) in named_types() {
            let found = cat.find_type(en).and_then(|idx| {
                let info = cat.types().get(usize::from(idx.0))?;
                Some((idx, info))
            });
            match found {
                Some((idx, info)) => {
                    names.insert(fold_name(&info.name_ru), ty.clone());
                    names.insert(fold_name(&info.name_en), ty);
                    match guid_of(cat, idx) {
                        Some(g) => {
                            guids.insert(en, g);
                        }
                        None => diags.push(format!("platform type {en}: no UUID in the catalog")),
                    }
                }
                None => diags.push(format!("platform type {en}: not in the catalog")),
            }
        }
        let value_table = cat.find_type("ValueTable").and_then(|i| guid_of(cat, i));
        if value_table.is_none() {
            diags.push("platform type ValueTable: no UUID in the catalog".into());
        }
        let g = |n: &str| guids.get(n).copied().unwrap_or(Guid::NIL);
        let account_type = cat.find_type("AccountType");
        if account_type.is_none() {
            diags.push("platform type AccountType: not in the catalog".into());
        }
        SysTypes {
            null: g("Null"),
            boolean: g("Boolean"),
            number: g("Number"),
            string: g("String"),
            date: g("Date"),
            uuid: g("UUID"),
            binary: g("BinaryData"),
            storage: g("ValueStorage"),
            type_: g("Type"),
            type_description: g("TypeDescription"),
            point_in_time: g("PointInTime"),
            boundary: g("Boundary"),
            query_result: g("QueryResult"),
            value_table: value_table.unwrap_or(Guid::NIL),
            account_type,
            names,
        }
    }

    pub(super) fn by_folded_name(&self, name: &str) -> Option<&QType> {
        self.names.get(name)
    }

    pub fn by_name(&self, name: &str) -> Option<QType> {
        self.names.get(&fold_name(name)).cloned()
    }
}

struct SysEnumInfo {
    guid: Guid,
    values: HashMap<String, u32>,
}

pub(crate) struct SysEnums {
    by_name: HashMap<String, TypeIdx>,
    info: HashMap<TypeIdx, SysEnumInfo>,
}

impl SysEnums {
    pub fn new(cat: &Catalog) -> SysEnums {
        let mut by_name: HashMap<String, TypeIdx> = HashMap::new();
        let mut info = HashMap::new();
        for (i, t) in cat.types().iter().enumerate() {
            if t.kind != TypeKind::Enum {
                continue;
            }
            let Ok(n) = u16::try_from(i) else { continue };
            let idx = TypeIdx(n);
            let mut values = HashMap::new();
            for (k, v) in t.values.iter().enumerate() {
                let Ok(k) = u32::try_from(k) else { continue };
                values.entry(fold_name(&v.name_ru)).or_insert(k);
                values.entry(fold_name(&v.name_en)).or_insert(k);
            }
            info.insert(
                idx,
                SysEnumInfo {
                    guid: t.guid.map(Guid::from_bytes).unwrap_or(Guid::NIL),
                    values,
                },
            );
            for name in [&t.name_ru, &t.name_en] {
                let key = fold_name(name);
                let replace = match by_name.get(&key) {
                    None => true,
                    Some(old) => {
                        t.presentation_ru.is_some()
                            && cat
                                .types()
                                .get(usize::from(old.0))
                                .is_some_and(|o| o.presentation_ru.is_none())
                    }
                };
                if replace {
                    by_name.insert(key, idx);
                }
            }
        }
        SysEnums { by_name, info }
    }

    pub(super) fn find_folded(&self, name: &str) -> Option<TypeIdx> {
        self.by_name.get(name).copied()
    }

    pub(super) fn value_folded(&self, ty: &str, value: &str) -> Option<(TypeIdx, u32)> {
        let idx = self.find_folded(ty)?;
        Some((idx, *self.info.get(&idx)?.values.get(value)?))
    }

    pub fn find(&self, name: &str) -> Option<TypeIdx> {
        self.by_name.get(&fold_name(name)).copied()
    }

    pub fn value(&self, ty: &str, value: &str) -> Option<(TypeIdx, u32)> {
        let idx = self.find(ty)?;
        let v = self.info.get(&idx)?.values.get(&fold_name(value))?;
        Some((idx, *v))
    }

    pub fn guid(&self, idx: TypeIdx) -> Guid {
        self.info.get(&idx).map_or(Guid::NIL, |i| i.guid)
    }
}

#[derive(Default)]
pub(crate) struct RefIndex {
    pub refs: Vec<RefInfo>,
    pub by_qname: HashMap<String, RefAtom>,
    pub by_type: HashMap<Guid, RefAtom>,
    pub rp_by_qname: HashMap<String, RefAtom>,
    pub rp_by_type: HashMap<Guid, RefAtom>,
    pub rp_uuid: HashMap<RefAtom, Guid>,
}

const REF_SETS: [(&str, MdKind); 9] = [
    ("cfg:CatalogRef", MdKind::Catalog),
    ("cfg:DocumentRef", MdKind::Document),
    ("cfg:EnumRef", MdKind::Enum),
    (
        "cfg:ChartOfCharacteristicTypesRef",
        MdKind::ChartOfCharacteristicTypes,
    ),
    ("cfg:ChartOfAccountsRef", MdKind::ChartOfAccounts),
    (
        "cfg:ChartOfCalculationTypesRef",
        MdKind::ChartOfCalculationTypes,
    ),
    ("cfg:ExchangePlanRef", MdKind::ExchangePlan),
    ("cfg:BusinessProcessRef", MdKind::BusinessProcess),
    ("cfg:TaskRef", MdKind::Task),
];

const ANY_REF: [&str; 2] = ["cfg:AnyIBRef", "cfg:AnyRef"];
const ROUTE_POINT_SET: &str = "cfg:BusinessProcessRoutePointRef";

impl RefIndex {
    pub fn add(&mut self, info: RefInfo, names: &[String]) -> RefAtom {
        let r = RefAtom(u32::try_from(self.refs.len()).unwrap_or(u32::MAX));
        for n in names {
            self.by_qname.entry(fold_name(n)).or_insert(r);
        }
        if !info.type_uuid.is_nil() {
            self.by_type.entry(info.type_uuid).or_insert(r);
        }
        self.refs.push(info);
        r
    }

    pub fn of_kind(&self, kind: MdKind) -> Vec<RefAtom> {
        (0..self.refs.len())
            .filter(|i| self.refs.get(*i).is_some_and(|r| r.kind == kind))
            .filter_map(|i| u32::try_from(i).ok().map(RefAtom))
            .collect()
    }

    pub fn route_points(&self) -> Vec<Other> {
        let mut v: Vec<RefAtom> = self.rp_uuid.keys().copied().collect();
        v.sort();
        v.into_iter().map(Other::RoutePoint).collect()
    }
}

fn resolve(
    r: &TypeRef,
    by_type: &HashMap<Guid, RefAtom>,
    by_name: &HashMap<String, RefAtom>,
    name: &str,
    owner: &str,
    diags: &mut Vec<String>,
) -> Option<RefAtom> {
    let named = by_name.get(&fold_name(name)).copied();
    let typed = r.type_id.and_then(|g| by_type.get(&g).copied());
    match (typed, named) {
        (Some(t), Some(n)) if t != n => {
            diags.push(format!(
                "{owner}: type {}: the type id names another object",
                r.name
            ));
            Some(t)
        }
        (Some(t), _) => Some(t),
        (None, n) => n,
    }
}

pub(crate) fn map_type(
    refs: &RefIndex,
    sys_enums: &SysEnums,
    td: &TypeDescription,
    owner: &str,
    diags: &mut Vec<String>,
) -> QType {
    let mut t = QType::default();
    for r in &td.types {
        let legacy = r.name.to_string();
        let q = legacy.as_str();
        match q {
            "xs:decimal" => {
                t.number = Some(td.number.map_or(
                    NumQ {
                        digits: 0,
                        scale: 0,
                        nonneg: false,
                    },
                    |n| NumQ {
                        digits: u8::try_from(n.digits).unwrap_or(u8::MAX),
                        scale: u8::try_from(n.fraction).unwrap_or(u8::MAX),
                        nonneg: n.nonnegative,
                    },
                ));
            }
            "xs:string" => {
                t.string = Some(td.string.map_or(
                    StrQ {
                        len: 0,
                        fixed: false,
                    },
                    |s| StrQ {
                        len: s.length,
                        fixed: s.fixed,
                    },
                ));
            }
            "xs:dateTime" => {
                t.date = Some(match td.date {
                    Some(DateFractions::Date) => DateQ::Date,
                    Some(DateFractions::Time) => DateQ::Time,
                    Some(DateFractions::DateTime) | None => DateQ::DateTime,
                });
            }
            "xs:boolean" => t.boolean = true,
            "xs:base64Binary" => t.others.push(Other::Binary),
            "v8:UUID" => t.others.push(Other::Uuid),
            "v8:ValueStorage" => t.others.push(Other::Storage),
            "v8:Type" => t.others.push(Other::Type),
            "v8:TypeDescription" => t.others.push(Other::TypeDescription),
            "v8:PointInTime" => t.others.push(Other::PointInTime),
            "v8:Boundary" => t.others.push(Other::Boundary),
            "v8:Null" => t.null = true,
            _ if ANY_REF.contains(&q) => {
                for (_, kind) in REF_SETS {
                    t.refs.extend(refs.of_kind(kind));
                }
                t.others.extend(refs.route_points());
            }
            _ if q == ROUTE_POINT_SET => t.others.extend(refs.route_points()),
            _ => {
                if let Some((_, kind)) = REF_SETS.iter().find(|(s, _)| *s == q) {
                    t.refs.extend(refs.of_kind(*kind));
                    continue;
                }
                if let Some(name) = q.strip_prefix("cfg:")
                    && name.contains('.')
                {
                    let point = name.starts_with("BusinessProcessRoutePointRef.");
                    let (by_type, by_name) = if point {
                        (&refs.rp_by_type, &refs.rp_by_qname)
                    } else {
                        (&refs.by_type, &refs.by_qname)
                    };
                    match resolve(r, by_type, by_name, name, owner, diags) {
                        Some(a) if point => t.others.push(Other::RoutePoint(a)),
                        Some(a) => t.refs.push(a),
                        None => diags.push(format!("{owner}: unknown type {q}")),
                    }
                    continue;
                }
                if let Some(name) = q.strip_prefix("v8:")
                    && let Some(idx) = sys_enums.find(name)
                {
                    t.others.push(Other::SysEnum(idx));
                    continue;
                }
                diags.push(format!("{owner}: type {q} has no query type, skipped"));
            }
        }
    }
    t.refs.sort();
    t.refs.dedup();
    t.others.sort();
    t.others.dedup();
    t
}
