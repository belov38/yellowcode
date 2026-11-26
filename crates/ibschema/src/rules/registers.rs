use md_model::Child;

use super::common::{common_group, is_independent, separators, separators_group};
use super::ctx::{Ctx, Props};
use super::document::table_of;
use super::exchange;
use super::fields;
use super::index::{Tok, indexed, plain_indexed};
use super::types::{self, Class};
use super::{Names, TableBuilder, TableRole};
use crate::error::Result;
use crate::logical::{Field, KeyKind, Table, TypeCode};
use crate::names::{StdField, field as f, purpose, role};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Group {
    Dims,
    DimsInTotals,
    Resources,
    ResourcesTotals,
    ResourcesAggregates,
    Attrs,
}

pub(super) fn group(b: &mut TableBuilder, cx: &Ctx, names: &mut Names, g: Group) -> Result<()> {
    let (items, totals, agg_res): (Vec<&Child>, bool, bool) = match g {
        Group::Dims => (cx.obj.dimensions().collect(), false, false),
        Group::DimsInTotals => (
            cx.obj
                .dimensions()
                .filter(|d| d.bool_or("UseInTotals", true))
                .collect(),
            false,
            false,
        ),
        Group::Resources => (cx.obj.resources().collect(), false, false),
        Group::ResourcesTotals => (cx.obj.resources().collect(), true, false),
        Group::ResourcesAggregates => (cx.obj.resources().collect(), true, true),
        Group::Attrs => (cx.obj.attributes().collect(), false, false),
    };
    for it in items {
        let at = cx.child_name(it);
        let fl = if agg_res
            && let Some(td) = it.type_desc().filter(|td| td.types.iter().any(|t| t.set))
        {
            let cls = types::classes(cx, td, &at, names)?;
            if matches!(cls.as_slice(), [Class::N { .. }]) {
                fields::numeric(&fields::item_name(it, names)?, 6, 0)
            } else {
                fields::item(cx, it, &at, false, totals, names)?
            }
        } else {
            fields::item(cx, it, &at, false, totals, names)?
        };
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

fn plain_indexed_attrs_res<'a>(cx: &Ctx<'a>) -> Vec<&'a Child> {
    cx.obj
        .attributes()
        .chain(cx.obj.resources())
        .filter(|c| plain_indexed(c))
        .collect()
}

pub(super) fn indexed_dims<'a>(cx: &Ctx<'a>) -> Vec<&'a Child> {
    cx.obj.dimensions().filter(|c| indexed(c)).collect()
}

fn plain_indexed_dims<'a>(cx: &Ctx<'a>) -> Vec<&'a Child> {
    cx.obj.dimensions().filter(|c| plain_indexed(c)).collect()
}

fn main_filter(v: Option<&md_model::Value>) -> bool {
    !matches!(v, Some(md_model::Value::Bool(false)))
}

fn dim_key_weight(cx: &Ctx, d: &Child, names: &mut Names) -> Result<usize> {
    let Some(td) = d.type_desc() else {
        return Ok(1);
    };
    if td.types.len() <= 1 {
        return Ok(1);
    }
    let at = cx.child_name(d);
    let cls = types::classes(cx, td, &at, names)?;
    let lt = types::logical_types(&cls, false)?;
    Ok(lt
        .iter()
        .map(|t| {
            if t.code == TypeCode::R && t.ref_kind == 4 {
                2
            } else {
                1
            }
        })
        .sum())
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
    if per && !sub && cx.dims {
        let toks = [Tok::Dims, p.clone()];
        b.rule(cx, names, "ByDims", true, &toks, None)?;
    }
    if per && sub && !recpos {
        let toks = [Tok::Dims, p.clone(), act.clone()];
        b.rule(cx, names, "ByDims", false, &toks, None)?;
    }
    if recpos {
        let toks = [Tok::Dims, p.clone(), rec.clone(), ln.clone(), act.clone()];
        b.rule(cx, names, "ByDims", true, &toks, None)?;
    }
    let key_dims: Vec<&Child> = cx
        .obj
        .dimensions()
        .filter(|d| plain_indexed(d) || d.flag("Master"))
        .collect();
    if per && !sub {
        let toks = [Tok::Attr, p.clone(), Tok::OtherDims];
        for d in &key_dims {
            b.rule_distinct(cx, names, "ByDims{n}", true, &toks, Some(d))?;
        }
    } else if !per && !sub {
        let toks = [Tok::Attr, Tok::OtherDims];
        for d in &key_dims {
            b.rule_distinct(cx, names, "ByDims{n}", true, &toks, Some(d))?;
        }
    } else if recpos {
        let toks = [Tok::Attr, p.clone(), rec.clone(), ln.clone(), act.clone()];
        for d in &key_dims {
            b.rule_distinct(cx, names, "ByDims{n}", true, &toks, Some(d))?;
        }
    } else if per && sub {
        let toks = [Tok::Attr, p.clone(), Tok::OtherDims, act.clone()];
        for d in &key_dims {
            b.rule_distinct(cx, names, "ByDims{n}", false, &toks, Some(d))?;
        }
    } else {
        let toks = [Tok::Attr, Tok::OtherDims];
        for d in &key_dims {
            b.rule_distinct(cx, names, "ByDims{n}", false, &toks, Some(d))?;
        }
    }
    if !sub {
        let toks = if per {
            vec![Tok::Attr, p.clone(), Tok::Dims]
        } else {
            vec![Tok::Attr, Tok::Dims]
        };
        for a in plain_indexed_attrs_res(cx) {
            b.rule(cx, names, property_rule(a), true, &toks, Some(a))?;
        }
    } else if recpos {
        for a in plain_indexed_attrs_res(cx) {
            let toks = if a.tag == md_model::ChildTag::Resource {
                vec![Tok::Attr, p.clone(), rec.clone(), ln.clone()]
            } else {
                vec![Tok::Attr, p.clone(), rec.clone(), ln.clone(), act.clone()]
            };
            b.rule(cx, names, property_rule(a), true, &toks, Some(a))?;
        }
    } else if per {
        for a in plain_indexed_attrs_res(cx) {
            let toks = if a.tag == md_model::ChildTag::Resource {
                vec![Tok::Attr, p.clone(), Tok::Dims]
            } else {
                vec![Tok::Attr, p.clone(), Tok::Dims, act.clone()]
            };
            b.rule(cx, names, property_rule(a), false, &toks, Some(a))?;
        }
    } else {
        let toks = [Tok::Attr, Tok::Dims];
        for a in plain_indexed_attrs_res(cx) {
            b.rule(cx, names, property_rule(a), false, &toks, Some(a))?;
        }
    }
    if !sub && cx.dims {
        let mfp = per && main_filter(cx.obj.value("MainFilterOnPeriod"));
        let mut mf = Vec::new();
        let mut rest = Vec::new();
        for d in cx.obj.dimensions() {
            let name = fields::item_name(d, names)?;
            if main_filter(d.value("MainFilter")) {
                mf.push(name);
            } else {
                rest.push(name);
            }
        }
        let mut key = Vec::with_capacity(mf.len() + rest.len() + usize::from(per));
        if mfp {
            key.push(f::PERIOD.to_string());
        }
        key.extend(mf);
        if per && !mfp {
            key.push(f::PERIOD.to_string());
        }
        key.extend(rest);
        b.rule_distinct(cx, names, "ByMainFilter", false, &[Tok::List(key)], None)?;
    }
    let mut out = vec![b.finish()];

    if per && (cx.slice_first || cx.slice_last) {
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
        (
            per && cx.slice_last,
            purpose::INFO_RG_SL,
            role::TOTALS_SLICE_LAST,
        ),
        (
            per && cx.slice_first,
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
        if recpos {
            let toks = [Tok::Dims, p.clone(), F(f::RECORDER)];
            s.rule(cx, names, "ByDims", true, &toks, None)?;
        } else {
            let toks = [Tok::Dims, p.clone()];
            s.rule(cx, names, "ByDims", !sub, &toks, None)?;
            let dim_toks = [Tok::Attr, Tok::OtherDims];
            for d in &key_dims {
                s.rule_unless_prefix(cx, names, "ByDims{n}", !sub, &dim_toks, Some(d))?;
            }
            let prop_toks = [Tok::Attr, Tok::Dims];
            for a in plain_indexed_attrs_res(cx) {
                s.rule(cx, names, property_rule(a), false, &prop_toks, Some(a))?;
            }
        }
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
    let tot_dims: Vec<&Child> = if bal {
        cx.obj.dimensions().collect()
    } else {
        cx.obj
            .dimensions()
            .filter(|d| d.bool_or("UseInTotals", true))
            .collect()
    };
    let indep_seps = separators(cx)
        .into_iter()
        .filter(|s| is_independent(s))
        .count();
    let overhead = 1 + usize::from(split) + indep_seps;
    let mut dim_weights = Vec::with_capacity(tot_dims.len());
    for d in &tot_dims {
        dim_weights.push(dim_key_weight(cx, d, names)?);
    }
    let use_dim_hash = !tot_dims.is_empty() && overhead + dim_weights.iter().sum::<usize>() > 16;

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
    if use_dim_hash {
        b.std(StdField::DimHash, fields::numeric(f::DIM_HASH, 10, 0));
    }

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
    for d in plain_indexed_dims(cx) {
        b.rule(cx, names, "ByDims{n}", true, &toks, Some(d))?;
    }
    for a in plain_indexed_attrs_res(cx) {
        b.rule(cx, names, "ByProperty{n}", true, &toks, Some(a))?;
    }
    let mut out = vec![b.finish()];

    let splitter = |b: &mut TableBuilder| {
        if split {
            b.std(StdField::Splitter, fields::splitter());
        }
    };
    let by_dims_fields = |names: &mut Names| -> Result<(bool, Vec<String>)> {
        let mut fields = vec![f::PERIOD.to_string()];
        if use_dim_hash {
            let mut used = overhead + 1;
            for (d, &w) in tot_dims.iter().zip(&dim_weights) {
                if used + w > 16 {
                    break;
                }
                used += w;
                fields.push(fields::item_name(d, names)?);
            }
            fields.push(f::DIM_HASH.to_string());
        } else {
            for d in &tot_dims {
                fields.push(fields::item_name(d, names)?);
            }
        }
        if split {
            fields.push(f::SPLITTER.to_string());
        }
        Ok((!use_dim_hash, fields))
    };
    if bal {
        let mut t = table(cx, names, purpose::ACCUM_RG_T, role::TOTALS)?;
        t.std(StdField::Period, fields::period());
        group(&mut t, cx, names, Group::Dims)?;
        group(&mut t, cx, names, Group::ResourcesTotals)?;
        separators_group(&mut t, cx, names)?;
        if use_dim_hash {
            t.std(StdField::DimHash, fields::numeric(f::DIM_HASH, 10, 0));
        }
        splitter(&mut t);
        let (uniq, key) = by_dims_fields(names)?;
        t.rule(cx, names, "ByDims", uniq, &[Tok::List(key)], None)?;
        let toks = [p.clone(), Tok::Attr];
        for d in plain_indexed_dims(cx) {
            t.rule_unless_prefix(cx, names, "ByDims{n}", false, &toks, Some(d))?;
        }
        out.push(t.finish());
    } else if !tot_dims.is_empty() {
        let mut t = table(cx, names, purpose::ACCUM_RG_TN, role::TURNOVERS)?;
        t.std(StdField::Period, fields::period());
        group(&mut t, cx, names, Group::DimsInTotals)?;
        group(&mut t, cx, names, Group::ResourcesTotals)?;
        separators_group(&mut t, cx, names)?;
        if use_dim_hash {
            t.std(StdField::DimHash, fields::numeric(f::DIM_HASH, 10, 0));
        }
        splitter(&mut t);
        let (uniq, key) = by_dims_fields(names)?;
        t.rule(cx, names, "ByDims", uniq, &[Tok::List(key)], None)?;
        let toks = [Tok::Attr, p.clone()];
        for d in tot_dims
            .iter()
            .skip(1)
            .copied()
            .filter(|d| plain_indexed(d))
        {
            t.rule(cx, names, "ByDims{n}", false, &toks, Some(d))?;
        }
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

    let mut dims = Vec::new();
    for d in cx.obj.dimensions() {
        dims.push((names.number(d.id, purpose::FLD)?, d));
    }
    dims.sort_by_key(|(n, _)| *n);
    for (i, (_, d)) in dims.into_iter().enumerate() {
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
    group(&mut dl, cx, names, Group::ResourcesAggregates)?;
    separators_group(&mut dl, cx, names)?;
    splitter(&mut dl);
    out.push(dl.finish());

    let mut bf = table(cx, names, purpose::ACCUM_RG_BF_K, role::TURNOVERS_BUFFER)?;
    bf.std(StdField::Period, fields::period());
    for d in cx.obj.dimensions() {
        let at = cx.child_name(d);
        let name = fields::item_name(d, names)?;
        bf.item(dim_key(&name), d.id, at);
    }
    group(&mut bf, cx, names, Group::ResourcesAggregates)?;
    separators_group(&mut bf, cx, names)?;
    splitter(&mut bf);
    out.push(bf.finish());

    let mut st = table(cx, names, purpose::ACCUM_RG_ST, role::QUERY_STATISTICS)?;
    st.std(StdField::Id, fields::binary(f::ID, 8));
    st.std(StdField::Period, fields::period());
    st.std(StdField::Splitter, fields::splitter());
    st.std(StdField::PrdBeg, fields::datetime(f::PRD_BEG));
    st.std(StdField::PrdEnd, fields::datetime(f::PRD_END));
    st.std(StdField::DimsMap, fields::numeric(f::DIMS_MAP, 31, 0));
    st.std(StdField::UseCnt, fields::numeric(f::USE_CNT, 23, 0));
    let mut use_names = Vec::new();
    for sep in separators(cx) {
        let u = names.number(sep.id, purpose::DATA_SEPARATION_USE)?;
        let uname = StdField::DataSeparationUse.numbered(u);
        st.std(StdField::DataSeparationUse, fields::boolean(&uname));
        use_names.push(uname);
    }
    separators_group(&mut st, cx, names)?;
    if !use_names.is_empty() {
        st.table.splitters[1].clear();
    }
    let mut by_id = vec![F(f::ID), p, F(f::SPLITTER)];
    if !use_names.is_empty() {
        by_id.push(Tok::List(use_names.clone()));
    }
    st.rule(cx, names, "ById", false, &by_id, None)?;
    st.rule(cx, names, "ByDMap", false, &[F(f::DIMS_MAP)], None)?;
    if !use_names.is_empty() {
        st.rule(cx, names, "BySepUse", false, &[Tok::List(use_names)], None)?;
    }
    out.push(st.finish());
    Ok(())
}
