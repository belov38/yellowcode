use crate::Opened;
use mdschema::MdKind;
use query_ir::{
    FieldRole, QField, QTable, QTableId, QType, QuerySchema, TableKind, TempOut, VtParamKind,
};
use query_syntax::ScriptVariant;
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq)]
pub struct TableGroup {
    pub presentation: String,
    pub tables: Vec<AvailableTable>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AvailableTable {
    pub name: String,
    pub id: Option<QTableId>,
    pub params: Vec<AvailableParam>,
    pub fields: Vec<AvailableField>,
    pub children: Vec<AvailableTable>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AvailableParam {
    pub name: String,
    pub kind: VtParamKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AvailableField {
    pub name: String,
    pub ty: Option<QType>,
    pub dimension: bool,
    pub resource: bool,
    pub nested: Option<QTableId>,
}

fn table_name(t: &QTable, ru: bool) -> String {
    if ru {
        t.name_ru.clone()
    } else {
        t.name_en.clone()
    }
}

fn field(f: &QField, ru: bool) -> AvailableField {
    AvailableField {
        name: if ru {
            f.name_ru.clone()
        } else {
            f.name_en.clone()
        },
        ty: Some(f.ty.clone()),
        dimension: f.role == FieldRole::Dimension,
        resource: f.role == FieldRole::Resource,
        nested: f.nested,
    }
}

fn table(
    schema: &QuerySchema,
    id: QTableId,
    ru: bool,
    by_owner: &[Vec<QTableId>],
) -> AvailableTable {
    let t = schema.table(id);
    let children = by_owner
        .get(id.0 as usize)
        .map(|v| v.iter().map(|c| table(schema, *c, ru, by_owner)).collect())
        .unwrap_or_default();
    AvailableTable {
        name: table_name(t, ru),
        id: Some(id),
        params: t
            .params
            .iter()
            .map(|p| AvailableParam {
                name: if ru {
                    p.name_ru.clone()
                } else {
                    p.name_en.clone()
                },
                kind: p.kind,
            })
            .collect(),
        fields: t.fields.iter().map(|f| field(f, ru)).collect(),
        children,
    }
}

fn section(kind: MdKind, ru: bool) -> String {
    let Some(folder) = kind.folder() else {
        return kind.as_str().to_string();
    };
    if !ru {
        return folder.to_string();
    }
    let cat = platform_catalog::catalog();
    cat.find_type("ConfigurationMetadataObject")
        .and_then(|t| cat.find_property(t, folder))
        .map(|p| p.name_ru.clone())
        .unwrap_or_else(|| folder.to_string())
}

pub fn database(schema: &QuerySchema) -> Vec<TableGroup> {
    let ru = schema.script() == ScriptVariant::Russian;
    let tables = schema.tables();
    let mut by_owner: Vec<Vec<QTableId>> = vec![Vec::new(); tables.len()];
    let mut roots: Vec<(MdKind, QTableId)> = Vec::new();
    for (i, t) in tables.iter().enumerate() {
        let id = QTableId(i as u32);
        match t.owner {
            Some(o) => {
                if let Some(v) = by_owner.get_mut(o.0 as usize) {
                    v.push(id);
                }
            }
            None => {
                let kind = match t.kind {
                    TableKind::Object(k) => Some(k),
                    TableKind::Constants => Some(MdKind::Constant),
                    _ => None,
                };
                if let Some(k) = kind {
                    roots.push((k, id));
                }
            }
        }
    }
    MdKind::all()
        .iter()
        .filter_map(|&kind| {
            let tabs: Vec<AvailableTable> = roots
                .iter()
                .filter(|(k, _)| *k == kind)
                .map(|(_, id)| table(schema, *id, ru, &by_owner))
                .collect();
            (!tabs.is_empty()).then(|| TableGroup {
                presentation: section(kind, ru),
                tables: tabs,
            })
        })
        .collect()
}

pub fn temp_tables(opened: &Opened, before: usize) -> Vec<AvailableTable> {
    let mut out: Vec<AvailableTable> = Vec::new();
    for c in opened.compiled.iter().take(before) {
        if let Some(d) = &c.drop {
            let key = bsl_prim::fold_name(d);
            out.retain(|t| bsl_prim::fold_name(&t.name) != key);
        }
        if let Some(TempOut::Put(s)) = &c.temp_out {
            let key = bsl_prim::fold_name(&s.name);
            out.retain(|t| bsl_prim::fold_name(&t.name) != key);
            out.push(AvailableTable {
                name: s.name.clone(),
                id: None,
                params: Vec::new(),
                fields: s
                    .columns
                    .iter()
                    .map(|c| AvailableField {
                        name: c.name.clone(),
                        ty: c.ty.clone(),
                        dimension: false,
                        resource: false,
                        nested: None,
                    })
                    .collect(),
                children: Vec::new(),
            });
        }
    }
    out
}

pub fn ref_fields(schema: &QuerySchema, ty: &QType) -> Vec<AvailableField> {
    let ru = schema.script() == ScriptVariant::Russian;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in &ty.refs {
        let t = schema.table(schema.ref_info(*r).table);
        for f in &t.fields {
            let af = field(f, ru);
            if seen.insert(bsl_prim::fold_name(&af.name)) {
                out.push(af);
            }
        }
    }
    out
}

pub fn columns(opened: &Opened, stmt: usize) -> Vec<(String, Option<QType>)> {
    let Some(c) = opened.compiled.get(stmt) else {
        return Vec::new();
    };
    match &c.temp_out {
        Some(TempOut::Put(s)) => s
            .columns
            .iter()
            .map(|c| (c.name.clone(), c.ty.clone()))
            .collect(),
        _ => c
            .columns
            .iter()
            .map(|c| (c.name.clone(), Some(c.ty.clone())))
            .collect(),
    }
}
