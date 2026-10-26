use super::{Builder, FieldRole, ObjMeta, QFieldId, QTableId, StdRole};
use crate::QType;
use bsl_prim::fold_name;
use md_model::{ChildTag, MdRef, Value};
use mdschema::MdKind;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Addressing {
    pub register: QTableId,
    pub main: QFieldId,
    pub fields: Vec<(QFieldId, QFieldId)>,
    pub session: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Criterion {
    pub ty: QType,
    pub members: Vec<(QTableId, QFieldId)>,
}

fn reference(v: Option<&Value>) -> Option<&MdRef> {
    match v {
        Some(Value::Ref(r)) => Some(r),
        _ => None,
    }
}
fn field(b: &Builder<'_>, table: QTableId, r: &MdRef) -> Option<QFieldId> {
    b.table_ref(table)?
        .fields
        .iter()
        .enumerate()
        .find_map(|(i, f)| {
            (match r.id {
                Some(id) => f.md == Some(id),
                None => r
                    .name
                    .rsplit('.')
                    .next()
                    .is_some_and(|n| fold_name(n) == fold_name(&f.name_en)),
            })
            .then(|| u16::try_from(i).ok().map(QFieldId))
            .flatten()
        })
}
fn resolve_field(b: &Builder<'_>, r: &MdRef) -> Option<(QTableId, QFieldId)> {
    let parts: Vec<_> = r.name.split('.').collect();
    let owner = b.by_full.get(&fold_name(&parts.get(..2)?.join(".")))?;
    let table = match parts.as_slice() {
        [_, _, "Attribute" | "AddressingAttribute" | "Dimension", _] => *owner,
        [_, _, "TabularSection", name, "Attribute", _] => {
            b.table_ref(*owner)?
                .fields
                .iter()
                .find(|f| f.name_en == *name)?
                .nested?
        }
        _ => return None,
    };
    Some((table, field(b, table, r)?))
}

pub(super) fn build(b: &mut Builder<'_>) {
    let cfg = b.cfg;
    for o in &cfg.objects {
        let Some(&table) = b.by_object.get(&o.id) else {
            continue;
        };
        if o.kind == MdKind::Task {
            let facts = (|| {
                let addressing = reference(o.prop("Addressing"))?;
                let register = match addressing.id {
                    Some(id) => b.by_object.get(&id).copied(),
                    None => b.by_full.get(&fold_name(&addressing.name)).copied(),
                }?;
                if b.table_ref(register)?.kind
                    != super::TableKind::Object(MdKind::InformationRegister)
                {
                    return None;
                }
                let main = field(b, table, reference(o.prop("MainAddressingAttribute"))?)?;
                let mut fields = Vec::new();
                for c in o
                    .children
                    .iter()
                    .filter(|c| c.tag == ChildTag::AddressingAttribute)
                {
                    let task_field = field(
                        b,
                        table,
                        &MdRef {
                            id: Some(c.id),
                            name: c.name.clone(),
                        },
                    )?;
                    let (reg, dim) = resolve_field(b, reference(c.prop("AddressingDimension"))?)?;
                    if reg != register
                        || b.table_ref(reg)?.fields.get(usize::from(dim.0))?.role
                            != FieldRole::Dimension
                    {
                        return None;
                    }
                    fields.push((task_field, dim));
                }
                if !fields.iter().any(|(f, _)| *f == main) {
                    return None;
                }
                let session = reference(o.prop("CurrentPerformer"))
                    .and_then(|r| r.name.strip_prefix("SessionParameter."))
                    .map(str::to_string);
                Some(Addressing {
                    register,
                    main,
                    fields,
                    session,
                })
            })();
            b.meta
                .entry(table)
                .or_insert_with(|| ObjMeta::new(o.kind))
                .addressing = facts;
        }
        if o.kind == MdKind::FilterCriterion {
            let facts = (|| {
                let Value::Type(ty) = o.prop("Type")? else {
                    return None;
                };
                let ty = b.map(ty, &format!("FilterCriterion.{}", o.name));
                let Value::RefList(content) = o.prop("Content")? else {
                    return None;
                };
                let members = content
                    .iter()
                    .map(|r| resolve_field(b, r))
                    .collect::<Option<Vec<_>>>()?;
                if members.iter().any(|(t, _)| {
                    !b.table_ref(*t).is_some_and(|t| {
                        t.fields
                            .iter()
                            .any(|f| f.role == FieldRole::Std(StdRole::Ref))
                    })
                }) {
                    return None;
                }
                Some(Criterion { ty, members })
            })();
            b.meta
                .entry(table)
                .or_insert_with(|| ObjMeta::new(o.kind))
                .criterion = facts;
        }
    }
}
