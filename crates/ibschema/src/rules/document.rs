use md_model::{Child, ChildTag, MdKind, MdRef};

use super::catalog::{attributes, indexed_attributes, main_table, tabular_sections};
use super::common::common_group;
use super::ctx::{Ctx, Props, tag_name};
use super::exchange;
use super::fields::{self, RefForm};
use super::in_object;
use super::index::{Tok, indexed};
use super::types::{
    Class, RefTarget, classes as classes_of, logical_types, main_target, ref_target,
};
use super::{Names, TableBuilder, TableRole};
use crate::error::{Error, Result};
use crate::logical::{KeyKind, LType, Table, TableKind, TypeCode};
use crate::names::{StdField, field as f, purpose, role};

pub(crate) fn document(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    let mut out = main(cx, names)?;
    out.extend(tabular_sections(cx, names)?);
    out.extend(exchange::change_records(
        cx,
        names,
        purpose::DOCUMENT_CHNGR,
        exchange::Key::Ref,
    )?);
    Ok(out)
}

pub(crate) fn main(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    use Tok::F;
    let mut b = main_table(cx, names)?;
    b.std(StdField::Version, fields::version());
    b.std(StdField::Marked, fields::marked());
    b.std(StdField::DateTime, fields::date_time());
    let (num, prefix) = (cx.num_len > 0, cx.periodic && cx.num_len > 0);
    if prefix {
        b.std(StdField::NumberPrefix, fields::number_prefix());
    }
    if num {
        b.std(StdField::Number, fields::number(cx)?);
    }
    b.std(StdField::Posted, fields::posted());
    attributes(&mut b, cx, names, false)?;
    common_group(&mut b, cx, names)?;

    if prefix {
        let toks = [F(f::NUMBER_PREFIX), F(f::NUMBER), F(f::ID)];
        b.rule(cx, names, "ByDocNumPrefix", true, &toks, None)?;
    }
    if num {
        b.rule(cx, names, "ByDocNum", true, &[F(f::NUMBER), F(f::ID)], None)?;
    }
    let toks = [F(f::DATE_TIME), F(f::ID), F(f::MARKED), Tok::AddOrderAll];
    b.rule(cx, names, "ByDocDate", true, &toks, None)?;
    let toks = [
        Tok::Attr,
        Tok::AddOrder(f::DATE_TIME),
        F(f::ID),
        Tok::AddOrder(f::MARKED),
    ];
    for a in indexed_attributes(cx) {
        b.rule(cx, names, "ByField{F}", true, &toks, Some(a))?;
    }
    Ok(vec![b.finish()])
}

pub(super) fn table_of(
    cx: &Ctx,
    names: &mut Names,
    purpose: &str,
    role: &str,
    key: KeyKind,
) -> Result<TableBuilder> {
    let n = names.number(cx.obj.id, purpose)?;
    let mut t = Table::new(format!("{purpose}{n}"), TableKind::Main, n, "");
    t.key = key;
    Ok(TableBuilder::new(
        t,
        role,
        cx.obj.id,
        cx.full_name.as_str(),
        cx.full_name.as_str(),
    ))
}

fn referenced_item<'a>(cx: &Ctx<'a>, r: &MdRef) -> Option<&'a Child> {
    let all = || cx.cfg.objects.iter().flat_map(|o| o.children.iter());
    match r.id {
        Some(id) => all().find(|c| c.id == id),
        None => {
            let mut it = r.name.split('.');
            let (kind, obj, tag, name) = (it.next()?, it.next()?, it.next()?, it.next()?);
            let o = cx.md.find(MdKind::from_name(kind)?, obj)?;
            o.children
                .iter()
                .find(|c| tag_name(c.tag) == tag && c.name == name)
        }
    }
}

pub(crate) fn journal(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    use Tok::F;
    let mut b = table_of(
        cx,
        names,
        purpose::DOCUMENT_JOURNAL,
        role::MAIN,
        KeyKind::Ref,
    )?;
    b.std(
        StdField::Document,
        fields::reference(f::DOCUMENT, &[], RefForm::T, false),
    );
    b.std(StdField::DateTime, fields::date_time());
    b.std(StdField::Marked, fields::marked());
    b.std(StdField::Posted, fields::posted());
    let mut classes: Vec<Class> = Vec::new();
    for d in &cx.docs {
        let dc = Ctx::new(cx.md, d)?;
        let c = match dc.number {
            Class::N { digits, .. } => Class::N {
                digits,
                fraction: 0,
            },
            other => other,
        };
        if !classes.contains(&c) {
            classes.push(c);
        }
    }
    if !classes.is_empty() {
        let t = logical_types(&classes, false).map_err(|e| in_object(&cx.full_name, e))?;
        b.std(StdField::Number, fields::field(f::NUMBER, false, t));
    }
    let columns: Vec<&Child> = cx
        .obj
        .children
        .iter()
        .filter(|c| c.tag == ChildTag::Column)
        .collect();
    for col in &columns {
        let at = cx.child_name(col);
        let rs = col.refs("References");
        let cls = union_classes(cx, rs, &at, names)?;
        let types = if cls.is_empty() {
            vec![LType::new(TypeCode::E, 0, 0)]
        } else {
            logical_types(&cls, false).map_err(|e| in_object(&at, e))?
        };
        let name = fields::item_name(col, names)?;
        let null = rs.len() < cx.docs.len();
        b.item(fields::field(name, null, types), col.id, at);
    }

    b.rule(cx, names, "ByDoc", true, &[F(f::DOCUMENT)], None)?;
    let toks = [F(f::DATE_TIME), F(f::DOCUMENT), F(f::MARKED)];
    b.rule(cx, names, "ByDocDate", true, &toks, None)?;
    for col in columns.iter().filter(|c| indexed(c)) {
        let toks = [Tok::Attr, F(f::DOCUMENT)];
        b.rule(cx, names, "ByField{n}", true, &toks, Some(col))?;
    }
    Ok(vec![b.finish()])
}

fn union_classes(cx: &Ctx, rs: &[MdRef], at: &str, names: &mut Names) -> Result<Vec<Class>> {
    let mut cls: Vec<Class> = Vec::new();
    for r in rs {
        let item = referenced_item(cx, r).ok_or_else(|| {
            in_object(
                at,
                Error::Unsupported(format!("reference {} is unknown", r.name)),
            )
        })?;
        let td = item
            .type_desc()
            .ok_or_else(|| Error::Unsupported(format!("{at}: referenced item has no type")))?;
        for c in classes_of(cx, td, at, names)? {
            if !cls.contains(&c) {
                cls.push(c);
            }
        }
    }
    Ok(cls)
}

fn dimensions(b: &mut TableBuilder, cx: &Ctx, names: &mut Names) -> Result<()> {
    for d in cx.obj.dimensions() {
        let at = cx.child_name(d);
        let fl = match (d.type_desc(), d.refs("DocumentMap")) {
            (None, map) if !map.is_empty() => {
                let cls = union_classes(cx, map, &at, names)?;
                let t = logical_types(&cls, false).map_err(|e| in_object(&at, e))?;
                fields::field(fields::item_name(d, names)?, false, t)
            }
            _ => fields::item(cx, d, &at, false, false, names)?,
        };
        b.item(fl, d.id, at);
    }
    Ok(())
}

pub(crate) fn sequence(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    use Tok::F;
    let targets = cx
        .docs
        .iter()
        .map(|d| main_target(d, names))
        .collect::<Result<Vec<_>>>()?;
    let mut b = table_of(cx, names, purpose::SEQ, role::MAIN, KeyKind::Plain)?;
    b.std(StdField::Period, fields::period());
    b.std(
        StdField::Recorder,
        fields::reference(f::RECORDER, &targets, RefForm::T, false),
    );
    dimensions(&mut b, cx, names)?;
    b.rule(cx, names, "ByRecorder", !cx.dims, &[F(f::RECORDER)], None)?;
    let toks = [Tok::Dims, F(f::PERIOD), F(f::RECORDER)];
    b.rule(cx, names, "ByDims", true, &toks, None)?;

    let mut bb = table_of(
        cx,
        names,
        purpose::SEQ_B,
        role::SEQUENCE_BOUNDARIES,
        KeyKind::Plain,
    )?;
    bb.role.query_name = format!("{}.Boundaries", cx.full_name);
    dimensions(&mut bb, cx, names)?;
    bb.std(StdField::Period, fields::period());
    bb.std(
        StdField::Recorder,
        fields::reference(f::RECORDER, &[], RefForm::E, false),
    );
    if cx.dims {
        bb.rule(cx, names, "ByDims", true, &[Tok::Dims], None)?;
    }
    Ok(vec![b.finish(), bb.finish()])
}

fn process_targets(cx: &Ctx, names: &mut Names, purpose: &str) -> Result<Vec<RefTarget>> {
    cx.bps
        .iter()
        .map(|b| ref_target(b, purpose, names))
        .collect()
}

pub(crate) fn business_process(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    use Tok::F;
    let tasks = cx
        .md
        .resolve_all(cx.obj.refs("Task"), &format!("{}.Task", cx.full_name))?;
    let tasks = tasks
        .iter()
        .map(|t| main_target(t, names))
        .collect::<Result<Vec<_>>>()?;
    let mut b = main_table(cx, names)?;
    b.std(StdField::Version, fields::version());
    b.std(StdField::Marked, fields::marked());
    b.std(StdField::DateTime, fields::date_time());
    b.std(StdField::Completed, fields::boolean(f::COMPLETED));
    if cx.num_len > 0 {
        b.std(StdField::Number, fields::number(cx)?);
    }
    b.std(
        StdField::HeadTask,
        fields::reference(f::HEAD_TASK, &tasks, RefForm::T, false),
    );
    b.std(StdField::Started, fields::boolean(f::STARTED));
    attributes(&mut b, cx, names, false)?;
    common_group(&mut b, cx, names)?;

    if cx.num_len > 0 {
        b.rule(cx, names, "ByBPNum", true, &[F(f::NUMBER), F(f::ID)], None)?;
    }
    b.rule(
        cx,
        names,
        "ByBPDate",
        true,
        &[F(f::DATE_TIME), F(f::ID)],
        None,
    )?;
    let toks = [F(f::COMPLETED), F(f::DATE_TIME), F(f::ID)];
    b.rule(cx, names, "ByCompleted", true, &toks, None)?;
    b.rule(
        cx,
        names,
        "ByHeadTask",
        true,
        &[F(f::HEAD_TASK), F(f::ID)],
        None,
    )?;
    let toks = [F(f::STARTED), F(f::DATE_TIME), F(f::ID)];
    b.rule(cx, names, "ByStarted", true, &toks, None)?;
    for a in indexed_attributes(cx) {
        b.rule(
            cx,
            names,
            "ByField{n}",
            true,
            &[Tok::Attr, F(f::ID)],
            Some(a),
        )?;
    }
    let mut out = vec![b.finish()];

    let mut p = table_of(
        cx,
        names,
        purpose::BPR_POINTS,
        role::ROUTE_POINTS,
        KeyKind::Ref,
    )?;
    p.role.query_name = format!("{}.Points", cx.full_name);
    let own = p.table.name.clone();
    p.std(StdField::Id, fields::id(&own));
    p.std(
        StdField::RoutePointOrder,
        fields::numeric(f::ROUTE_POINT_ORDER, 10, 0),
    );
    p.rule(
        cx,
        names,
        "ByOrder",
        true,
        &[F(f::ROUTE_POINT_ORDER), F(f::ID)],
        None,
    )?;
    out.push(p.finish());
    out.extend(tabular_sections(cx, names)?);
    out.extend(exchange::change_records(
        cx,
        names,
        purpose::BPR_CHNGR,
        exchange::Key::Ref,
    )?);
    Ok(out)
}

pub(crate) fn task(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    use Tok::F;
    let procs = process_targets(cx, names, purpose::BPR)?;
    let points = process_targets(cx, names, purpose::BPR_POINTS)?;
    let mut b = main_table(cx, names)?;
    b.std(StdField::Version, fields::version());
    b.std(StdField::Marked, fields::marked());
    b.std(
        StdField::BusinessProcess,
        fields::reference(f::BUSINESS_PROCESS, &procs, RefForm::T, false),
    );
    b.std(
        StdField::Point,
        fields::reference(f::POINT, &points, RefForm::T, false),
    );
    b.std(StdField::DateTime, fields::date_time());
    if cx.num_len > 0 {
        b.std(StdField::Number, fields::number(cx)?);
    }
    if cx.descr_len > 0 {
        b.std(StdField::Name, fields::string(f::NAME, cx.descr_len));
    }
    b.std(StdField::Executed, fields::boolean(f::EXECUTED));
    attributes(&mut b, cx, names, false)?;
    for a in cx
        .obj
        .children
        .iter()
        .filter(|c| c.tag == ChildTag::AddressingAttribute)
    {
        let at = cx.child_name(a);
        let fl = fields::item(cx, a, &at, false, false, names)?;
        b.item(fl, a.id, at);
    }
    common_group(&mut b, cx, names)?;

    if cx.num_len > 0 {
        b.rule(
            cx,
            names,
            "ByTaskNum",
            true,
            &[F(f::NUMBER), F(f::ID)],
            None,
        )?;
    }
    b.rule(
        cx,
        names,
        "ByTaskDate",
        true,
        &[F(f::DATE_TIME), F(f::ID)],
        None,
    )?;
    let toks = [F(f::EXECUTED), F(f::DATE_TIME), F(f::ID)];
    b.rule(cx, names, "ByExecTaskDate", true, &toks, None)?;
    let toks = [F(f::BUSINESS_PROCESS), F(f::POINT), F(f::ID)];
    b.rule(cx, names, "ByProcPoint", true, &toks, None)?;
    let toks = [
        F(f::EXECUTED),
        F(f::BUSINESS_PROCESS),
        F(f::POINT),
        F(f::ID),
    ];
    b.rule(cx, names, "ByProcPointExec", true, &toks, None)?;
    let toks = [F(f::BUSINESS_PROCESS), F(f::DATE_TIME), F(f::ID)];
    b.rule(cx, names, "ByProcDate", true, &toks, None)?;
    if cx.descr_len > 0 {
        b.rule(cx, names, "ByName", true, &[F(f::NAME), F(f::ID)], None)?;
        let toks = [F(f::EXECUTED), F(f::NAME), F(f::ID)];
        b.rule(cx, names, "ByNameExec", true, &toks, None)?;
    }
    for a in indexed_attributes(cx) {
        b.rule(
            cx,
            names,
            "ByField{n}",
            true,
            &[Tok::Attr, F(f::ID)],
            Some(a),
        )?;
    }
    if cx.addressing {
        b.rule(
            cx,
            names,
            "ByAddrProps",
            true,
            &[Tok::Addressing, F(f::ID)],
            None,
        )?;
    }
    let mut out = vec![b.finish()];
    out.extend(tabular_sections(cx, names)?);
    out.extend(exchange::change_records(
        cx,
        names,
        purpose::TASK_CHNGR,
        exchange::Key::Ref,
    )?);
    Ok(out)
}
