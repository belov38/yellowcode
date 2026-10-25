use md_model::{Child, ChildTag, Value};

use super::common::{self, common_group, separators_group};
use super::ctx::{Ctx, Props, md_name};
use super::exchange;
use super::fields::{self, RefForm};
use super::index::{Tok, add_order, indexed};
use super::types::{classes, logical_types, main_target};
use super::{Names, TableBuilder, TableRole};
use crate::error::{Error, Result};
use crate::logical::{KeyKind, Table, TableKind};
use crate::names::{StdField, field as f, purpose, role};
use crate::values::Uuid16;

pub(super) fn main_table(cx: &Ctx, names: &mut Names) -> Result<TableBuilder> {
    let own = main_target(cx.obj, names)?;
    let mut t = Table::new(own.table.as_str(), TableKind::Main, own.number, "");
    t.key = KeyKind::Ref;
    let mut b = TableBuilder::new(
        t,
        role::MAIN,
        cx.obj.id,
        cx.full_name.as_str(),
        cx.full_name.as_str(),
    );
    b.std(StdField::Id, fields::id(&own.table));
    Ok(b)
}

pub(super) fn attributes(
    b: &mut TableBuilder,
    cx: &Ctx,
    names: &mut Names,
    folders: bool,
) -> Result<()> {
    for a in cx.obj.attributes() {
        let null = folders && a.text("Use").unwrap_or("ForItem") != "ForFolderAndItem";
        let at = cx.child_name(a);
        let fl = fields::item(cx, a, &at, null, false, names)?;
        b.item(fl, a.id, at);
    }
    Ok(())
}

pub(super) fn indexed_attributes<'a>(cx: &Ctx<'a>) -> Vec<&'a Child> {
    cx.obj.attributes().filter(|a| indexed(a)).collect()
}

fn reference_head(b: &mut TableBuilder) {
    b.std(StdField::Version, fields::version());
    b.std(StdField::Marked, fields::marked());
    b.std(StdField::PredefinedId, fields::predefined_id());
}

fn code_and_description(b: &mut TableBuilder, cx: &Ctx) -> Result<()> {
    if cx.code_len > 0 {
        b.std(StdField::Code, fields::code(cx)?);
    }
    if cx.descr_len > 0 {
        b.std(StdField::Description, fields::description(cx.descr_len));
    }
    Ok(())
}

pub(crate) fn catalog(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    use Tok::F;
    let mut b = main_table(cx, names)?;
    let own = b.table.name.clone();
    reference_head(&mut b);
    if !cx.owners.is_empty() {
        let targets = cx
            .owners
            .iter()
            .map(|o| main_target(o, names))
            .collect::<Result<Vec<_>>>()?;
        b.std(
            StdField::OwnerId,
            fields::reference(f::OWNER_ID, &targets, RefForm::E, false),
        );
    }
    if cx.hier {
        b.std(StdField::ParentId, fields::parent_id(&own));
    }
    if cx.folders {
        b.std(StdField::Folder, fields::folder());
    }
    code_and_description(&mut b, cx)?;
    attributes(&mut b, cx, names, cx.folders)?;
    common_group(&mut b, cx, names)?;

    let (owners, code, descr) = (!cx.owners.is_empty(), cx.code_len > 0, cx.descr_len > 0);
    b.rule(
        cx,
        names,
        "ByPredefinedIDNotUniq",
        false,
        &[F(f::PREDEFINED_ID)],
        None,
    )?;
    if owners && code {
        let toks = [F(f::OWNER_ID), F(f::CODE), F(f::ID)];
        b.rule(cx, names, "OwnerCode", true, &toks, None)?;
    }
    if owners && descr {
        let toks = [F(f::OWNER_ID), F(f::DESCRIPTION), F(f::ID)];
        b.rule(cx, names, "OwnerDescr", true, &toks, None)?;
    }
    let parent = |last| {
        [
            F(f::OWNER_ID),
            F(f::PARENT_ID),
            F(f::FOLDER),
            F(last),
            F(f::ID),
        ]
    };
    if cx.hier && code {
        b.rule(cx, names, "ParentCode", true, &parent(f::CODE), None)?;
    }
    if cx.hier && descr {
        let toks = parent(f::DESCRIPTION);
        b.rule(cx, names, "ParentDescr", true, &toks, None)?;
    }
    if code {
        b.rule(cx, names, "Code", true, &[F(f::CODE), F(f::ID)], None)?;
    }
    if descr {
        let toks = [F(f::DESCRIPTION), F(f::ID)];
        b.rule(cx, names, "Descr", true, &toks, None)?;
    }
    field_indexes(&mut b, cx, names, false)?;

    let mut out = vec![b.finish()];
    out.extend(sections(cx, names, &own)?);
    out.extend(exchange::predefined_init(
        cx,
        names,
        purpose::REF_SINF,
        role::INITIALIZED_PREDEFINED_DATA_IN_CATALOG,
    )?);
    out.extend(exchange::change_records(
        cx,
        names,
        purpose::REFERENCE_CHNGR,
        exchange::Key::Ref,
    )?);
    Ok(out)
}

fn field_indexes(b: &mut TableBuilder, cx: &Ctx, names: &mut Names, numbered: bool) -> Result<()> {
    let ao_attrs: Vec<(String, String)> = indexed_attributes(cx)
        .into_iter()
        .filter(|a| add_order(a))
        .map(|a| Ok((a.name.clone(), fields::item_name(a, names)?)))
        .collect::<Result<_>>()?;
    let mut items: Vec<(String, bool, Option<String>, Uuid16)> = Vec::new();
    for a in indexed_attributes(cx) {
        items.push((
            fields::item_name(a, names)?,
            add_order(a),
            Some(a.name.clone()),
            a.id,
        ));
    }
    for ca in common::applied(cx) {
        if matches!(
            ca.text("Indexing"),
            Some("Index" | "IndexWithAdditionalOrder")
        ) {
            let n = names.number(ca.id, purpose::FLD)?;
            items.push((
                format!("{}{n}", purpose::FLD),
                ca.text("Indexing") == Some("IndexWithAdditionalOrder"),
                None,
                ca.id,
            ));
        }
    }
    let present = if cx.descr_len > 0 {
        Some(f::DESCRIPTION)
    } else if cx.code_len > 0 {
        Some(f::CODE)
    } else {
        None
    };
    let owner = (!cx.owners.is_empty()).then(|| f::OWNER_ID.to_string());
    for (field, ao, own_name, id) in items {
        let mut name = |pre: &str| -> Result<String> {
            Ok(if numbered {
                format!("{pre}{}", names.number(id, pre)?)
            } else {
                format!("{pre}{field}")
            })
        };
        let mut tail = vec![field.clone()];
        if ao {
            tail.extend(present.map(str::to_string));
        }
        tail.push(f::ID.to_string());
        if ao {
            tail.push(f::MARKED.to_string());
            for (name, fld) in &ao_attrs {
                if own_name.as_deref() != Some(name.as_str()) {
                    tail.push(fld.clone());
                }
            }
        }
        if let Some(o) = &owner {
            let fields = [vec![o.clone()], tail.clone()].concat();
            b.rule_fields(cx, name("ByOwnerField")?, true, fields)?;
        }
        if cx.hier {
            let mut fields: Vec<String> = owner.iter().cloned().collect();
            fields.push(f::PARENT_ID.to_string());
            if b.table.field(f::FOLDER).is_some() {
                fields.push(f::FOLDER.to_string());
            }
            fields.extend(tail.clone());
            b.rule_fields(cx, name("ByParentField")?, true, fields)?;
        }
        b.rule_fields(cx, name("ByField")?, true, tail)?;
    }
    Ok(())
}

pub(crate) fn chrc(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    use Tok::F;
    let mut b = main_table(cx, names)?;
    let own = b.table.name.clone();
    reference_head(&mut b);
    if cx.hier {
        b.std(StdField::ParentId, fields::parent_id(&own));
        b.std(StdField::Folder, fields::folder());
    }
    code_and_description(&mut b, cx)?;
    b.std(StdField::Type, fields::value_type());
    attributes(&mut b, cx, names, cx.hier)?;
    common_group(&mut b, cx, names)?;

    let (code, descr) = (cx.code_len > 0, cx.descr_len > 0);
    b.rule(
        cx,
        names,
        "ByPredefinedIDNotUniq",
        false,
        &[F(f::PREDEFINED_ID)],
        None,
    )?;
    if code {
        b.rule(cx, names, "ByCode", true, &[F(f::CODE), F(f::ID)], None)?;
    }
    if descr {
        let toks = [F(f::DESCRIPTION), F(f::ID)];
        b.rule(cx, names, "ByDescr", true, &toks, None)?;
    }
    let parent = |last| [F(f::PARENT_ID), F(f::FOLDER), F(last), F(f::ID)];
    if cx.hier && code {
        b.rule(cx, names, "ByParentCode", true, &parent(f::CODE), None)?;
    }
    if cx.hier && descr {
        let toks = parent(f::DESCRIPTION);
        b.rule(cx, names, "ByParentDescr", true, &toks, None)?;
    }
    field_indexes(&mut b, cx, names, true)?;

    let mut out = vec![b.finish()];
    out.extend(sections(cx, names, &own)?);
    out.extend(exchange::predefined_init(
        cx,
        names,
        purpose::CHRC_SINF,
        role::INITIALIZED_PREDEFINED_DATA_IN_CHART_OF_CHARACTERISTIC_TYPES,
    )?);
    out.extend(exchange::change_records(
        cx,
        names,
        purpose::CHRC_CHNGR,
        exchange::Key::Ref,
    )?);
    Ok(out)
}

pub(crate) fn constant(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    let n = names.number(cx.obj.id, purpose::CONST)?;
    let t = Table::new(format!("{}{n}", purpose::CONST), TableKind::Main, n, "");
    let mut b = TableBuilder::new(
        t,
        role::CONSTANT,
        cx.obj.id,
        cx.full_name.as_str(),
        cx.full_name.as_str(),
    );
    let Some(Value::Type(td)) = cx.obj.value("Type") else {
        return Err(Error::Unsupported(format!("{}: no type", cx.full_name)));
    };
    let cl = classes(cx, td, &cx.full_name, names)?;
    let types = logical_types(&cl, false)?;
    let name = format!("{}{}", purpose::FLD, names.number(cx.obj.id, purpose::FLD)?);
    b.item(
        fields::field(name, false, types),
        cx.obj.id,
        cx.full_name.as_str(),
    );
    separators_group(&mut b, cx, names)?;
    b.std(StdField::RecordKey, fields::binary(f::RECORD_KEY, 1));
    b.rule(cx, names, "ByKey", true, &[Tok::F(f::RECORD_KEY)], None)?;
    let mut out = vec![b.finish()];
    out.extend(exchange::change_records(
        cx,
        names,
        purpose::CONST_CHNGR,
        exchange::Key::Constant,
    )?);
    Ok(out)
}

pub(crate) fn enumeration(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    let mut b = main_table(cx, names)?;
    b.std(StdField::EnumOrder, fields::enum_order());
    let toks = [Tok::F(f::ENUM_ORDER), Tok::F(f::ID)];
    b.rule(cx, names, "ByOrder", true, &toks, None)?;
    Ok(vec![b.finish()])
}

pub(crate) fn tabular_sections(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    let own = main_target(cx.obj, names)?;
    sections(cx, names, &own.table)
}

fn sections(cx: &Ctx, names: &mut Names, owner: &str) -> Result<Vec<(Table, TableRole)>> {
    let mut out = Vec::new();
    for ts in cx.obj.tabular_sections() {
        let n = names.number(ts.id, purpose::VT)?;
        let m = names.number(ts.id, purpose::LINE_NO)?;
        let t = Table::new(format!("{}{n}", purpose::VT), TableKind::Nested, 0, owner);
        let ts_name = cx.child_name(ts);
        let query = format!("{}.{}", cx.full_name, ts.name);
        let mut b = TableBuilder::new(t, role::TABULAR_SECTION, ts.id, ts_name.as_str(), query);
        let digits = match ts.text("LineNumberLength") {
            None => 9,
            Some(v) => v
                .parse::<u32>()
                .ok()
                .filter(|d| (1..=9).contains(d))
                .ok_or_else(|| {
                    Error::Unsupported(format!(
                        "{}.{}.LineNumberLength: {v:?} is out of range (1..9)",
                        cx.full_name, ts.name
                    ))
                })?,
        };
        b.std(StdField::LineNo, fields::line_no(Some(m), digits));
        let attrs: Vec<&Child> = ts
            .children
            .iter()
            .filter(|c| c.tag == ChildTag::Attribute)
            .collect();
        for a in &attrs {
            let at = md_name(&ts_name, a);
            let fl = fields::item(cx, a, &at, false, false, names)?;
            b.item(fl, a.id, at);
        }
        for a in attrs.iter().filter(|a| indexed(a)) {
            b.rule(
                cx,
                names,
                "ByField{F}",
                false,
                &[Tok::Attr, Tok::Owner],
                Some(a),
            )?;
        }
        out.push(b.finish());
    }
    Ok(out)
}
