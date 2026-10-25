use super::objects::{Common, bool_prop, common_field, int_prop, refs_type, sub_table, text_prop};
use super::templates::{self, P_BASE_RES, P_DIM, P_EXT, P_RES, P_SCHED, TplNames};
use super::{
    Builder, FieldRole, Item, ObjMeta, Only, PeriodField, QField, QFieldId, QTable, QTableId,
    SchemaError, StdRole, TableKind, VtKind, names, number, string,
};
use crate::types::{DateQ, Other, QType};
use brace::Guid;
use bsl_prim::fold_name;
use md_model::{ChildTag, Configuration, MdObject, MdRef, Value};
use mdschema::MdKind;
use platform_catalog::QueryTableInfo;
use query_syntax::Kw;
use std::collections::HashMap;

const REGISTER_KINDS: [MdKind; 4] = [
    MdKind::InformationRegister,
    MdKind::AccumulationRegister,
    MdKind::AccountingRegister,
    MdKind::CalculationRegister,
];

const LINE_NUMBER_DIGITS: u8 = 9;
const VT_DIGITS: u8 = 38;
const SUM_EXTRA_DIGITS: u8 = 6;
const DATA_VERSION_LEN: u32 = 12;

const UNITS: [(&str, Kw); 10] = [
    ("Секунда", Kw::Second),
    ("Минута", Kw::Minute),
    ("Час", Kw::Hour),
    ("День", Kw::Day),
    ("Неделя", Kw::Week),
    ("Декада", Kw::TenDays),
    ("Месяц", Kw::Month),
    ("Квартал", Kw::Quarter),
    ("Полугодие", Kw::HalfYear),
    ("Год", Kw::Year),
];

pub(super) struct ObjIndex<'a> {
    by_id: HashMap<Guid, &'a MdObject>,
    by_name: HashMap<String, &'a MdObject>,
}

impl<'a> ObjIndex<'a> {
    pub fn new(cfg: &'a Configuration) -> ObjIndex<'a> {
        let mut by_id = HashMap::with_capacity(cfg.objects.len());
        let mut by_name = HashMap::with_capacity(cfg.objects.len());
        for o in &cfg.objects {
            by_id.entry(o.id).or_insert(o);
            by_name
                .entry(fold_name(&format!("{}.{}", o.kind.as_str(), o.name)))
                .or_insert(o);
        }
        ObjIndex { by_id, by_name }
    }

    pub fn get(&self, r: &MdRef) -> Option<&'a MdObject> {
        if let Some(o) = r.id.and_then(|id| self.by_id.get(&id)) {
            return Some(o);
        }
        let mut segs = r.name.splitn(3, '.');
        let (Some(k), Some(n)) = (segs.next(), segs.next()) else {
            return None;
        };
        self.by_name.get(&fold_name(&format!("{k}.{n}"))).copied()
    }

    pub fn by_id(&self, id: Guid) -> Option<&'a MdObject> {
        self.by_id.get(&id).copied()
    }
}

pub(super) struct Tf {
    kind: MdKind,
    names: TplNames,
    fields: Vec<QField>,
    star: Vec<QFieldId>,
    missing: Vec<String>,
}

impl Tf {
    pub fn new(kind: MdKind, tpl: Option<&QueryTableInfo>) -> Tf {
        Tf {
            kind,
            names: TplNames::new(tpl),
            fields: Vec::new(),
            star: Vec::new(),
            missing: Vec::new(),
        }
    }

    pub fn push(&mut self, f: QField, star: bool) {
        if star && let Ok(i) = u16::try_from(self.fields.len()) {
            self.star.push(QFieldId(i));
        }
        self.fields.push(f);
    }

    pub fn named(
        &mut self,
        (ru, en): (String, String),
        role: FieldRole,
        ty: QType,
        star: bool,
        period: Option<PeriodField>,
    ) {
        let name_en = self.names.fixed(&ru).map_or(en, str::to_string);
        self.push(
            QField {
                name_ru: ru,
                name_en,
                ty,
                role,
                only: Only::Any,
                nested: None,
                md: None,
                period,
                side: None,
            },
            star,
        );
    }

    fn sided(&mut self, side: super::Side, add: impl FnOnce(&mut Tf)) {
        let n = self.fields.len();
        add(self);
        for x in self.fields.iter_mut().skip(n) {
            x.side = Some(side);
        }
    }

    pub fn std(&mut self, role: StdRole, ty: QType) {
        self.named(
            names::std_name(role, self.kind),
            FieldRole::Std(role),
            ty,
            true,
            None,
        );
    }

    pub fn virt(&mut self, role: StdRole, ty: QType) {
        self.named(
            names::std_name(role, self.kind),
            FieldRole::Virtual,
            ty,
            false,
            None,
        );
    }

    fn period(&mut self, ru: String, en: String, ty: QType, p: PeriodField) {
        if self.names.fixed(&ru).is_none() {
            self.missing.push(ru.clone());
        }
        self.named((ru, en), FieldRole::Virtual, ty, true, Some(p));
    }

    pub fn item(&mut self, it: &Item, role: FieldRole) {
        self.push(
            QField {
                name_ru: it.name.clone(),
                name_en: it.name.clone(),
                ty: it.ty.clone(),
                role,
                only: Only::Any,
                nested: None,
                md: Some(it.id),
                period: None,
                side: None,
            },
            true,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn derived(
        &mut self,
        ph: &str,
        pre: &str,
        item: &str,
        suf: &str,
        role: FieldRole,
        ty: QType,
        md: Option<Guid>,
    ) {
        let Some((name_ru, name_en)) = self.names.pattern(ph, pre, suf, item) else {
            self.missing.push(format!("{pre}{ph}{suf}"));
            return;
        };
        self.push(
            QField {
                name_ru,
                name_en,
                ty,
                role,
                only: Only::Any,
                nested: None,
                md,
                period: None,
                side: None,
            },
            true,
        );
    }

    fn commons(&mut self, commons: &[QField]) {
        for c in commons {
            self.push(c.clone(), true);
        }
    }

    fn into_table(self, t: &mut QTable) -> Vec<String> {
        t.fields = self.fields;
        t.star = self.star;
        self.missing
    }
}

struct Reg<'a> {
    obj: &'a MdObject,
    table: QTableId,
    meta: ObjMeta,
    commons: Vec<QField>,
    recorder: QType,
    record_kind: QType,
    account: QType,
    ext_value: QType,
    ext_kind: QType,
    period: QType,
    calc_type: QType,
    chart: Option<&'a MdObject>,
    adjustment_len: u32,
}

impl Reg<'_> {
    fn owner(&self) -> String {
        format!("{}.{}", self.obj.kind.as_str(), self.obj.name)
    }

    fn key(&self, suffix: &str) -> String {
        format!("{}.*{suffix}", self.obj.kind.as_str())
    }

    fn nc(&self) -> bool {
        self.obj.kind == MdKind::AccountingRegister && !self.meta.corr
    }
}

pub(super) fn build(b: &mut Builder<'_>, commons: &[Common]) -> Result<(), SchemaError> {
    let cfg = b.cfg;
    let objs = ObjIndex::new(cfg);
    let recorders = recorders(b, &objs);
    let mut regs = Vec::new();
    for o in cfg
        .objects
        .iter()
        .filter(|o| REGISTER_KINDS.contains(&o.kind))
    {
        if let Some(r) = declare(b, o, commons, &objs, &recorders)? {
            regs.push(r);
        }
    }
    for r in &regs {
        main_table(b, r)?;
        b.meta.insert(r.table, r.meta.clone());
    }
    for r in &regs {
        match r.obj.kind {
            MdKind::AccumulationRegister => accumulation_vts(b, r)?,
            MdKind::InformationRegister => information_vts(b, r)?,
            MdKind::AccountingRegister => accounting_vts(b, r)?,
            MdKind::CalculationRegister => {
                calculation_vts(b, r, &regs, &objs)?;
                recalculations(b, r)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn recorders(b: &Builder<'_>, objs: &ObjIndex<'_>) -> HashMap<Guid, Vec<crate::types::RefAtom>> {
    let mut out: HashMap<Guid, Vec<crate::types::RefAtom>> = HashMap::new();
    for d in b.cfg.objects.iter().filter(|o| o.kind == MdKind::Document) {
        let Some(atom) = b.by_object.get(&d.id).and_then(|t| b.reference_of(*t)) else {
            continue;
        };
        let Some(Value::RefList(list)) = d.prop("RegisterRecords") else {
            continue;
        };
        for r in list {
            if let Some(reg) = objs.get(r) {
                out.entry(reg.id).or_default().push(atom);
            }
        }
    }
    for v in out.values_mut() {
        v.sort();
        v.dedup();
    }
    out
}

pub(super) fn items(b: &mut Builder<'_>, o: &MdObject, tag: ChildTag, owner: &str) -> Vec<Item> {
    let mut out = Vec::new();
    for c in o.children.iter().filter(|c| c.tag == tag) {
        let ty = b.child_type(c, owner);
        let flag = |n: &str| !matches!(c.prop(n), Some(Value::Bool(false)));
        out.push(Item {
            id: c.id,
            name: c.name.clone(),
            ty,
            balance: flag("Balance"),
            main_filter: flag("MainFilter"),
        });
    }
    out
}

fn sys_enum(b: &mut Builder<'_>, en: &str) -> QType {
    match b.sys_enums.find(en) {
        Some(idx) => QType::other(Other::SysEnum(idx)),
        None => {
            b.diag(format!("platform type {en}: not in the catalog"));
            QType::default()
        }
    }
}

fn ref_of(b: &Builder<'_>, o: Option<&MdObject>) -> QType {
    o.and_then(|o| b.by_object.get(&o.id))
        .and_then(|t| b.reference_of(*t))
        .map_or_else(QType::default, QType::reference)
}

fn ref_prop<'o>(o: &'o MdObject, name: &str) -> Option<&'o MdRef> {
    match o.prop(name) {
        Some(Value::Ref(r)) if !r.name.is_empty() || r.id.is_some() => Some(r),
        _ => None,
    }
}

pub(super) fn declare_main(
    b: &mut Builder<'_>,
    o: &MdObject,
    tpl: Option<&QueryTableInfo>,
    kind: TableKind,
) -> Result<Option<QTableId>, SchemaError> {
    let Some(tpl) = tpl else {
        b.diag(format!(
            "{}.{}: no query table template for the kind",
            o.kind.as_str(),
            o.name
        ));
        return Ok(None);
    };
    let (ru, en) = templates::first_segment(tpl);
    let t = QTable {
        name_ru: format!("{ru}.{}", o.name),
        name_en: format!("{en}.{}", o.name),
        kind,
        object: Some(o.id),
        owner: None,
        fields: Vec::new(),
        star: Vec::new(),
        params: Vec::new(),
        reference: None,
    };
    let id = b.add_table(t, &[])?;
    b.add_object(o.kind, &o.name, o.id, id);
    Ok(Some(id))
}

fn declare<'a>(
    b: &mut Builder<'a>,
    o: &'a MdObject,
    commons: &[Common],
    objs: &ObjIndex<'a>,
    recorders: &HashMap<Guid, Vec<crate::types::RefAtom>>,
) -> Result<Option<Reg<'a>>, SchemaError> {
    let owner = format!("{}.{}", o.kind.as_str(), o.name);
    let mut meta = ObjMeta::new(o.kind);
    meta.corr = o.kind == MdKind::AccountingRegister && bool_prop(o, "Correspondence");
    let tpl = templates::find(b.cat, &format!("{}.*", o.kind.as_str()), {
        o.kind == MdKind::AccountingRegister && !meta.corr
    });
    let Some(table) = declare_main(b, o, tpl, TableKind::Object(o.kind))? else {
        return Ok(None);
    };
    meta.dims = items(b, o, ChildTag::Dimension, &owner);
    meta.resources = items(b, o, ChildTag::Resource, &owner);
    meta.attrs = items(b, o, ChildTag::Attribute, &owner);
    let used: Vec<&Common> = commons.iter().filter(|c| c.used_by(o)).collect();
    meta.commons = used.iter().map(|c| c.name.clone()).collect();
    let mut r = Reg {
        obj: o,
        table,
        commons: used.iter().map(|c| common_field(c)).collect(),
        meta,
        recorder: refs_type(recorders.get(&o.id).cloned().unwrap_or_default()),
        record_kind: QType::default(),
        account: QType::default(),
        ext_value: QType::default(),
        ext_kind: QType::default(),
        period: QType::date(DateQ::DateTime),
        calc_type: QType::default(),
        chart: None,
        adjustment_len: 0,
    };
    match o.kind {
        MdKind::AccumulationRegister => {
            r.meta.balance_type = text_prop(o, "RegisterType") != Some("Turnovers");
            if r.meta.balance_type {
                r.record_kind = sys_enum(b, "AccumulationRecordType");
            }
        }
        MdKind::InformationRegister => information(b, &mut r, &owner),
        MdKind::AccountingRegister => accounting(b, &mut r, objs, &owner),
        MdKind::CalculationRegister => calculation(b, &mut r, objs, &owner),
        _ => {}
    }
    Ok(Some(r))
}

fn information(b: &mut Builder<'_>, r: &mut Reg<'_>, owner: &str) {
    let o = r.obj;
    let per = text_prop(o, "InformationRegisterPeriodicity").unwrap_or("Nonperiodical");
    r.meta.periodic = per != "Nonperiodical";
    r.period = QType::date(match per {
        "Day" | "Month" | "Quarter" | "Year" => DateQ::Date,
        "Second" | "RecorderPosition" | "Nonperiodical" => DateQ::DateTime,
        other => {
            b.diag(format!(
                "{owner}: InformationRegisterPeriodicity {other} is not known"
            ));
            DateQ::DateTime
        }
    });
    r.meta.subordinate = text_prop(o, "WriteMode") == Some("RecorderSubordinate");
    r.meta.filter_on_period = bool_prop(o, "MainFilterOnPeriod");
}

fn accounting<'a>(b: &mut Builder<'_>, r: &mut Reg<'a>, objs: &ObjIndex<'a>, owner: &str) {
    let o = r.obj;
    r.record_kind = sys_enum(b, "AccountingRecordType");
    r.adjustment_len = int_prop(o, "PeriodAdjustmentLength");
    r.meta.adjustment = r.adjustment_len > 0;
    let Some(chart) = ref_prop(o, "ChartOfAccounts").and_then(|c| objs.get(c)) else {
        b.diag(format!("{owner}: no ChartOfAccounts"));
        return;
    };
    r.account = ref_of(b, Some(chart));
    let Some(kinds) = ref_prop(chart, "ExtDimensionTypes").and_then(|c| objs.get(c)) else {
        return;
    };
    r.meta.ext_dims = u8::try_from(int_prop(chart, "MaxExtDimensionCount")).unwrap_or(u8::MAX);
    r.ext_kind = ref_of(b, Some(kinds));
    r.ext_value = match kinds.prop("Type") {
        Some(Value::Type(td)) => b.map(td, &format!("{owner}.ExtDimension")),
        _ => {
            b.diag(format!(
                "ChartOfCharacteristicTypes.{}: no Type",
                kinds.name
            ));
            QType::default()
        }
    };
}

fn calculation<'a>(b: &mut Builder<'_>, r: &mut Reg<'a>, objs: &ObjIndex<'a>, owner: &str) {
    let o = r.obj;
    let charts: Vec<&MdObject> = b
        .cfg
        .objects
        .iter()
        .filter(|x| x.kind == MdKind::ChartOfCalculationTypes)
        .collect();
    r.chart = match ref_prop(o, "ChartOfCalculationTypes") {
        Some(c) => objs.get(c),
        None => match charts.as_slice() {
            [one] => objs.by_id(one.id),
            _ => None,
        },
    };
    r.calc_type = match r.chart {
        Some(c) => ref_of(b, Some(c)),
        None => {
            b.diag(format!(
                "{owner}: chart of calculation types not known ({} charts)",
                charts.len()
            ));
            let mut t = QType::default();
            for c in &charts {
                t.refs.extend(ref_of(b, Some(c)).refs);
            }
            t.refs.sort();
            t.refs.dedup();
            t
        }
    };
    r.meta.action_period = match o.prop("ActionPeriod") {
        Some(Value::Bool(v)) => *v,
        _ => r.chart.is_some_and(|c| bool_prop(c, "ActionPeriodUse")),
    };
    r.meta.base_period = bool_prop(o, "BasePeriod");
    let Some(sched) = ref_prop(o, "Schedule").and_then(|s| objs.get(s)) else {
        return;
    };
    let value = match o.prop("ScheduleValue") {
        Some(Value::Ref(v)) => v.name.rsplit('.').next().map(str::to_string),
        Some(Value::Text(v)) => v.rsplit('.').next().map(str::to_string),
        _ => None,
    };
    let res = items(
        b,
        sched,
        ChildTag::Resource,
        &format!("InformationRegister.{}", sched.name),
    );
    r.meta.schedule_resource = value.and_then(|v| res.into_iter().find(|x| x.name == v));
    if r.meta.schedule_resource.is_none() {
        b.diag(format!(
            "{owner}: ScheduleValue is not a resource of the schedule"
        ));
    }
}

pub(super) fn flush(b: &mut Builder<'_>, t: QTableId, f: Tf) -> Result<(), SchemaError> {
    let owner_name = b
        .table_ref(t)
        .map(|x| x.name_en.clone())
        .unwrap_or_default();
    let table = b
        .table_mut(t)
        .ok_or_else(|| SchemaError::Config("internal: table id out of range".into()))?;
    let missing = f.into_table(table);
    for m in missing {
        b.diag(format!("{owner_name}: no template field {m}"));
    }
    Ok(())
}

fn line_number() -> QType {
    number(LINE_NUMBER_DIGITS, 0, true)
}

const SIDES: [(super::Side, &str); 2] = [(super::Side::Dr, "Дт"), (super::Side::Cr, "Кт")];

fn dims(f: &mut Tf, r: &Reg<'_>, split: bool) {
    for d in &r.meta.dims {
        if split && !d.balance {
            for (side, sfx) in SIDES {
                f.sided(side, |f| {
                    f.derived(
                        P_DIM,
                        "",
                        &d.name,
                        sfx,
                        FieldRole::Dimension,
                        d.ty.clone(),
                        Some(d.id),
                    );
                });
            }
        } else {
            f.item(d, FieldRole::Dimension);
        }
    }
}

fn resources(f: &mut Tf, r: &Reg<'_>, split: bool) {
    for x in &r.meta.resources {
        if split && !x.balance {
            for (side, sfx) in SIDES {
                f.sided(side, |f| {
                    f.derived(
                        P_RES,
                        "",
                        &x.name,
                        sfx,
                        FieldRole::Resource,
                        x.ty.clone(),
                        Some(x.id),
                    );
                });
            }
        } else {
            f.item(x, FieldRole::Resource);
        }
    }
}

fn attributes(f: &mut Tf, r: &Reg<'_>) {
    for a in &r.meta.attrs {
        f.item(a, FieldRole::Attribute);
    }
}

fn main_table(b: &mut Builder<'_>, r: &Reg<'_>) -> Result<(), SchemaError> {
    let tpl = templates::find(b.cat, &r.key(""), r.nc());
    let kind = r.obj.kind;
    let mut f = Tf::new(kind, tpl);
    let dt = QType::date(DateQ::DateTime);
    match kind {
        MdKind::AccumulationRegister => {
            f.std(StdRole::Period, dt);
            f.std(StdRole::Recorder, r.recorder.clone());
            f.std(StdRole::LineNumber, line_number());
            f.std(StdRole::Active, QType::boolean());
            if r.meta.balance_type {
                f.std(StdRole::RecordKind, r.record_kind.clone());
            }
            dims(&mut f, r, false);
            resources(&mut f, r, false);
            attributes(&mut f, r);
            f.commons(&r.commons);
            f.virt(StdRole::PointInTime, QType::other(Other::PointInTime));
        }
        MdKind::InformationRegister => {
            information_fields(&mut f, r);
            if r.meta.subordinate {
                f.virt(StdRole::PointInTime, QType::other(Other::PointInTime));
            }
        }
        MdKind::AccountingRegister => {
            f.std(StdRole::Period, dt);
            f.std(StdRole::Recorder, r.recorder.clone());
            f.std(StdRole::LineNumber, line_number());
            f.std(StdRole::Active, QType::boolean());
            if r.meta.corr {
                f.sided(super::Side::Dr, |f| {
                    f.std(StdRole::AccountDr, r.account.clone())
                });
                f.sided(super::Side::Cr, |f| {
                    f.std(StdRole::AccountCr, r.account.clone())
                });
            } else {
                f.std(StdRole::RecordKind, r.record_kind.clone());
                f.std(StdRole::Account, r.account.clone());
            }
            dims(&mut f, r, r.meta.corr);
            resources(&mut f, r, r.meta.corr);
            attributes(&mut f, r);
            f.commons(&r.commons);
            if r.meta.adjustment {
                let len = u8::try_from(r.adjustment_len).unwrap_or(u8::MAX);
                f.std(StdRole::PeriodAdjustment, number(len, 0, true));
            }
            f.virt(StdRole::PointInTime, QType::other(Other::PointInTime));
        }
        MdKind::CalculationRegister => {
            calculation_fields(&mut f, r);
            f.named(
                names::std_name(StdRole::DataVersion, kind),
                FieldRole::Std(StdRole::DataVersion),
                string(DATA_VERSION_LEN, true),
                false,
                None,
            );
        }
        _ => {}
    }
    flush(b, r.table, f)
}

fn information_fields(f: &mut Tf, r: &Reg<'_>) {
    if r.meta.periodic {
        f.std(StdRole::Period, r.period.clone());
    }
    if r.meta.subordinate {
        f.std(StdRole::Recorder, r.recorder.clone());
        f.std(StdRole::LineNumber, line_number());
        f.std(StdRole::Active, QType::boolean());
    }
    dims(f, r, false);
    resources(f, r, false);
    attributes(f, r);
    f.commons(&r.commons);
}

fn calculation_fields(f: &mut Tf, r: &Reg<'_>) {
    let dt = || QType::date(DateQ::DateTime);
    f.std(StdRole::RegistrationPeriod, dt());
    f.std(StdRole::Recorder, r.recorder.clone());
    f.std(StdRole::LineNumber, line_number());
    f.std(StdRole::CalculationType, r.calc_type.clone());
    if r.meta.action_period {
        f.std(StdRole::ActionPeriod, dt());
        f.std(StdRole::BegOfActionPeriod, dt());
        f.std(StdRole::EndOfActionPeriod, dt());
    }
    if r.meta.base_period {
        f.std(StdRole::BegOfBasePeriod, dt());
        f.std(StdRole::EndOfBasePeriod, dt());
    }
    f.std(StdRole::Active, QType::boolean());
    f.std(StdRole::ReversingEntry, QType::boolean());
    dims(f, r, false);
    resources(f, r, false);
    attributes(f, r);
    f.commons(&r.commons);
}

fn add_vt(
    b: &mut Builder<'_>,
    r: &Reg<'_>,
    vt: VtKind,
    tpl: &QueryTableInfo,
    seg: (String, String),
    f: Tf,
) -> Result<QTableId, SchemaError> {
    let (mut t, extra) = sub_table(b, r.table, &seg, TableKind::Virtual(vt), None)?;
    t.params = match templates::params(tpl) {
        Ok(p) => p,
        Err(e) => {
            b.diag(e);
            Vec::new()
        }
    };
    let name = t.name_en.clone();
    let missing = f.into_table(&mut t);
    for m in missing {
        b.diag(format!("{name}: no template field {m}"));
    }
    b.add_table(t, &extra)
}

fn vt_template<'c>(b: &mut Builder<'c>, r: &Reg<'_>, suffix: &str) -> Option<&'c QueryTableInfo> {
    let cat = b.cat;
    let t = templates::find(cat, &r.key(&format!(".{suffix}")), r.nc());
    if t.is_none() {
        b.diag(format!("{}: no query table template {suffix}", r.owner()));
    }
    t
}

fn simple_vt(
    b: &mut Builder<'_>,
    r: &Reg<'_>,
    vt: VtKind,
    suffix: &str,
    fill: impl FnOnce(&mut Tf),
) -> Result<(), SchemaError> {
    let Some(tpl) = vt_template(b, r, suffix) else {
        return Ok(());
    };
    let mut f = Tf::new(r.obj.kind, Some(tpl));
    fill(&mut f);
    add_vt(b, r, vt, tpl, templates::last_segment(tpl), f)?;
    Ok(())
}

fn period_fields(f: &mut Tf, r: &Reg<'_>) {
    let dt = || QType::date(DateQ::DateTime);
    let (ru, en) = names::std_name(StdRole::Period, r.obj.kind);
    f.period(ru.clone(), en.clone(), dt(), PeriodField::Period);
    for (unit, kw) in UNITS {
        let name = format!("{ru}{unit}");
        f.period(name.clone(), name, dt(), PeriodField::Unit(kw));
    }
    let (ru, en) = names::std_name(StdRole::Recorder, r.obj.kind);
    f.period(ru, en, r.recorder.clone(), PeriodField::Recorder);
    let (ru, en) = names::std_name(StdRole::LineNumber, r.obj.kind);
    f.period(ru, en, line_number(), PeriodField::LineNumber);
}

fn vt_number(x: &Item) -> QType {
    number(VT_DIGITS, x.ty.number.map_or(0, |n| n.scale), false)
}

fn sum_number(x: &Item) -> QType {
    let (digits, scale) = x.ty.number.map_or((0, 0), |n| (n.digits, n.scale));
    number(
        digits.saturating_add(SUM_EXTRA_DIGITS).min(VT_DIGITS),
        scale,
        false,
    )
}

fn resource_fields(f: &mut Tf, x: &Item, sufs: &[&str]) {
    for suf in sufs {
        f.derived(
            P_RES,
            "",
            &x.name,
            suf,
            FieldRole::Virtual,
            vt_number(x),
            Some(x.id),
        );
    }
}

fn accumulation_vts(b: &mut Builder<'_>, r: &Reg<'_>) -> Result<(), SchemaError> {
    let balance = r.meta.balance_type;
    if balance {
        simple_vt(b, r, VtKind::Balance, "Balance", |f| {
            dims(f, r, false);
            f.commons(&r.commons);
            for x in &r.meta.resources {
                resource_fields(f, x, &["Остаток"]);
            }
        })?;
    }
    simple_vt(b, r, VtKind::Turnovers, "Turnovers", |f| {
        dims(f, r, false);
        f.commons(&r.commons);
        period_fields(f, r);
        for x in &r.meta.resources {
            if balance {
                resource_fields(f, x, &["Оборот", "Приход", "Расход"]);
            } else {
                resource_fields(f, x, &["Оборот"]);
            }
        }
    })?;
    if balance {
        simple_vt(
            b,
            r,
            VtKind::BalanceAndTurnovers,
            "BalanceAndTurnovers",
            |f| {
                dims(f, r, false);
                f.commons(&r.commons);
                period_fields(f, r);
                for x in &r.meta.resources {
                    resource_fields(
                        f,
                        x,
                        &[
                            "НачальныйОстаток",
                            "Оборот",
                            "Приход",
                            "Расход",
                            "КонечныйОстаток",
                        ],
                    );
                }
            },
        )?;
    }
    Ok(())
}

fn information_vts(b: &mut Builder<'_>, r: &Reg<'_>) -> Result<(), SchemaError> {
    if !r.meta.periodic {
        return Ok(());
    }
    simple_vt(b, r, VtKind::SliceLast, "SliceLast", |f| {
        information_fields(f, r);
    })?;
    simple_vt(b, r, VtKind::SliceFirst, "SliceFirst", |f| {
        information_fields(f, r);
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Both,
    Dr,
    Cr,
}

impl Side {
    fn suffix(self) -> &'static str {
        match self {
            Side::Both => "",
            Side::Dr => "Дт",
            Side::Cr => "Кт",
        }
    }

    fn stored(self) -> Option<super::Side> {
        match self {
            Side::Both => None,
            Side::Dr => Some(super::Side::Dr),
            Side::Cr => Some(super::Side::Cr),
        }
    }

    fn account(self) -> StdRole {
        match self {
            Side::Both => StdRole::Account,
            Side::Dr => StdRole::AccountDr,
            Side::Cr => StdRole::AccountCr,
        }
    }

    fn ext(self, n: u8) -> StdRole {
        match self {
            Side::Both => StdRole::ExtDimension(n),
            Side::Dr => StdRole::ExtDimensionDr(n),
            Side::Cr => StdRole::ExtDimensionCr(n),
        }
    }

    fn ext_kind(self, n: u8) -> StdRole {
        match self {
            Side::Both => StdRole::ExtDimensionType(n),
            Side::Dr => StdRole::ExtDimensionTypeDr(n),
            Side::Cr => StdRole::ExtDimensionTypeCr(n),
        }
    }
}

fn account_fields(f: &mut Tf, r: &Reg<'_>, side: Side, kinds: bool) {
    match side.stored() {
        Some(sd) => f.sided(sd, |f| account_fields_of(f, r, side, kinds)),
        None => account_fields_of(f, r, side, kinds),
    }
}

fn account_fields_of(f: &mut Tf, r: &Reg<'_>, side: Side, kinds: bool) {
    f.std(side.account(), r.account.clone());
    let sfx = side.suffix();
    for n in 1..=r.meta.ext_dims {
        f.derived(
            P_EXT,
            &format!("Субконто{sfx}"),
            &n.to_string(),
            "",
            FieldRole::Std(side.ext(n)),
            r.ext_value.clone(),
            None,
        );
        if kinds {
            f.derived(
                P_EXT,
                &format!("ВидСубконто{sfx}"),
                &n.to_string(),
                "",
                FieldRole::Std(side.ext_kind(n)),
                r.ext_kind.clone(),
                None,
            );
        }
    }
}

fn accounting_vts(b: &mut Builder<'_>, r: &Reg<'_>) -> Result<(), SchemaError> {
    let corr = r.meta.corr;
    let n = r.meta.ext_dims;
    simple_vt(b, r, VtKind::Balance, "Balance", |f| {
        account_fields(f, r, Side::Both, false);
        dims(f, r, false);
        f.commons(&r.commons);
        for x in &r.meta.resources {
            resource_fields(
                f,
                x,
                &[
                    "Остаток",
                    "ОстатокДт",
                    "ОстатокКт",
                    "РазвернутыйОстатокДт",
                    "РазвернутыйОстатокКт",
                ],
            );
        }
    })?;
    simple_vt(b, r, VtKind::Turnovers, "Turnovers", |f| {
        period_fields(f, r);
        account_fields(f, r, Side::Both, false);
        dims(f, r, false);
        f.commons(&r.commons);
        f.named(
            names::std_name(StdRole::BalancedAccount, r.obj.kind),
            FieldRole::Virtual,
            r.account.clone(),
            true,
            None,
        );
        for i in 1..=n {
            f.derived(
                P_EXT,
                "КорСубконто",
                &i.to_string(),
                "",
                FieldRole::Virtual,
                r.ext_value.clone(),
                None,
            );
        }
        if corr {
            for d in r.meta.dims.iter().filter(|d| !d.balance) {
                f.derived(
                    P_DIM,
                    "",
                    &d.name,
                    "Кор",
                    FieldRole::Virtual,
                    d.ty.clone(),
                    Some(d.id),
                );
            }
        }
        for x in &r.meta.resources {
            resource_fields(f, x, &["Оборот", "ОборотДт", "ОборотКт"]);
            if corr && !x.balance {
                resource_fields(f, x, &["КорОборот", "КорОборотДт", "КорОборотКт"]);
            }
        }
    })?;
    simple_vt(
        b,
        r,
        VtKind::BalanceAndTurnovers,
        "BalanceAndTurnovers",
        |f| {
            period_fields(f, r);
            account_fields(f, r, Side::Both, false);
            dims(f, r, false);
            f.commons(&r.commons);
            for x in &r.meta.resources {
                resource_fields(
                    f,
                    x,
                    &[
                        "НачальныйОстаток",
                        "НачальныйОстатокДт",
                        "НачальныйОстатокКт",
                        "НачальныйРазвернутыйОстатокДт",
                        "НачальныйРазвернутыйОстатокКт",
                        "Оборот",
                        "ОборотДт",
                        "ОборотКт",
                        "КонечныйОстаток",
                        "КонечныйОстатокДт",
                        "КонечныйОстатокКт",
                        "КонечныйРазвернутыйОстатокДт",
                        "КонечныйРазвернутыйОстатокКт",
                    ],
                );
            }
        },
    )?;
    if corr {
        simple_vt(b, r, VtKind::DrCrTurnovers, "DrCrTurnovers", |f| {
            period_fields(f, r);
            account_fields(f, r, Side::Dr, false);
            account_fields(f, r, Side::Cr, false);
            dims(f, r, true);
            f.commons(&r.commons);
            for x in &r.meta.resources {
                if x.balance {
                    resource_fields(f, x, &["Оборот"]);
                } else {
                    resource_fields(f, x, &["ОборотДт", "ОборотКт"]);
                }
            }
        })?;
    }
    simple_vt(
        b,
        r,
        VtKind::RecordsWithExtDimensions,
        "RecordsWithExtDimensions",
        |f| {
            f.std(StdRole::Period, QType::date(DateQ::DateTime));
            f.std(StdRole::Recorder, r.recorder.clone());
            f.std(StdRole::LineNumber, line_number());
            f.std(StdRole::Active, QType::boolean());
            if corr {
                account_fields(f, r, Side::Dr, true);
                account_fields(f, r, Side::Cr, true);
            } else {
                account_fields(f, r, Side::Both, true);
            }
            dims(f, r, corr);
            resources(f, r, corr);
            attributes(f, r);
            f.commons(&r.commons);
            if !corr {
                f.std(StdRole::RecordKind, r.record_kind.clone());
            }
            f.virt(StdRole::PointInTime, QType::other(Other::PointInTime));
        },
    )?;
    if n > 0 {
        simple_vt(b, r, VtKind::ExtDimensions, "ExtDimensions", |f| {
            f.std(StdRole::Period, QType::date(DateQ::DateTime));
            f.std(StdRole::Recorder, r.recorder.clone());
            f.std(StdRole::LineNumber, line_number());
            if corr {
                f.std(StdRole::RecordKind, r.record_kind.clone());
            }
            f.std(StdRole::ExtDimensionKind, r.ext_kind.clone());
            f.std(StdRole::Value, r.ext_value.clone());
            if r.meta.adjustment {
                let len = u8::try_from(r.adjustment_len).unwrap_or(u8::MAX);
                f.std(StdRole::PeriodAdjustment, number(len, 0, true));
            }
            f.virt(StdRole::PointInTime, QType::other(Other::PointInTime));
        })?;
    }
    Ok(())
}

fn main_star(b: &Builder<'_>, r: &Reg<'_>) -> Vec<QField> {
    b.table_ref(r.table)
        .map(|t| {
            t.star
                .iter()
                .filter_map(|i| t.fields.get(usize::from(i.0)).cloned())
                .collect()
        })
        .unwrap_or_default()
}

fn base_registers<'r, 'a>(
    r: &Reg<'a>,
    regs: &'r [Reg<'a>],
    objs: &ObjIndex<'a>,
) -> Vec<&'r Reg<'a>> {
    let Some(chart) = r.chart else {
        return Vec::new();
    };
    let depends = match text_prop(chart, "DependenceOnCalculationTypes") {
        Some(d) => d != "DontUse",
        None => chart.type_id("BaseCalculationTypes").is_some(),
    };
    if !depends {
        return Vec::new();
    }
    let charts: Vec<Guid> = match chart.prop("BaseCalculationTypes") {
        Some(Value::RefList(list)) if !list.is_empty() => list
            .iter()
            .filter_map(|x| objs.get(x))
            .map(|o| o.id)
            .collect(),
        _ => vec![chart.id],
    };
    regs.iter()
        .filter(|x| x.obj.kind == MdKind::CalculationRegister)
        .filter(|x| x.chart.is_some_and(|c| charts.contains(&c.id)))
        .collect()
}

fn calculation_vts(
    b: &mut Builder<'_>,
    r: &Reg<'_>,
    regs: &[Reg<'_>],
    objs: &ObjIndex<'_>,
) -> Result<(), SchemaError> {
    let star = main_star(b, r);
    if r.meta.action_period {
        simple_vt(
            b,
            r,
            VtKind::ActualActionPeriod,
            "AdjustedEffectivePeriod",
            |f| {
                for x in &star {
                    f.push(x.clone(), true);
                }
            },
        )?;
        if let Some(sr) = &r.meta.schedule_resource {
            simple_vt(b, r, VtKind::ScheduleData, "ScheduleData", |f| {
                for x in &star {
                    f.push(x.clone(), true);
                }
                let mut sufs = vec!["ПериодДействия", "ФактическийПериодДействия"];
                if r.meta.base_period {
                    sufs.push("БазовыйПериод");
                }
                sufs.push("ПериодРегистрации");
                for suf in sufs {
                    f.derived(
                        P_SCHED,
                        "",
                        &sr.name,
                        suf,
                        FieldRole::Virtual,
                        sum_number(sr),
                        Some(sr.id),
                    );
                }
            })?;
        }
    }
    let bases = base_registers(r, regs, objs);
    if bases.is_empty() {
        return Ok(());
    }
    let Some(tpl) = vt_template(b, r, "Base*") else {
        return Ok(());
    };
    let (ru, en) = templates::last_segment(tpl);
    for base in bases {
        let mut f = Tf::new(r.obj.kind, Some(tpl));
        for x in &star {
            f.push(x.clone(), true);
        }
        for x in &base.meta.resources {
            f.derived(
                P_BASE_RES,
                "",
                &x.name,
                "База",
                FieldRole::Virtual,
                sum_number(x),
                Some(x.id),
            );
        }
        let seg = (
            format!("{ru}{}", base.obj.name),
            format!("{en}{}", base.obj.name),
        );
        add_vt(b, r, VtKind::Base, tpl, seg, f)?;
    }
    Ok(())
}

fn recalculations(b: &mut Builder<'_>, r: &Reg<'_>) -> Result<(), SchemaError> {
    if r.obj.recalculations.is_empty() {
        return Ok(());
    }
    let cat = b.cat;
    let Some(tpl) = templates::find(cat, "CalculationRegister.*.*", false) else {
        b.diag(format!(
            "{}: no query table template of recalculations",
            r.owner()
        ));
        return Ok(());
    };
    for rc in &r.obj.recalculations {
        let mut f = Tf::new(r.obj.kind, Some(tpl));
        for d in &rc.dimensions {
            let reg_dim = r.meta.dims.iter().find(|x| {
                d.register_dimension.id == Some(x.id)
                    || (d.register_dimension.id.is_none()
                        && d.register_dimension
                            .name
                            .rsplit('.')
                            .next()
                            .is_some_and(|n| fold_name(n) == fold_name(&x.name)))
            });
            let ty = match reg_dim {
                Some(x) => x.ty.clone(),
                None => {
                    b.diag(format!(
                        "{}.{}.{}: dimension {} does not refer to a register dimension",
                        r.owner(),
                        rc.name,
                        d.name,
                        d.name
                    ));
                    QType::default()
                }
            };
            let it = Item {
                id: d.id,
                name: d.name.clone(),
                ty,
                balance: true,
                main_filter: true,
            };
            f.item(&it, FieldRole::Dimension);
        }
        f.std(StdRole::CalculationType, r.calc_type.clone());
        f.std(StdRole::RecalculationObject, r.recorder.clone());
        let (mut t, extra) = sub_table(
            b,
            r.table,
            &(rc.name.clone(), rc.name.clone()),
            TableKind::Recalculation,
            Some(rc.id),
        )?;
        let name = t.name_en.clone();
        for m in f.into_table(&mut t) {
            b.diag(format!("{name}: no template field {m}"));
        }
        b.add_table(t, &extra)?;
    }
    Ok(())
}
