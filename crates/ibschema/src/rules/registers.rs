use md_model::Child;

use super::common::{common_group, separators_group};
use super::ctx::{Ctx, Props};
use super::document::table_of;
use super::exchange;
use super::fields;
use super::index::{Tok, indexed};
use super::{Names, TableBuilder, TableRole};
use crate::error::Result;
use crate::logical::{Field, KeyKind, Table};
use crate::names::{StdField, field as f, purpose, role};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Group {
    Dims,
    DimsInTotals,
    Resources,
    ResourcesTotals,
    Attrs,
}

pub(super) fn group(b: &mut TableBuilder, cx: &Ctx, names: &mut Names, g: Group) -> Result<()> {
    let (items, totals): (Vec<&Child>, bool) = match g {
        Group::Dims => (cx.obj.dimensions().collect(), false),
        Group::DimsInTotals => (
            cx.obj
                .dimensions()
                .filter(|d| d.bool_or("UseInTotals", true))
                .collect(),
            false,
        ),
        Group::Resources => (cx.obj.resources().collect(), false),
        Group::ResourcesTotals => (cx.obj.resources().collect(), true),
        Group::Attrs => (cx.obj.attributes().collect(), false),
    };
    for it in items {
        let at = cx.child_name(it);
        let fl = fields::item(cx, it, &at, false, totals, names)?;
        b.item(fl, it.id, at);
    }
    Ok(())
}

fn dim_key(name: &str) -> Field {
    let mut fl = fields::numeric(name, 10, 0);
    for t in &mut fl.types {
        t.extra = Some(1);
    }
    fl
}

fn table(cx: &Ctx, names: &mut Names, purpose: &str, role: &str) -> Result<TableBuilder> {
    table_of(cx, names, purpose, role, KeyKind::Plain)
}

fn property_rule(item: &Child) -> &'static str {
    if item.tag == md_model::ChildTag::Resource {
        "ByResource{n}"
    } else {
        "ByProperty{n}"
    }
}

pub(super) fn indexed_attrs_res<'a>(cx: &Ctx<'a>) -> Vec<&'a Child> {
    cx.obj
        .attributes()
        .chain(cx.obj.resources())
        .filter(|c| indexed(c))
        .collect()
}

pub(super) fn indexed_dims<'a>(cx: &Ctx<'a>) -> Vec<&'a Child> {
    cx.obj.dimensions().filter(|c| indexed(c)).collect()
}

pub(crate) fn information(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    use Tok::F;
    let (per, sub, recpos) = (cx.periodic, cx.sub, cx.recpos);
    let mut b = table(cx, names, purpose::INFO_RG, role::MAIN)?;
    if per {
        b.std(StdField::Period, fields::period());
    }
    if sub {
        b.std(StdField::Recorder, fields::recorder(cx, names)?);
        b.std(StdField::LineNo, fields::line_no(None, 9));
        b.std(StdField::Active, fields::active());
    }
    group(&mut b, cx, names, Group::Dims)?;
    group(&mut b, cx, names, Group::Resources)?;
    group(&mut b, cx, names, Group::Attrs)?;
    common_group(&mut b, cx, names)?;

    let (p, rec, ln, act) = (F(f::PERIOD), F(f::RECORDER), F(f::LINE_NO), F(f::ACTIVE));
    if per && !sub {
        b.rule(cx, names, "ByPeriod", true, &[p.clone(), Tok::Dims], None)?;
    }
    if per && sub && !recpos {
        let toks = [p.clone(), Tok::Dims, act.clone()];
        b.rule(cx, names, "ByPeriod", false, &toks, None)?;
    }
    if recpos {
        let toks = [p.clone(), rec.clone(), ln.clone()];
        b.rule(cx, names, "ByPeriod", true, &toks, None)?;
    }
    if sub {
        let toks = [rec.clone(), ln.clone()];
        b.rule(cx, names, "ByRecorder", true, &toks, None)?;
    }
    if !per && !sub && cx.dims {
        b.rule(cx, names, "ByDims", true, &[Tok::Dims], None)?;
    }
    if !per && sub && cx.dims {
        b.rule(cx, names, "ByDims", false, &[Tok::Dims], None)?;
    }
    if per && !sub {
        let toks = [Tok::Dims, p.clone()];
        b.rule(cx, names, "ByDims", true, &toks, None)?;
    }
    if per && sub && !recpos {
        let toks = [Tok::Dims, p.clone(), act.clone()];
        b.rule(cx, names, "ByDims", false, &toks, None)?;
    }
    if recpos {
        let toks = [Tok::Dims, p.clone(), rec, ln, act.clone()];
        b.rule(cx, names, "ByDims", true, &toks, None)?;
    }
    if per && !sub {
        let toks = [Tok::Attr, p.clone(), Tok::OtherDims];
        for d in cx
            .obj
            .dimensions()
            .filter(|d| indexed(d) || d.flag("Master"))
        {
            b.rule_distinct(cx, names, "ByDims{n}", true, &toks, Some(d))?;
        }
    }
    if !per && !sub {
        let toks = [Tok::Attr, Tok::OtherDims];
        for d in cx
            .obj
            .dimensions()
            .filter(|d| d.text("Indexing") == Some("Index") || d.flag("Master"))
        {
            b.rule_distinct(cx, names, "ByDims{n}", true, &toks, Some(d))?;
        }
    }
    if !sub {
        let toks = if per {
            vec![Tok::Attr, p.clone(), Tok::Dims]
        } else {
            vec![Tok::Attr, Tok::Dims]
        };
        for a in indexed_attrs_res(cx) {
            b.rule(cx, names, property_rule(a), true, &toks, Some(a))?;
        }
    } else if per {
        let toks = [Tok::Attr, p.clone(), Tok::Dims, act];
        for a in indexed_attrs_res(cx) {
            b.rule(cx, names, property_rule(a), false, &toks, Some(a))?;
        }
    }
    let mut out = vec![b.finish()];

    if cx.slice_first || cx.slice_last {
        let mut o = table(
            cx,
            names,
            purpose::INFO_RG_OPT,
            role::INFORMATION_REGISTER_TOTALS_OPTIONS,
        )?;
        o.std(StdField::SliceUsing, fields::boolean(f::SLICE_USING));
        separators_group(&mut o, cx, names)?;
        out.push(o.finish());
    }
    for (on, pur, rl) in [
        (cx.slice_last, purpose::INFO_RG_SL, role::TOTALS_SLICE_LAST),
        (
            cx.slice_first,
            purpose::INFO_RG_SF,
            role::TOTALS_SLICE_FIRST,
        ),
    ] {
        if !on {
            continue;
        }
        let mut s = table(cx, names, pur, rl)?;
        s.std(StdField::Period, fields::period());
        if sub {
            s.std(StdField::Recorder, fields::recorder(cx, names)?);
        }
        group(&mut s, cx, names, Group::Dims)?;
        group(&mut s, cx, names, Group::Resources)?;
        group(&mut s, cx, names, Group::Attrs)?;
        separators_group(&mut s, cx, names)?;
        let toks = [Tok::Dims, p.clone()];
        s.rule(cx, names, "ByDims", !sub, &toks, None)?;
        out.push(s.finish());
    }
    out.extend(exchange::change_records(
        cx,
        names,
        purpose::INFO_RG_CHNGR,
        exchange::Key::InfoRegister,
    )?);
    Ok(out)
}

pub(super) fn totals_options(
    cx: &Ctx,
    names: &mut Names,
    purpose: &str,
    role: &str,
) -> Result<(Table, TableRole)> {
    use Tok::F;
    let mut o = table(cx, names, purpose, role)?;
    o.std(StdField::RegId, fields::binary(f::REG_ID, 16));
    o.std(StdField::Period, fields::period());
    o.std(StdField::ActualPeriod, fields::boolean(f::ACTUAL_PERIOD));
    o.std(StdField::Periodicity, fields::numeric(f::PERIODICITY, 2, 0));
    o.std(
        StdField::RepetitionFactor,
        fields::numeric(f::REPETITION_FACTOR, 2, 0),
    );
    o.std(StdField::UseTotals, fields::numeric(f::USE_TOTALS, 1, 0));
    o.std(StdField::MinPeriod, fields::datetime(f::MIN_PERIOD));
    o.std(StdField::UseSplitter, fields::boolean(f::USE_SPLITTER));
    o.std(
        StdField::MinCalculatedPeriod,
        fields::datetime(f::MIN_CALCULATED_PERIOD),
    );
    separators_group(&mut o, cx, names)?;
    o.rule(cx, names, "ByRegID", true, &[F(f::REG_ID)], None)?;
    Ok(o.finish())
}

pub(crate) fn accumulation(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    use Tok::F;
    let (bal, split) = (cx.balance, cx.split);
    let (p, rec, ln) = (F(f::PERIOD), F(f::RECORDER), F(f::LINE_NO));
    let mut b = table(cx, names, purpose::ACCUM_RG, role::MAIN)?;
    b.std(StdField::Period, fields::period());
    b.std(StdField::Recorder, fields::recorder(cx, names)?);
    b.std(StdField::LineNo, fields::line_no(None, 9));
    b.std(StdField::Active, fields::active());
    if bal {
        b.std(StdField::RecordKind, fields::record_kind());
    }
    group(&mut b, cx, names, Group::Dims)?;
    group(&mut b, cx, names, Group::Resources)?;
    group(&mut b, cx, names, Group::Attrs)?;
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
    let toks = [Tok::Attr, p.clone(), rec, ln];
    for d in indexed_dims(cx) {
        b.rule(cx, names, "ByDims{n}", true, &toks, Some(d))?;
    }
    for a in indexed_attrs_res(cx) {
        b.rule(cx, names, "ByProperty{n}", true, &toks, Some(a))?;
    }
    let mut out = vec![b.finish()];

    let splitter = |b: &mut TableBuilder| {
        if split {
            b.std(StdField::Splitter, fields::splitter());
        }
    };
    if bal {
        let mut t = table(cx, names, purpose::ACCUM_RG_T, role::TOTALS)?;
        t.std(StdField::Period, fields::period());
        group(&mut t, cx, names, Group::Dims)?;
        group(&mut t, cx, names, Group::ResourcesTotals)?;
        splitter(&mut t);
        separators_group(&mut t, cx, names)?;
        let toks = [p.clone(), Tok::Dims, F(f::SPLITTER)];
        t.rule(cx, names, "ByDims", true, &toks, None)?;
        let toks = [p.clone(), Tok::Attr];
        for d in indexed_dims(cx) {
            t.rule(cx, names, "ByDims{n}", false, &toks, Some(d))?;
        }
        out.push(t.finish());
    } else {
        let mut t = table(cx, names, purpose::ACCUM_RG_TN, role::TURNOVERS)?;
        t.std(StdField::Period, fields::period());
        group(&mut t, cx, names, Group::DimsInTotals)?;
        group(&mut t, cx, names, Group::ResourcesTotals)?;
        splitter(&mut t);
        separators_group(&mut t, cx, names)?;
        let toks = [p.clone(), Tok::DimsInTotals, F(f::SPLITTER)];
        t.rule(cx, names, "ByDims", true, &toks, None)?;
        out.push(t.finish());
    }

    out.push(totals_options(
        cx,
        names,
        purpose::ACCUM_RG_OPT,
        role::ACCUMULATION_REGISTER_TOTALS_OPTIONS,
    )?);

    if !bal {
        aggregates(cx, names, &mut out, splitter)?;
    }
    out.extend(exchange::change_records(
        cx,
        names,
        purpose::ACCUM_RG_CHNGR,
        exchange::Key::Recorder,
    )?);
    Ok(out)
}

fn aggregates(
    cx: &Ctx,
    names: &mut Names,
    out: &mut Vec<(Table, TableRole)>,
    splitter: impl Fn(&mut TableBuilder),
) -> Result<()> {
    use Tok::F;
    let p = F(f::PERIOD);

    let mut g = table(
        cx,
        names,
        purpose::ACCUM_RG_AGG_GRID_K,
        role::ACCUMULATION_REGISTERS_AGGREGATE_LIST,
    )?;
    g.std(StdField::RegId, fields::binary(f::REG_ID, 16));
    g.std(StdField::Use, fields::boolean(f::USE));
    g.std(StdField::Level, fields::numeric(f::LEVEL, 2, 0));
    g.std(
        StdField::DefaultPeriodic,
        fields::numeric(f::DEFAULT_PERIODIC, 1, 0),
    );
    g.std(StdField::Periodicity, fields::numeric(f::PERIODICITY, 1, 0));
    g.std(StdField::PrdBeg, fields::datetime(f::PRD_BEG));
    g.std(StdField::PrdEnd, fields::datetime(f::PRD_END));
    for k in 1..=30 {
        g.std(
            StdField::DimUse,
            fields::boolean(&StdField::DimUse.numbered(k)),
        );
    }
    g.std(StdField::Size, fields::numeric(f::SIZE, 15, 0));
    g.std(StdField::Updated, fields::boolean(f::UPDATED));
    g.std(StdField::Version, fields::numeric(f::VERSION, 10, 0));
    g.std(StdField::Id, fields::binary(f::ID, 4));
    g.std(
        StdField::DefaultPrdBeg,
        fields::datetime(f::DEFAULT_PRD_BEG),
    );
    g.std(
        StdField::DefaultPrdEnd,
        fields::datetime(f::DEFAULT_PRD_END),
    );
    g.std(StdField::Prob, fields::numeric(f::PROB, 10, 0));
    g.std(StdField::Data, fields::binary(f::DATA, 32));
    g.std(StdField::OffDate, fields::datetime(f::OFF_DATE));
    separators_group(&mut g, cx, names)?;
    let toks = [
        F(f::REG_ID),
        F(f::DEFAULT_PERIODIC),
        F(f::ID),
        F(f::UPDATED),
    ];
    g.rule(cx, names, "ByID", true, &toks, None)?;
    out.push(g.finish());

    let mut o = table(
        cx,
        names,
        purpose::ACCUM_RG_AGG_OPT_K,
        role::ACCUMULATION_REGISTERS_AGGREGATES_OPTIONS,
    )?;
    o.std(StdField::RegId, fields::binary(f::REG_ID, 16));
    o.std(StdField::AggMode, fields::boolean(f::AGG_MODE));
    o.std(StdField::EnableUse, fields::boolean(f::ENABLE_USE));
    o.std(
        StdField::FillStartDate,
        fields::datetime(f::FILL_START_DATE),
    );
    o.std(StdField::FillEndDate, fields::datetime(f::FILL_END_DATE));
    o.std(
        StdField::DeltaPeriodic,
        fields::numeric(f::DELTA_PERIODIC, 1, 0),
    );
    o.std(
        StdField::BufferPeriodic,
        fields::numeric(f::BUFFER_PERIODIC, 1, 0),
    );
    o.std(StdField::State, fields::numeric(f::STATE, 15, 0));
    separators_group(&mut o, cx, names)?;
    o.rule(cx, names, "ByID", true, &[F(f::REG_ID)], None)?;
    out.push(o.finish());

    for (i, d) in cx.obj.dimensions().enumerate() {
        let k = u32::try_from(i + 1).unwrap_or(u32::MAX);
        let mut t = table(
            cx,
            names,
            &purpose::accum_rg_agg_dict(k),
            role::ACCUMULATION_REGISTER_DIMENSION_DICTIONARY,
        )?;
        t.std(StdField::DimKey, dim_key(f::DIM_KEY));
        let at = cx.child_name(d);
        let fl = fields::item(cx, d, &at, false, false, names)?;
        t.item(fl, d.id, at);
        separators_group(&mut t, cx, names)?;
        t.rule(cx, names, "ByKey", false, &[F(f::DIM_KEY)], None)?;
        t.rule(cx, names, "ByDim", true, &[Tok::Attr], Some(d))?;
        out.push(t.finish());
    }

    let mut dl = table(cx, names, purpose::ACCUM_RG_DL_K, role::NEW_TURNOVERS)?;
    dl.std(StdField::Period, fields::period());
    group(&mut dl, cx, names, Group::Dims)?;
    group(&mut dl, cx, names, Group::ResourcesTotals)?;
    splitter(&mut dl);
    separators_group(&mut dl, cx, names)?;
    out.push(dl.finish());

    let mut bf = table(cx, names, purpose::ACCUM_RG_BF_K, role::TURNOVERS_BUFFER)?;
    bf.std(StdField::Period, fields::period());
    for d in cx.obj.dimensions() {
        let at = cx.child_name(d);
        let name = fields::item_name(d, names)?;
        bf.item(dim_key(&name), d.id, at);
    }
    group(&mut bf, cx, names, Group::ResourcesTotals)?;
    splitter(&mut bf);
    separators_group(&mut bf, cx, names)?;
    out.push(bf.finish());

    let mut st = table(cx, names, purpose::ACCUM_RG_ST, role::QUERY_STATISTICS)?;
    st.std(StdField::Id, fields::binary(f::ID, 8));
    st.std(StdField::Period, fields::period());
    st.std(StdField::Splitter, fields::splitter());
    st.std(StdField::PrdBeg, fields::datetime(f::PRD_BEG));
    st.std(StdField::PrdEnd, fields::datetime(f::PRD_END));
    st.std(StdField::DimsMap, fields::numeric(f::DIMS_MAP, 31, 0));
    st.std(StdField::UseCnt, fields::numeric(f::USE_CNT, 23, 0));
    separators_group(&mut st, cx, names)?;
    st.rule(
        cx,
        names,
        "ById",
        false,
        &[F(f::ID), p, F(f::SPLITTER)],
        None,
    )?;
    st.rule(cx, names, "ByDMap", false, &[F(f::DIMS_MAP)], None)?;
    out.push(st.finish());
    Ok(())
}
