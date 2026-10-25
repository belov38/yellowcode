use md_model::{Child, ChildTag, MdKind, MdObject, Value};

use super::catalog::{attributes, indexed_attributes, main_table, tabular_sections};
use super::common::{common_group, separators_group};
use super::ctx::{Ctx, Props, full_name};
use super::document::table_of;
use super::exchange;
use super::fields::{self, RefForm};
use super::in_object;
use super::index::{Tok, indexed};
use super::types::{Class, classes as classes_of, logical_types, main_target};
use super::{Names, TableBuilder, TableRole};
use crate::error::{Error, Result};
use crate::logical::{Field, KeyKind, Table, TableKind};
use crate::names::{Side, StdField, field as f, purpose, role};

fn flagged(c: &Child) -> bool {
    let set = |name: &str| match c.value(name) {
        Some(Value::Text(t)) => !t.is_empty(),
        Some(Value::Ref(_) | Value::RefList(_)) => !c.refs(name).is_empty(),
        _ => false,
    };
    set("AccountingFlag") || set("ExtDimensionAccountingFlag")
}

fn chart<'a>(cx: &Ctx<'a>) -> Result<&'a MdObject> {
    if cx.kind == MdKind::ChartOfAccounts {
        return Ok(cx.obj);
    }
    cx.coa.ok_or_else(|| {
        Error::Unsupported(format!(
            "{}.ChartOfAccounts: not set or not found",
            cx.full_name
        ))
    })
}

fn ext_dimension_chart<'a>(cx: &Ctx<'a>) -> Result<&'a MdObject> {
    let coa = chart(cx)?;
    let at = format!("{}.ExtDimensionTypes", full_name(coa));
    let r = coa
        .refs("ExtDimensionTypes")
        .first()
        .ok_or_else(|| Error::Unsupported(format!("{at}: not set")))?;
    cx.md
        .resolve(r)
        .ok_or_else(|| Error::Unsupported(format!("{at}: {} is not found", r.name)))
}

fn ed_classes(cx: &Ctx, names: &mut Names) -> Result<Vec<Class>> {
    let chrc = ext_dimension_chart(cx)?;
    let at = format!("{}.Type", full_name(chrc));
    match chrc.prop("Type") {
        Some(Value::Type(td)) => classes_of(cx, td, &at, names),
        _ => Err(Error::Unsupported(format!("{at}: no type"))),
    }
}

fn ed_value_field(cx: &Ctx, name: &str, classes: &[Class]) -> Result<Field> {
    let t = logical_types(classes, false).map_err(|e| in_object(&cx.full_name, e))?;
    Ok(fields::field(name, true, t))
}

struct Cols<'a> {
    totals: bool,
    null_all: bool,
    sided: &'a dyn Fn(&Child) -> bool,
}

fn item_columns(
    b: &mut TableBuilder,
    cx: &Ctx,
    names: &mut Names,
    items: &[&Child],
    how: &Cols,
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for it in items {
        let at = cx.child_name(it);
        let null = how.null_all || flagged(it);
        let fl = fields::item(cx, it, &at, null, how.totals, names)?;
        if (how.sided)(it) {
            for side in [Side::Dr, Side::Cr] {
                let mut s = fl.clone();
                s.name.push_str(side.stor_suffix());
                out.push(s.name.clone());
                b.item_sided(s, it.id, at.clone(), side);
            }
        } else {
            out.push(fl.name.clone());
            b.item(fl, it.id, at);
        }
    }
    Ok(out)
}

fn not_balance(c: &Child) -> bool {
    !c.flag("Balance")
}

fn flags_of<'a>(cx: &Ctx<'a>, tag: ChildTag) -> impl Iterator<Item = &'a Child> {
    cx.obj.children.iter().filter(move |c| c.tag == tag)
}

pub(crate) fn chart_of_accounts(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    use Tok::F;
    let mut b = main_table(cx, names)?;
    let own = b.table.name.clone();
    b.std(StdField::Version, fields::version());
    b.std(StdField::Marked, fields::marked());
    b.std(StdField::PredefinedId, fields::predefined_id());
    b.std(StdField::ParentId, fields::parent_id(&own));
    if cx.code_len > 0 {
        b.std(StdField::Code, fields::code(cx)?);
    }
    if cx.descr_len > 0 {
        b.std(StdField::Description, fields::description(cx.descr_len));
    }
    if cx.order_len > 0 {
        b.std(
            StdField::OrderField,
            fields::string(f::ORDER_FIELD, cx.order_len),
        );
    }
    b.std(StdField::Kind, fields::numeric(f::KIND, 1, 0));
    b.std(StdField::OffBalance, fields::boolean(f::OFF_BALANCE));
    attributes(&mut b, cx, names, false)?;
    for fl in flags_of(cx, ChildTag::AccountingFlag) {
        let at = cx.child_name(fl);
        let field = fields::item(cx, fl, &at, false, false, names)?;
        b.item(field, fl.id, at);
    }
    common_group(&mut b, cx, names)?;

    let (par, id) = (F(f::PARENT_ID), F(f::ID));
    let pid = [F(f::PREDEFINED_ID)];
    b.rule(cx, names, "ByPredefinedIDNotUniq", false, &pid, None)?;
    for (on, plain, with_parent, last) in [
        (cx.code_len > 0, "ByCode", "ByParentCode", f::CODE),
        (cx.descr_len > 0, "ByDescr", "ByParentDescr", f::DESCRIPTION),
        (cx.order_len > 0, "ByOrder", "ByParentOrder", f::ORDER_FIELD),
    ] {
        if !on {
            continue;
        }
        b.rule(cx, names, plain, true, &[F(last), id.clone()], None)?;
        let toks = [par.clone(), F(last), id.clone()];
        b.rule(cx, names, with_parent, true, &toks, None)?;
    }
    let items = indexed_attributes(cx);
    for a in &items {
        let toks = [par.clone(), Tok::Attr, id.clone()];
        b.rule(cx, names, "ByParentField{n}", true, &toks, Some(a))?;
    }
    for a in &items {
        let toks = [Tok::Attr, id.clone()];
        b.rule(cx, names, "ByField{n}", true, &toks, Some(a))?;
    }
    let mut out = vec![b.finish()];

    if cx.ed > 0 {
        out.push(ext_dimension_types(cx, names, &own)?);
    }
    out.extend(tabular_sections(cx, names)?);
    out.extend(exchange::predefined_init(
        cx,
        names,
        purpose::ACC_SINF,
        role::INITIALIZED_PREDEFINED_DATA_IN_CHART_OF_ACCOUNTS,
    )?);
    out.extend(exchange::change_records(
        cx,
        names,
        purpose::ACC_CHNGR,
        exchange::Key::Ref,
    )?);
    Ok(out)
}

fn ext_dimension_types(cx: &Ctx, names: &mut Names, owner: &str) -> Result<(Table, TableRole)> {
    let n = names.number(cx.obj.id, purpose::EXT_DIM)?;
    let name = format!("{}{n}", purpose::EXT_DIM);
    let t = Table::new(name, TableKind::Nested, 0, owner);
    let md = format!("{}.TabularSection.ExtDimensionTypes", cx.full_name);
    let query = format!("{}.ExtDimensionTypes", cx.full_name);
    let mut b = TableBuilder::new(t, role::TABULAR_SECTION, cx.obj.id, md, query);
    b.std(StdField::LineNo, fields::line_no(None, 5));
    let target = main_target(ext_dimension_chart(cx)?, names)?;
    let kind = fields::reference(f::DIM_KIND, &[target], RefForm::T, false);
    b.std(StdField::DimKind, kind);
    let flag = |n| fields::boolean(n);
    b.std(StdField::DimIsMetadata, flag(f::DIM_IS_METADATA));
    b.std(StdField::TurnoverOnly, flag(f::TURNOVER_ONLY));
    for fl in flags_of(cx, ChildTag::ExtDimensionAccountingFlag) {
        let at = cx.child_name(fl);
        let field = fields::item(cx, fl, &at, false, false, names)?;
        b.item(field, fl.id, at);
    }
    let toks = [Tok::Owner, Tok::F(f::LINE_NO), Tok::F(f::DIM_KIND)];
    b.rule(cx, names, "ByLineNo", true, &toks, None)?;
    Ok(b.finish())
}

fn account(cx: &Ctx, names: &mut Names, name: &str) -> Result<Field> {
    let target = main_target(chart(cx)?, names)?;
    Ok(fields::reference(name, &[target], RefForm::T, false))
}

fn kind_field(cx: &Ctx, names: &mut Names, name: &str, null: bool) -> Result<Field> {
    let target = main_target(ext_dimension_chart(cx)?, names)?;
    Ok(fields::reference(name, &[target], RefForm::T, null))
}

pub(crate) fn register(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    use Tok::F;
    let (corr, ed) = (cx.corr, cx.ed);
    let dims: Vec<&Child> = cx.obj.dimensions().collect();
    let res: Vec<&Child> = cx.obj.resources().collect();
    let classes = if ed > 0 {
        ed_classes(cx, names)?
    } else {
        Vec::new()
    };
    let (p, rec, ln) = (F(f::PERIOD), F(f::RECORDER), F(f::LINE_NO));

    let mut b = table_of(cx, names, purpose::ACC_RG, role::MAIN, KeyKind::Plain)?;
    b.std(StdField::Period, fields::period());
    b.std(StdField::Recorder, fields::recorder(cx, names)?);
    b.std(StdField::LineNo, fields::line_no(None, 9));
    b.std(StdField::Active, fields::active());
    if corr {
        b.std(StdField::AccountDt, account(cx, names, f::ACCOUNT_DT)?);
        b.std(StdField::AccountCt, account(cx, names, f::ACCOUNT_CT)?);
    } else {
        b.std(StdField::Correspond, fields::numeric(f::CORRESPOND, 1, 0));
        b.std(StdField::Account, account(cx, names, f::ACCOUNT)?);
    }
    let main_sided = |c: &Child| corr && not_balance(c);
    let how = Cols {
        totals: false,
        null_all: false,
        sided: &main_sided,
    };
    item_columns(&mut b, cx, names, &dims, &how)?;
    item_columns(&mut b, cx, names, &res, &how)?;
    attributes(&mut b, cx, names, false)?;
    for i in 1..=ed {
        let sides: &[(StdField, StdField)] = if corr {
            &[
                (StdField::ValueDt, StdField::KindDt),
                (StdField::ValueCt, StdField::KindCt),
            ]
        } else {
            &[(StdField::Value, StdField::Kind)]
        };
        for (v, k) in sides {
            b.std(*v, ed_value_field(cx, &v.numbered(i), &classes)?);
            b.std(*k, kind_field(cx, names, &k.numbered(i), true)?);
        }
    }
    common_group(&mut b, cx, names)?;

    let key = [p.clone(), rec.clone(), ln.clone()];
    b.rule(cx, names, "ByPeriod", true, &key, None)?;
    b.rule(
        cx,
        names,
        "ByRecorder",
        true,
        &[rec.clone(), ln.clone()],
        None,
    )?;
    let account_rules: &[(&str, &str)] = if corr {
        &[
            ("ByAccountDt", f::ACCOUNT_DT),
            ("ByAccountCt", f::ACCOUNT_CT),
        ]
    } else {
        &[("ByAccount", f::ACCOUNT)]
    };
    for (name, acc) in account_rules {
        let toks = [F(acc), p.clone(), rec.clone(), ln.clone()];
        b.rule(cx, names, name, true, &toks, None)?;
    }
    let toks = [Tok::Attr, p.clone(), rec, ln];
    let indexed_items = cx
        .obj
        .attributes()
        .chain(cx.obj.resources())
        .filter(|c| indexed(c));
    for a in indexed_items {
        b.rule(cx, names, "ByProperty{n}", true, &toks, Some(a))?;
    }
    let mut out = vec![b.finish()];

    for k in 0..=ed {
        out.push(totals_by_accounts(cx, names, k, &classes)?);
    }
    if corr {
        out.push(totals_between_accounts(cx, names)?);
    }
    if ed > 0 {
        out.push(ext_dimension_values(cx, names, &classes)?);
    }
    out.push(super::registers::totals_options(
        cx,
        names,
        purpose::ACC_RG_OPT,
        role::ACCOUNT_REGISTER_TOTALS_OPTIONS,
    )?);
    out.extend(exchange::change_records(
        cx,
        names,
        purpose::ACC_RG_CHNGR,
        exchange::Key::Recorder,
    )?);
    Ok(out)
}

fn splitter(b: &mut TableBuilder, cx: &Ctx) {
    if cx.split {
        b.std(StdField::Splitter, fields::splitter());
    }
}

fn totals_by_accounts(
    cx: &Ctx,
    names: &mut Names,
    k: u32,
    classes: &[Class],
) -> Result<(Table, TableRole)> {
    use Tok::F;
    let mut b = table_of(
        cx,
        names,
        &purpose::acc_rg_at(k),
        &role::totals_by_accounts(k),
        KeyKind::Plain,
    )?;
    b.std(StdField::Account, account(cx, names, f::ACCOUNT)?);
    b.std(StdField::Period, fields::period());
    let single = |_: &Child| false;
    let how = Cols {
        totals: false,
        null_all: false,
        sided: &single,
    };
    let (mut plain, mut with_flag) = (Vec::new(), Vec::new());
    for d in cx.obj.dimensions() {
        let got = item_columns(&mut b, cx, names, &[d], &how)?;
        if flagged(d) {
            with_flag.extend(got);
        } else {
            plain.extend(got);
        }
    }
    let mut values = Vec::new();
    for i in 1..=k {
        let name = StdField::Value.numbered(i);
        b.std(StdField::Value, ed_value_field(cx, &name, classes)?);
        values.push(name);
    }
    for r in cx.obj.resources() {
        let at = cx.child_name(r);
        let fl = fields::item(cx, r, &at, true, true, names)?;
        let types = fl.types.clone();
        b.item(fl, r.id, at);
        for (s, pur) in [
            (StdField::TurnoverDt, purpose::TURNOVER_DT),
            (StdField::TurnoverCt, purpose::TURNOVER_CT),
            (StdField::Turnover, purpose::TURNOVER),
        ] {
            let n = names.number(r.id, pur)?;
            let f = fields::field(s.numbered(n), true, types.clone());
            b.std_of(s, f, cx.child_name(r));
        }
    }
    splitter(&mut b, cx);
    separators_group(&mut b, cx, names)?;
    let toks = [
        F(f::ACCOUNT),
        F(f::PERIOD),
        Tok::List(plain),
        Tok::List(values),
        Tok::List(with_flag),
        F(f::SPLITTER),
    ];
    b.rule(cx, names, "ByPeriod", true, &toks, None)?;
    Ok(b.finish())
}

fn totals_between_accounts(cx: &Ctx, names: &mut Names) -> Result<(Table, TableRole)> {
    use Tok::F;
    let mut b = table_of(
        cx,
        names,
        purpose::ACC_RG_CT,
        role::TOTALS_BETWEEN_ACCOUNTS,
        KeyKind::Plain,
    )?;
    b.std(StdField::Period, fields::period());
    b.std(StdField::AccountDt, account(cx, names, f::ACCOUNT_DT)?);
    b.std(StdField::AccountCt, account(cx, names, f::ACCOUNT_CT)?);
    let dims: Vec<&Child> = cx.obj.dimensions().collect();
    let res: Vec<&Child> = cx.obj.resources().collect();
    let sided = |c: &Child| not_balance(c);
    let dim_cols = Cols {
        totals: false,
        null_all: false,
        sided: &sided,
    };
    let (plain, with_flag): (Vec<&Child>, Vec<&Child>) = dims.iter().partition(|d| !flagged(d));
    let mut ct_dims = item_columns(&mut b, cx, names, &plain, &dim_cols)?;
    ct_dims.extend(item_columns(&mut b, cx, names, &with_flag, &dim_cols)?);
    let res_cols = Cols {
        totals: true,
        null_all: true,
        sided: &sided,
    };
    item_columns(&mut b, cx, names, &res, &res_cols)?;
    splitter(&mut b, cx);
    separators_group(&mut b, cx, names)?;
    let (p, dt, ct, sp) = (
        F(f::PERIOD),
        F(f::ACCOUNT_DT),
        F(f::ACCOUNT_CT),
        F(f::SPLITTER),
    );
    let by_dt = [
        p.clone(),
        dt.clone(),
        ct.clone(),
        Tok::List(ct_dims.clone()),
        sp.clone(),
    ];
    b.rule(cx, names, "ByDt", true, &by_dt, None)?;
    let by_ct = [p.clone(), ct, dt, Tok::List(ct_dims), sp];
    b.rule(cx, names, "ByCt", true, &by_ct, None)?;
    b.rule(cx, names, "ByPeriod", false, &[p], None)?;
    Ok(b.finish())
}

fn ext_dimension_values(
    cx: &Ctx,
    names: &mut Names,
    classes: &[Class],
) -> Result<(Table, TableRole)> {
    use Tok::F;
    let mut b = table_of(
        cx,
        names,
        purpose::ACC_RG_ED,
        role::EXT_DIMENSIONS_VALUES,
        KeyKind::Plain,
    )?;
    b.role.query_name = format!("{}.ExtDimensions", cx.full_name);
    b.std(StdField::Period, fields::period());
    b.std(StdField::Recorder, fields::recorder(cx, names)?);
    b.std(StdField::LineNo, fields::line_no(None, 9));
    if cx.corr {
        b.std(StdField::Correspond, fields::numeric(f::CORRESPOND, 1, 0));
    }
    b.std(StdField::Kind, kind_field(cx, names, f::KIND, false)?);
    b.std(StdField::Value, ed_value_field(cx, f::VALUE, classes)?);
    let (p, rec, ln) = (F(f::PERIOD), F(f::RECORDER), F(f::LINE_NO));
    let (kind, corr) = (F(f::KIND), F(f::CORRESPOND));
    let toks = [
        p.clone(),
        rec.clone(),
        ln.clone(),
        kind.clone(),
        corr.clone(),
    ];
    b.rule(cx, names, "ByPeriod", true, &toks, None)?;
    let toks = [rec.clone(), ln.clone(), corr.clone()];
    b.rule(cx, names, "ByRecorder", false, &toks, None)?;
    let toks = [F(f::VALUE), kind, p, rec, ln, corr];
    b.rule(cx, names, "ByExtDim", true, &toks, None)?;
    Ok(b.finish())
}
