use super::objects::{Common, common_field, refs_type, sub_table};
use super::registers::{ObjIndex, Tf, declare_main, flush, items};
use super::templates;
use super::{
    Builder, FieldRole, ObjMeta, Only, QField, QFieldId, QTableId, SchemaError, StdRole, TableKind,
    VtKind, number, string,
};
use crate::types::{DateQ, Other, QType, RefAtom};
use brace::Guid;
use md_model::{ChildTag, MdObject, MdRef, Value};
use mdschema::MdKind;
use std::collections::{HashMap, HashSet};

const MESSAGE_NO_DIGITS: u8 = 10;

pub(super) fn build(b: &mut Builder<'_>, commons: &[Common]) -> Result<(), SchemaError> {
    let objs = ObjIndex::new(b.cfg);
    journals(b, commons, &objs)?;
    constants(b, commons)?;
    sequences(b, commons)?;
    filter_criteria(b, &objs)?;
    tasks_by_performer(b)?;
    changes(b, &objs)
}

fn of_kind<'a>(b: &Builder<'a>, kind: MdKind) -> Vec<&'a MdObject> {
    let cfg = b.cfg;
    cfg.objects.iter().filter(|o| o.kind == kind).collect()
}

fn owner_name(o: &MdObject) -> String {
    format!("{}.{}", o.kind.as_str(), o.name)
}

fn column_source(
    b: &mut Builder<'_>,
    objs: &ObjIndex<'_>,
    r: &MdRef,
    owner: &str,
) -> Option<QType> {
    let doc = objs.get(r)?;
    let segs: Vec<&str> = r.name.split('.').collect();
    if let [_, _, "StandardAttribute", name] = segs.as_slice() {
        let t = b.table_ref(*b.by_object.get(&doc.id)?)?;
        return t
            .fields
            .iter()
            .find(|f| f.name_en == *name)
            .map(|f| f.ty.clone());
    }
    let last = segs.last().copied().unwrap_or_default();
    let c = doc
        .children
        .iter()
        .filter(|c| c.tag == ChildTag::Attribute)
        .find(|c| r.id == Some(c.id) || c.name == last)?;
    Some(b.child_type(c, owner))
}

fn journals(
    b: &mut Builder<'_>,
    commons: &[Common],
    objs: &ObjIndex<'_>,
) -> Result<(), SchemaError> {
    let tpl = templates::find(b.cat, "DocumentJournal.*", false);
    for o in of_kind(b, MdKind::DocumentJournal) {
        let owner = owner_name(o);
        let Some(table) = declare_main(b, o, tpl, TableKind::Object(o.kind))? else {
            continue;
        };
        let docs: &[MdRef] = match o.prop("RegisteredDocuments") {
            Some(Value::RefList(list)) => list,
            _ => &[],
        };
        let refs = b.refs_of(docs, &format!("{owner}.RegisteredDocuments"));
        let mut num: Option<QType> = None;
        for r in docs {
            let Some(t) = b.resolve(r).and_then(|t| b.table_ref(t)) else {
                continue;
            };
            let Some(f) = t
                .fields
                .iter()
                .find(|f| f.role == FieldRole::Std(StdRole::Number))
            else {
                continue;
            };
            num = Some(num.map_or_else(|| f.ty.clone(), |n| n.merge(&f.ty)));
        }
        let mut f = Tf::new(o.kind, tpl);
        f.std(StdRole::Ref, refs_type(refs));
        f.std(StdRole::Date, QType::date(DateQ::DateTime));
        f.std(StdRole::DeletionMark, QType::boolean());
        if let Some(n) = num {
            f.std(StdRole::Number, n);
        }
        f.std(StdRole::Posted, QType::boolean());
        f.std(StdRole::Type, QType::other(Other::Type));
        for c in o.children.iter().filter(|c| c.tag == ChildTag::Column) {
            let col_owner = format!("{owner}.{}", c.name);
            let sources: &[MdRef] = match c.prop("References") {
                Some(Value::RefList(list)) => list,
                _ => &[],
            };
            let mut ty: Option<QType> = None;
            for r in sources {
                match column_source(b, objs, r, &col_owner) {
                    Some(t) => ty = Some(ty.map_or_else(|| t.clone(), |x| x.merge(&t))),
                    None => b.diag(format!("{col_owner}: unknown field {}", r.name)),
                }
            }
            f.push(
                QField {
                    name_ru: c.name.clone(),
                    name_en: c.name.clone(),
                    ty: ty.unwrap_or_default(),
                    role: FieldRole::Graph,
                    only: Only::Any,
                    nested: None,
                    md: Some(c.id),
                    period: None,
                    side: None,
                },
                true,
            );
        }
        let used: Vec<&Common> = commons.iter().filter(|c| c.used_by(o)).collect();
        for c in &used {
            f.push(common_field(c), true);
        }
        f.virt(StdRole::Presentation, string(0, false));
        flush(b, table, f)?;
        let mut meta = ObjMeta::new(o.kind);
        meta.commons = used.iter().map(|c| c.name.clone()).collect();
        b.meta.insert(table, meta);
    }
    Ok(())
}

fn constants(b: &mut Builder<'_>, commons: &[Common]) -> Result<(), SchemaError> {
    let cat = b.cat;
    let one = templates::find(cat, "Constant.*", false);
    let all_tpl = templates::find(cat, "Constants", false);
    let mut all = Tf::new(MdKind::Constant, all_tpl);
    let list = of_kind(b, MdKind::Constant);
    for o in &list {
        let owner = owner_name(o);
        let ty = match o.prop("Type") {
            Some(Value::Type(td)) => b.map(td, &owner),
            _ => {
                b.diag(format!("{owner}: no Type"));
                QType::default()
            }
        };
        let Some(table) = declare_main(b, o, one, TableKind::Object(o.kind))? else {
            continue;
        };
        let mut f = Tf::new(o.kind, one);
        f.std(StdRole::Value, ty.clone());
        let used: Vec<&Common> = commons
            .iter()
            .filter(|c| c.separator && c.used_by(o))
            .collect();
        for c in &used {
            f.push(common_field(c), true);
        }
        flush(b, table, f)?;
        let mut meta = ObjMeta::new(o.kind);
        meta.commons = used.iter().map(|c| c.name.clone()).collect();
        b.meta.insert(table, meta);
        all.push(
            QField {
                name_ru: o.name.clone(),
                name_en: o.name.clone(),
                ty,
                role: FieldRole::Attribute,
                only: Only::Any,
                nested: None,
                md: Some(o.id),
                period: None,
                side: None,
            },
            true,
        );
    }
    if list.is_empty() {
        return Ok(());
    }
    let Some(tpl) = all_tpl else {
        b.diag("Константы: no query table template".into());
        return Ok(());
    };
    let t = super::QTable {
        name_ru: tpl.name_ru.clone(),
        name_en: tpl.name_en.clone(),
        kind: TableKind::Constants,
        object: None,
        owner: None,
        fields: Vec::new(),
        star: Vec::new(),
        params: Vec::new(),
        reference: None,
    };
    let id = b.add_table(t, &[])?;
    flush(b, id, all)
}

fn sequences(b: &mut Builder<'_>, commons: &[Common]) -> Result<(), SchemaError> {
    let cat = b.cat;
    let tpl = templates::find(cat, "Sequence.*", false);
    let bounds = templates::find(cat, "Sequence.*.Boundaries", false);
    for o in of_kind(b, MdKind::Sequence) {
        let owner = owner_name(o);
        let Some(table) = declare_main(b, o, tpl, TableKind::Object(o.kind))? else {
            continue;
        };
        let docs = match o.prop("Documents") {
            Some(Value::RefList(list)) => b.refs_of(list, &format!("{owner}.Documents")),
            _ => {
                b.diag(format!("{owner}: no Documents"));
                Vec::new()
            }
        };
        let mut meta = ObjMeta::new(o.kind);
        meta.dims = items(b, o, ChildTag::Dimension, &owner);
        let used: Vec<&Common> = commons.iter().filter(|c| c.used_by(o)).collect();
        meta.commons = used.iter().map(|c| c.name.clone()).collect();
        let recorder = refs_type(docs);
        let fill = |f: &mut Tf| {
            f.std(StdRole::Period, QType::date(DateQ::DateTime));
            f.std(StdRole::Recorder, recorder.clone());
            for d in &meta.dims {
                f.item(d, FieldRole::Dimension);
            }
            for c in &used {
                f.push(common_field(c), true);
            }
            f.virt(StdRole::PointInTime, QType::other(Other::PointInTime));
        };
        let mut f = Tf::new(o.kind, tpl);
        fill(&mut f);
        flush(b, table, f)?;
        match bounds {
            Some(bt) => {
                let mut f = Tf::new(o.kind, Some(bt));
                fill(&mut f);
                let kind = TableKind::Virtual(VtKind::SequenceBoundaries);
                let (t, extra) = sub_table(b, table, &templates::last_segment(bt), kind, None)?;
                let id = b.add_table(t, &extra)?;
                flush(b, id, f)?;
            }
            None => b.diag(format!("{owner}: no query table template Boundaries")),
        }
        b.meta.insert(table, meta);
    }
    Ok(())
}

fn filter_criteria(b: &mut Builder<'_>, objs: &ObjIndex<'_>) -> Result<(), SchemaError> {
    let tpl = templates::find(b.cat, "FilterCriterion.*", false);
    for o in of_kind(b, MdKind::FilterCriterion) {
        let owner = owner_name(o);
        let kind = TableKind::Virtual(VtKind::FilterCriterion);
        let Some(table) = declare_main(b, o, tpl, kind)? else {
            continue;
        };
        let content: &[MdRef] = match o.prop("Content") {
            Some(Value::RefList(list)) => list,
            _ => &[],
        };
        let mut owners: Vec<RefAtom> = Vec::new();
        for r in content {
            let atom = objs
                .get(r)
                .and_then(|x| b.by_object.get(&x.id))
                .and_then(|t| b.reference_of(*t));
            match atom {
                Some(a) => owners.push(a),
                None => b.diag(format!("{owner}.Content: unknown field {}", r.name)),
            }
        }
        owners.sort();
        owners.dedup();
        let mut f = Tf::new(o.kind, tpl);
        f.std(StdRole::Ref, refs_type(owners));
        flush(b, table, f)?;
        if let Some(tpl) = tpl {
            let params = match templates::params(tpl) {
                Ok(p) => p,
                Err(e) => {
                    b.diag(e);
                    Vec::new()
                }
            };
            if let Some(t) = b.table_mut(table) {
                t.params = params;
            }
        }
    }
    Ok(())
}

fn tasks_by_performer(b: &mut Builder<'_>) -> Result<(), SchemaError> {
    let Some(tpl) = templates::find(b.cat, "Task.*.TasksByPerformer", false) else {
        return Ok(());
    };
    for o in of_kind(b, MdKind::Task) {
        if let Some(Value::Ref(r)) = o.prop("Addressing")
            && r.name.is_empty()
            && r.id.is_none()
        {
            continue;
        }
        let Some(&task) = b.by_object.get(&o.id) else {
            continue;
        };
        let Some(src) = b.table_ref(task) else {
            continue;
        };
        let mut f = Tf::new(o.kind, Some(tpl));
        let star: HashSet<QFieldId> = src.star.iter().copied().collect();
        let main = match o.prop("MainAddressingAttribute") {
            Some(Value::Ref(r)) => o
                .children
                .iter()
                .find(|c| match r.id {
                    Some(id) => id == c.id,
                    None => r.name.rsplit('.').next() == Some(c.name.as_str()),
                })
                .map(|c| c.id),
            _ => None,
        };
        let mut ordered: Vec<_> = src.fields.iter().enumerate().collect();
        ordered.sort_by_key(|(_, f)| main.is_some() && f.md == main);
        for (i, x) in ordered {
            if x.role == FieldRole::Std(StdRole::DataVersion) {
                continue;
            }
            let in_star = u16::try_from(i).is_ok_and(|i| star.contains(&QFieldId(i)));
            f.push(x.clone(), in_star);
        }
        let kind = TableKind::Virtual(VtKind::TaskByPerformer);
        let (mut t, extra) = sub_table(b, task, &templates::last_segment(tpl), kind, None)?;
        t.params = templates::params(tpl).unwrap_or_default();
        let id = b.add_table(t, &extra)?;
        flush(b, id, f)?;
    }
    Ok(())
}

fn field_type(b: &Builder<'_>, t: QTableId, role: StdRole) -> QType {
    b.table_ref(t)
        .and_then(|x| x.fields.iter().find(|f| f.role == FieldRole::Std(role)))
        .map(|f| f.ty.clone())
        .unwrap_or_default()
}

fn changes(b: &mut Builder<'_>, objs: &ObjIndex<'_>) -> Result<(), SchemaError> {
    let mut nodes: HashMap<Guid, Vec<RefAtom>> = HashMap::new();
    for p in of_kind(b, MdKind::ExchangePlan) {
        let Some(atom) = b.by_object.get(&p.id).and_then(|t| b.reference_of(*t)) else {
            continue;
        };
        for item in p.exchange_content().unwrap_or_default() {
            if let Some(o) = objs.get(&item.object) {
                nodes.entry(o.id).or_default().push(atom);
            }
        }
    }
    if nodes.is_empty() {
        return Ok(());
    }
    let cfg = b.cfg;
    for o in &cfg.objects {
        let Some(plans) = nodes.get_mut(&o.id) else {
            continue;
        };
        plans.sort();
        plans.dedup();
        let Some(&main) = b.by_object.get(&o.id) else {
            continue;
        };
        let Some(tpl) = templates::find(b.cat, &format!("{}.*.Changes", o.kind.as_str()), false)
        else {
            b.diag(format!(
                "{}: no query table template Changes",
                owner_name(o)
            ));
            continue;
        };
        let mut f = Tf::new(o.kind, Some(tpl));
        f.std(StdRole::Node, refs_type(plans.clone()));
        f.std(StdRole::MessageNo, number(MESSAGE_NO_DIGITS, 0, false));
        let reference = b.reference_of(main);
        let meta = b.meta.get(&main).cloned();
        match (reference, o.kind, meta) {
            (Some(r), _, _) => f.std(StdRole::Ref, QType::reference(r)),
            (None, MdKind::Constant, _) => {}
            (None, MdKind::InformationRegister, Some(m)) if !m.subordinate => {
                if m.periodic && m.filter_on_period {
                    f.std(StdRole::Period, field_type(b, main, StdRole::Period));
                }
                for d in m.dims.iter().filter(|d| d.main_filter) {
                    f.item(d, FieldRole::Dimension);
                }
            }
            _ => f.std(StdRole::Recorder, field_type(b, main, StdRole::Recorder)),
        }
        let (t, extra) = sub_table(
            b,
            main,
            &templates::last_segment(tpl),
            TableKind::Changes,
            Some(o.id),
        )?;
        let id = b.add_table(t, &extra)?;
        flush(b, id, f)?;
    }
    Ok(())
}
