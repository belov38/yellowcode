use md_model::{Child, ChildTag, MdKind, MdObject, Recalculation};

use super::catalog::{attributes, indexed_attributes, main_table, tabular_sections};
use super::common::common_group;
use super::ctx::{Ctx, Props, tag_name};
use super::document::table_of;
use super::exchange;
use super::fields::{self, RefForm};
use super::index::Tok;
use super::registers::{Group, group, indexed_attrs_res, indexed_dims};
use super::types::{RefTarget, classes, logical_types, main_target};
use super::{Names, TableBuilder, TableRole, in_object};
use crate::error::{Error, Result};
use crate::logical::{Field, KeyKind, Table, TableKind};
use crate::names::{StdField, field as f, purpose, role};

pub(crate) fn chart_of_calculation_types(
    cx: &Ctx,
    names: &mut Names,
) -> Result<Vec<(Table, TableRole)>> {
    use Tok::F;
    let mut b = main_table(cx, names)?;
    let own = b.table.name.clone();
    b.std(StdField::Version, fields::version());
    b.std(StdField::Marked, fields::marked());
    b.std(StdField::PredefinedId, fields::predefined_id());
    if cx.code_len > 0 {
        b.std(StdField::Code, fields::code(cx)?);
    }
    if cx.descr_len > 0 {
        b.std(StdField::Description, fields::description(cx.descr_len));
    }
    if cx.apu {
        b.std(StdField::BaseAsAct, fields::boolean(f::BASE_AS_ACT));
    }
    attributes(&mut b, cx, names, false)?;
    common_group(&mut b, cx, names)?;

    let pid = [F(f::PREDEFINED_ID)];
    b.rule(cx, names, "ByPredefinedIDNotUniq", false, &pid, None)?;
    if cx.code_len > 0 {
        b.rule(cx, names, "Code", true, &[F(f::CODE), F(f::ID)], None)?;
    }
    if cx.descr_len > 0 {
        let toks = [F(f::DESCRIPTION), F(f::ID)];
        b.rule(cx, names, "Descr", true, &toks, None)?;
    }
    for a in indexed_attributes(cx) {
        let toks = [Tok::Attr, F(f::ID)];
        b.rule(cx, names, "ByField{n}", true, &toks, Some(a))?;
    }
    let mut out = vec![b.finish()];

    if cx.dep {
        let targets = charts(&cx.base_charts, names)?;
        out.push(list(cx, names, &own, &BASE, &targets, RefForm::E)?);
    }
    out.push(list(cx, names, &own, &LEADING, &[], RefForm::E)?);
    if cx.apu {
        let me = [main_target(cx.obj, names)?];
        out.push(list(cx, names, &own, &DISPLACED, &me, RefForm::T)?);
    }
    out.extend(tabular_sections(cx, names)?);
    if cx.apu {
        out.push(displacement_order(cx, names)?);
    }
    out.extend(exchange::predefined_init(
        cx,
        names,
        purpose::CKINDS_SINF,
        role::INITIALIZED_PREDEFINED_DATA_IN_CHART_OF_CALCULATION_TYPES,
    )?);
    out.extend(exchange::change_records(
        cx,
        names,
        purpose::CKINDS_CHNGR,
        exchange::Key::Ref,
    )?);
    Ok(out)
}

fn charts(list: &[&MdObject], names: &mut Names) -> Result<Vec<RefTarget>> {
    list.iter().map(|c| main_target(c, names)).collect()
}

struct List {
    table: &'static str,
    md: &'static str,
    line_no: (StdField, &'static str),
    reference: (StdField, &'static str),
    predefined: (StdField, &'static str),
    index: &'static str,
}

const BASE: List = List {
    table: purpose::BASE_CK,
    md: "BaseCalculationTypes",
    line_no: (StdField::BaseCkLineNo, f::BASE_CK_LINE_NO),
    reference: (StdField::BaseCkBaseCk, f::BASE_CK_BASE_CK),
    predefined: (
        StdField::PredefinedBaseCkTableLine,
        f::PREDEFINED_BASE_CK_TABLE_LINE,
    ),
    index: "ByBaseCKBaseCK",
};
const LEADING: List = List {
    table: purpose::LEADING_CK,
    md: "LeadingCalculationTypes",
    line_no: (StdField::LeadingCkLineNo, f::LEADING_CK_LINE_NO),
    reference: (StdField::LeadingCkLeadingCk, f::LEADING_CK_LEADING_CK),
    predefined: (
        StdField::PredefinedLeadingCkTableLine,
        f::PREDEFINED_LEADING_CK_TABLE_LINE,
    ),
    index: "ByLeadCKLeadCK",
};
const DISPLACED: List = List {
    table: purpose::DISPLACED_CK,
    md: "DisplacingCalculationTypes",
    line_no: (StdField::DisplacedCkLineNo, f::DISPLACED_CK_LINE_NO),
    reference: (StdField::DisplacedCkDisplCk, f::DISPLACED_CK_DISPL_CK),
    predefined: (
        StdField::PredefinedDisplCkTableLine,
        f::PREDEFINED_DISPL_CK_TABLE_LINE,
    ),
    index: "ByDispCKDispCK",
};

fn list(
    cx: &Ctx,
    names: &mut Names,
    owner: &str,
    l: &List,
    targets: &[RefTarget],
    form: RefForm,
) -> Result<(Table, TableRole)> {
    let t = Table::new(l.table, TableKind::Nested, 0, owner);
    let md = format!("{}.TabularSection.{}", cx.full_name, l.md);
    let query = format!("{}.{}", cx.full_name, l.md);
    let mut b = TableBuilder::new(t, role::TABULAR_SECTION, cx.obj.id, md, query);
    b.std(l.line_no.0, fields::numeric(l.line_no.1, 5, 0));
    b.std(
        l.reference.0,
        fields::reference(l.reference.1, targets, form, false),
    );
    b.std(l.predefined.0, fields::boolean(l.predefined.1));
    b.rule(cx, names, l.index, false, &[Tok::F(l.reference.1)], None)?;
    Ok(b.finish())
}

fn displacement_order(cx: &Ctx, names: &mut Names) -> Result<(Table, TableRole)> {
    let mut b = table_of(
        cx,
        names,
        purpose::CKINDS_DN,
        role::DISPLACEMENT_ORDER,
        KeyKind::Plain,
    )?;
    b.role.query_name = String::new();
    let me = [main_target(cx.obj, names)?];
    b.std(
        StdField::CkRef,
        fields::reference(f::CK_REF, &me, RefForm::T, false),
    );
    b.std(
        StdField::DisplNumber,
        fields::numeric(f::DISPL_NUMBER, 10, 0),
    );
    b.rule(cx, names, "ByRef", true, &[Tok::F(f::CK_REF)], None)?;
    let toks = [Tok::F(f::DISPL_NUMBER)];
    b.rule(cx, names, "ByDisplNumber", false, &toks, None)?;
    Ok(b.finish())
}

fn register_chart<'a>(cx: &Ctx<'a>) -> Result<&'a MdObject> {
    let at = format!("{}.ChartOfCalculationTypes", cx.full_name);
    let r = cx
        .obj
        .refs("ChartOfCalculationTypes")
        .first()
        .ok_or_else(|| Error::Unsupported(format!("{at}: not set")))?;
    let o = cx
        .md
        .resolve(r)
        .ok_or_else(|| Error::Unsupported(format!("{at}: {} is not found", r.name)))?;
    if o.kind != MdKind::ChartOfCalculationTypes {
        return Err(Error::Unsupported(format!(
            "{at}: {} is not a chart of calculation types",
            r.name
        )));
    }
    Ok(o)
}

pub(crate) fn register(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    use Tok::F;
    let chart = register_chart(cx)?;
    let mut b = table_of(cx, names, purpose::CRG, role::MAIN, KeyKind::Plain)?;
    b.std(StdField::Period, fields::period());
    b.std(StdField::Recorder, fields::recorder(cx, names)?);
    b.std(StdField::LineNo, fields::line_no(None, 9));
    let kind = [main_target(chart, names)?];
    b.std(
        StdField::CalcKind,
        fields::reference(f::CALC_KIND, &kind, RefForm::T, false),
    );
    if cx.action {
        b.std(StdField::ActionPeriod, fields::datetime(f::ACTION_PERIOD));
        b.std(StdField::ApDateFrom, fields::datetime(f::AP_DATE_FROM));
        b.std(StdField::ApDateTill, fields::datetime(f::AP_DATE_TILL));
    }
    if cx.base {
        b.std(StdField::BpDateFrom, fields::datetime(f::BP_DATE_FROM));
        b.std(StdField::BpDateTill, fields::datetime(f::BP_DATE_TILL));
    }
    b.std(StdField::Active, fields::active());
    b.std(StdField::Storno, fields::boolean(f::STORNO));
    group(&mut b, cx, names, Group::Dims)?;
    group(&mut b, cx, names, Group::Resources)?;
    group(&mut b, cx, names, Group::Attrs)?;
    common_group(&mut b, cx, names)?;

    let (p, rec, ln) = (F(f::PERIOD), F(f::RECORDER), F(f::LINE_NO));
    let act = F(f::ACTION_PERIOD);
    b.rule(
        cx,
        names,
        "ByPeriodDim",
        false,
        &[p.clone(), Tok::BaseDims],
        None,
    )?;
    if cx.action {
        let toks = [act.clone(), Tok::BaseDims];
        b.rule(cx, names, "ByActPDim", false, &toks, None)?;
    }
    b.rule(
        cx,
        names,
        "ByRecorder",
        true,
        &[rec.clone(), ln.clone()],
        None,
    )?;
    let toks = [p.clone(), rec.clone(), ln.clone()];
    b.rule(cx, names, "ByPRecorder", true, &toks, None)?;
    if cx.base_dims {
        let toks = [Tok::BaseDims, p.clone()];
        b.rule(cx, names, "ByDimPeriod", false, &toks, None)?;
    }
    if cx.base_dims && cx.action {
        let toks = [Tok::BaseDims, act];
        b.rule(cx, names, "ByDimActP", false, &toks, None)?;
    }
    let toks = [Tok::Attr, p, rec, ln];
    for d in indexed_dims(cx) {
        b.rule(cx, names, "ByDimRecorder{n}", false, &toks, Some(d))?;
    }
    for a in indexed_attrs_res(cx) {
        b.rule(cx, names, "ByPropRecorder{n}", false, &toks, Some(a))?;
    }
    let mut out = vec![b.finish()];

    if cx.action {
        out.push(action_periods(cx, names)?);
    }
    for rc in &cx.obj.recalculations {
        out.push(recalculation(cx, names, rc)?);
    }
    out.extend(exchange::change_records(
        cx,
        names,
        purpose::CRG_CHNGR,
        exchange::Key::Recorder,
    )?);
    Ok(out)
}

fn action_periods(cx: &Ctx, names: &mut Names) -> Result<(Table, TableRole)> {
    use Tok::F;
    let mut b = table_of(
        cx,
        names,
        purpose::CRG_ACT_P,
        role::ACTION_PERIODS,
        KeyKind::Plain,
    )?;
    b.role.query_name = " ".to_string();
    b.std(StdField::Recorder, fields::recorder(cx, names)?);
    b.std(StdField::LineNo, fields::line_no(None, 9));
    b.std(StdField::ApDateFrom, fields::datetime(f::AP_DATE_FROM));
    b.std(StdField::ApDateTill, fields::datetime(f::AP_DATE_TILL));
    let toks = [
        F(f::RECORDER),
        F(f::LINE_NO),
        F(f::AP_DATE_FROM),
        F(f::AP_DATE_TILL),
    ];
    b.rule(cx, names, "ByRecorder", false, &toks, None)?;
    Ok(b.finish())
}

fn register_dimension<'a>(cx: &Ctx<'a>, r: &md_model::MdRef) -> Option<&'a Child> {
    let dims = || cx.obj.dimensions();
    match r.id {
        Some(id) => dims().find(|d| d.id == id),
        None => {
            let prefix = format!("{}.{}.", cx.full_name, tag_name(ChildTag::Dimension));
            let name = r.name.strip_prefix(&prefix)?;
            dims().find(|d| d.name == name)
        }
    }
}

fn recalculation(cx: &Ctx, names: &mut Names, rc: &Recalculation) -> Result<(Table, TableRole)> {
    let n = names.number(rc.id, purpose::CRG_RECALC)?;
    let mut t = Table::new(
        format!("{}{n}", purpose::CRG_RECALC),
        TableKind::Main,
        n,
        "",
    );
    t.key = KeyKind::Plain;
    let md = format!("{}.Recalculation.{}", cx.full_name, rc.name);
    let query = format!("{}.{}", cx.full_name, rc.name);
    let mut b = TableBuilder::new(t, role::RECALCULATION, rc.id, md.as_str(), query);
    b.std(
        StdField::Recorder,
        fields::reference(f::RECORDER, &[], RefForm::T, false),
    );
    let kind = [main_target(register_chart(cx)?, names)?];
    b.std(
        StdField::CalcKind,
        fields::reference(f::CALC_KIND, &kind, RefForm::T, false),
    );
    let mut dims = Vec::new();
    for d in &rc.dimensions {
        let at = md_name_of(&md, d);
        let rd = register_dimension(cx, &d.register_dimension).ok_or_else(|| {
            Error::Unsupported(format!(
                "{at}: register dimension {} is not found",
                d.register_dimension.name
            ))
        })?;
        let td = rd.type_desc().ok_or_else(|| {
            Error::Unsupported(format!("{at}: {} has no type", cx.child_name(rd)))
        })?;
        let cl = classes(cx, td, &at, names)?;
        let types = logical_types(&cl, false).map_err(|e| in_object(&at, e))?;
        let name = format!("{}{}", purpose::FLD, names.number(d.id, purpose::FLD)?);
        dims.push(name.clone());
        b.item(Field::new(name, false, types), d.id, at);
    }
    let toks = [Tok::F(f::RECORDER), Tok::F(f::CALC_KIND), Tok::List(dims)];
    b.rule(cx, names, "ByDim", true, &toks, None)?;
    Ok(b.finish())
}

fn md_name_of(recalculation: &str, d: &md_model::RecalcDimension) -> String {
    format!(
        "{recalculation}.{}.{}",
        tag_name(ChildTag::Dimension),
        d.name
    )
}
