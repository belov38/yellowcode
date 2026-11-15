use crate::Application;
use crate::meta::{arg_value, member_name, no_field, type_mismatch};
use crate::model::{
    AccumRegType, FieldDef, FieldRole, InfoRegWriteMode, ObjDef, ObjId, ObjKind, Periodicity,
    StdAttr,
};
use crate::recordset::{accepts_key_value, applies, field_type, is_structure};
use crate::select::value_mismatch;
use crate::session::internal;
use crate::types::Category;
use crate::values::field::default_of;
use crate::values::refs::{RefValue, as_ref};
use bsl_prim::{BslDate, fold_name};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, MemberKind, TypeId, Value,
};
use md_model::MdKind;
use platform_catalog::messages::Lang;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

const FIELD_BASE: u32 = 1 << 20;

pub(crate) fn serves(kind: ObjKind, ru: &str) -> bool {
    match kind {
        ObjKind::InformationRegister => matches!(
            ru,
            "Выбрать"
                | "СрезПоследних"
                | "СрезПервых"
                | "ПолучитьПоследнее"
                | "ПолучитьПервое"
                | "СоздатьМенеджерЗаписи"
        ),
        ObjKind::AccumulationRegister => matches!(
            ru,
            "Остатки" | "Обороты" | "Выбрать" | "ВыбратьПоРегистратору" | "СоздатьНаборЗаписей"
        ),
        ObjKind::DocumentJournal => ru == "Выбрать",
        _ => false,
    }
}

pub(crate) fn manager_call(
    ex: &mut Execution,
    app: &Arc<Application>,
    obj: ObjId,
    ru: &str,
    args: &[Arg],
) -> CallResult<Value> {
    let reg = Reg::new(app, obj, ru);
    reg.check_shape()?;
    let a = |ex: &mut Execution, i| arg_value(ex, args, i);
    match (reg.def.kind, ru) {
        (ObjKind::InformationRegister, "СрезПоследних" | "СрезПервых") => {
            reg.only_args(args, 2)?;
            if !reg.periodic() {
                if args.iter().all(|x| matches!(x, Arg::Omitted)) {
                    return Ok(Value::Undefined);
                }
                return Err(reg.refuse("непериодический регистр с параметрами").into());
            }
            let (p, f) = (a(ex, 0)?, a(ex, 1)?);
            info_slice(ex, &reg, &p, &f)
        }
        (ObjKind::InformationRegister, "ПолучитьПоследнее" | "ПолучитьПервое") =>
        {
            let (p, f) = (a(ex, 0)?, a(ex, 1)?);
            reg.only_args(args, 2)?;
            info_get(ex, &reg, &p, &f)
        }
        (ObjKind::InformationRegister, "Выбрать") => {
            if reg.periodic() {
                let (b, e, f, o) = (a(ex, 0)?, a(ex, 1)?, a(ex, 2)?, a(ex, 3)?);
                reg.only_args(args, 4)?;
                info_select(ex, &reg, &b, &e, (&f, 3), &o)
            } else {
                let (f, o) = (a(ex, 0)?, a(ex, 1)?);
                reg.only_args(args, 2)?;
                info_select(ex, &reg, &Value::Undefined, &Value::Undefined, (&f, 1), &o)
            }
        }
        (ObjKind::InformationRegister, "СоздатьМенеджерЗаписи") => {
            reg.only_args(args, 0)?;
            RecordManager::create(ex, &reg)
        }
        (ObjKind::AccumulationRegister, "Остатки") => {
            let (p, f, d, r) = (a(ex, 0)?, a(ex, 1)?, a(ex, 2)?, a(ex, 3)?);
            reg.only_args(args, 4)?;
            accum_balance(ex, &reg, &p, &f, &d, &r)
        }
        (ObjKind::AccumulationRegister, "Обороты") => {
            let (b, e, f) = (a(ex, 0)?, a(ex, 1)?, a(ex, 2)?);
            let (d, r) = (a(ex, 3)?, a(ex, 4)?);
            reg.only_args(args, 5)?;
            accum_turnovers(ex, &reg, &b, &e, &f, &d, &r)
        }
        (ObjKind::AccumulationRegister, "Выбрать") => {
            let (b, e, f, o) = (a(ex, 0)?, a(ex, 1)?, a(ex, 2)?, a(ex, 3)?);
            reg.only_args(args, 4)?;
            accum_select(ex, &reg, &b, &e, &f, &o)
        }
        (ObjKind::AccumulationRegister, "ВыбратьПоРегистратору") => {
            let r = a(ex, 0)?;
            reg.only_args(args, 1)?;
            accum_by_recorder(ex, &reg, &r)
        }
        (ObjKind::AccumulationRegister, "СоздатьНаборЗаписей") => {
            reg.only_args(args, 0)?;
            AccumSet::create(ex, app, obj, Recorder::Unset)
        }
        (ObjKind::DocumentJournal, "Выбрать") => {
            let (b, e, f, o) = (a(ex, 0)?, a(ex, 1)?, a(ex, 2)?, a(ex, 3)?);
            reg.only_args(args, 4)?;
            reg.absent(&f, "Отбор")?;
            reg.absent(&o, "Порядок")?;
            journal_select(ex, &reg, &b, &e)
        }
        _ => Err(reg.refuse("метод").into()),
    }
}

struct Reg<'a> {
    app: &'a Arc<Application>,
    obj: ObjId,
    def: &'a ObjDef,
    method: &'a str,
}

impl<'a> Reg<'a> {
    fn new(app: &'a Arc<Application>, obj: ObjId, method: &'a str) -> Reg<'a> {
        Reg {
            app,
            obj,
            def: app.model().obj(obj),
            method,
        }
    }

    fn refuse(&self, what: &str) -> HostError {
        HostError::not_implemented(
            &format!("{}Менеджер", self.def.kind.name(Lang::Ru)),
            &format!("{}: {what}", self.method),
        )
    }

    fn periodic(&self) -> bool {
        !matches!(
            self.def.behavior.periodicity,
            None | Some(Periodicity::Nonperiodical)
        )
    }

    fn check_shape(&self) -> CallResult<()> {
        if self.app.model().script != md_model::ScriptVariant::Russian {
            return Err(self.refuse("английский вариант встроенного языка").into());
        }
        let md = match self.def.kind {
            ObjKind::InformationRegister => MdKind::InformationRegister,
            ObjKind::AccumulationRegister => MdKind::AccumulationRegister,
            ObjKind::DocumentJournal => return Ok(()),
            _ => return Err(self.refuse("вид объекта").into()),
        };
        let full = format!("{}.{}", md.as_str(), self.def.name);
        let common = self
            .app
            .model()
            .config
            .objects
            .iter()
            .filter(|o| o.kind == MdKind::CommonAttribute)
            .any(|ca| applies(ca, &full));
        if common {
            return Err(self.refuse("общий реквизит").into());
        }
        if self.fields(FieldRole::Attribute).next().is_some() {
            return Err(self.refuse("реквизиты регистра").into());
        }
        if self.def.kind == ObjKind::InformationRegister {
            if self.def.behavior.write_mode == Some(InfoRegWriteMode::RecorderSubordinate) {
                return Err(self.refuse("подчиненный регистратору").into());
            }
            if !matches!(
                self.def.behavior.periodicity,
                None | Some(Periodicity::Nonperiodical | Periodicity::Day)
            ) {
                return Err(self.refuse("периодичность").into());
            }
        }
        Ok(())
    }

    fn fields(&self, role: FieldRole) -> impl Iterator<Item = (usize, &'a FieldDef)> + 'a {
        self.def
            .fields
            .iter()
            .enumerate()
            .filter(move |(_, f)| f.std.is_none() && f.role == role)
    }

    fn std(&self, a: StdAttr) -> Option<(usize, &'a FieldDef)> {
        self.def
            .fields
            .iter()
            .enumerate()
            .find(|(_, f)| f.std == Some(a))
    }

    fn table(&self) -> String {
        format!("{}.{}", self.def.kind.name(Lang::Ru), self.def.name)
    }

    fn only_args(&self, args: &[Arg], n: usize) -> CallResult<()> {
        if args.len() > n {
            return Err(self.refuse("лишний параметр").into());
        }
        Ok(())
    }

    fn absent(&self, v: &Value, name: &str) -> CallResult<()> {
        match v {
            Value::Undefined => Ok(()),
            _ => Err(self.refuse(&format!("параметр {name}")).into()),
        }
    }

    fn period(&self, v: &Value, name: &str) -> CallResult<Option<Value>> {
        let catalog = platform_catalog::catalog();
        let is = |t: &str| {
            catalog
                .find_type(t)
                .is_some_and(|t| v.type_id() == TypeId::of_catalog(t))
        };
        match v {
            Value::Undefined => Ok(None),
            Value::Date(_) => Ok(Some(v.clone())),
            _ if is("МоментВремени") || is("Граница") => Ok(Some(v.clone())),
            _ => Err(self.refuse(&format!("{name}: тип периода")).into()),
        }
    }

    fn date(&self, v: &Value, name: &str) -> CallResult<Option<BslDate>> {
        match v {
            Value::Undefined => Ok(None),
            Value::Date(d) => Ok(Some(*d)),
            _ => Err(self
                .refuse(&format!(
                    "{name} не Дата (МоментВремени и Граница не записаны)"
                ))
                .into()),
        }
    }

    fn filter(
        &self,
        v: &Value,
        allowed: impl Fn(&FieldDef) -> bool,
        errors: Option<usize>,
    ) -> CallResult<Vec<(usize, Value)>> {
        if matches!(v, Value::Undefined) {
            return Ok(Vec::new());
        }
        if !is_structure(v) {
            let fixed = platform_catalog::catalog()
                .find_type("ФиксированнаяСтруктура")
                .is_some_and(|t| v.type_id() == TypeId::of_catalog(t));
            return Err(match errors {
                Some(n) if fixed => type_mismatch(n),
                _ => self.refuse("Отбор не Структура"),
            }
            .into());
        }
        let pairs = bsl_stdlib::structure_fields(v).ok_or_else(|| internal("a structure"))?;
        let mut out: Vec<(usize, Value)> = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            let found = self
                .fields(FieldRole::Dimension)
                .find(|(_, f)| fold_name(&f.name) == fold_name(&key));
            let Some((i, f)) = found.filter(|(_, f)| allowed(f)) else {
                return Err(match errors {
                    Some(n) => value_mismatch(n),
                    None => self.refuse(&format!("Отбор по полю {key}")),
                }
                .into());
            };
            if matches!(value, Value::Undefined | Value::Null) {
                return Err(self.refuse(&format!("Отбор: значение поля {key}")).into());
            }
            if !accepts_key_value(self.app, &f.types, &value) {
                return Err(match errors {
                    Some(n) => value_mismatch(n),
                    None => self.refuse(&format!("Отбор: значение поля {key}")),
                }
                .into());
            }
            if out.iter().any(|(j, _)| *j == i) {
                return Err(self.refuse(&format!("Отбор: поле {key} дважды")).into());
            }
            out.push((i, value));
        }
        Ok(out)
    }

    fn order(&self, v: &Value) -> CallResult<bool> {
        match v {
            Value::Undefined => Ok(false),
            Value::String(s) => match fold_name(s.to_string_lossy().trim()).as_str() {
                x if x == fold_name("Возр") => Ok(false),
                x if x == fold_name("Убыв") => Ok(true),
                _ => Err(self.refuse("Порядок по полю").into()),
            },
            _ => Err(self.refuse("Порядок не Строка").into()),
        }
    }

    fn subset(
        &self,
        v: &Value,
        role: FieldRole,
        name: &str,
    ) -> CallResult<Vec<(usize, &'a FieldDef)>> {
        let all: Vec<(usize, &FieldDef)> = self.fields(role).collect();
        let text = match v {
            Value::Undefined => return Ok(all),
            Value::String(s) => s.to_string_lossy(),
            _ => return Err(self.refuse(&format!("{name} не Строка")).into()),
        };
        let wanted: Vec<String> = text
            .split(',')
            .map(|p| fold_name(p.trim()))
            .filter(|p| !p.is_empty())
            .collect();
        if wanted.is_empty() {
            return Ok(all);
        }
        for w in &wanted {
            if !all.iter().any(|(_, f)| fold_name(&f.name) == *w) {
                return Err(self.refuse(&format!("{name}: неизвестное имя")).into());
            }
        }
        Ok(all
            .into_iter()
            .filter(|(_, f)| wanted.contains(&fold_name(&f.name)))
            .collect())
    }

    fn name(&self, i: usize) -> CallResult<&'a str> {
        Ok(self
            .def
            .fields
            .get(i)
            .ok_or_else(|| internal("no field of the register"))?
            .name
            .as_str())
    }
}

#[derive(Default)]
struct Stmt {
    params: Vec<(String, Value)>,
}

impl Stmt {
    fn param(&mut self, v: Value) -> String {
        let name = format!("П{}", self.params.len());
        self.params.push((name.clone(), v));
        format!("&{name}")
    }

    fn equalities(
        &mut self,
        reg: &Reg,
        prefix: &str,
        pairs: &[(usize, Value)],
    ) -> CallResult<Vec<String>> {
        let mut out = Vec::with_capacity(pairs.len());
        for (i, v) in pairs {
            let p = self.param(v.clone());
            out.push(format!("{prefix}{} = {p}", reg.name(*i)?));
        }
        Ok(out)
    }

    fn condition(&mut self, reg: &Reg, pairs: &[(usize, Value)]) -> CallResult<Option<String>> {
        let cond = self.equalities(reg, "", pairs)?;
        Ok((!cond.is_empty()).then(|| cond.join(" И ")))
    }

    fn run(self, ex: &mut Execution, text: &str) -> CallResult<Value> {
        let mut a = [Arg::Val(Value::from(text))];
        let q = ex.construct_by_name("Запрос", &mut a)?;
        for (name, v) in self.params {
            let mut a = [Arg::Val(Value::from(name.as_str())), Arg::Val(v)];
            ex.call_method(&q, "УстановитьПараметр", &mut a, false)?;
        }
        Ok(ex.call_method(&q, "Выполнить", &mut [], true)?)
    }
}

fn unload(ex: &mut Execution, result: &Value) -> CallResult<Value> {
    Ok(ex.call_method(result, "Выгрузить", &mut [], true)?)
}

fn select_list(names: &[String]) -> String {
    names
        .iter()
        .map(|n| format!("Т.{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn order_list(names: &[String], desc: bool) -> String {
    let dir = if desc { " УБЫВ" } else { "" };
    names
        .iter()
        .map(|n| format!("Т.{n}{dir}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn names_of<'a>(it: impl Iterator<Item = (usize, &'a FieldDef)>) -> Vec<String> {
    it.map(|(_, f)| f.name.clone()).collect()
}

fn vt_params(parts: &[Option<String>]) -> String {
    let last = parts.iter().rposition(Option::is_some).map_or(0, |i| i + 1);
    let shown: Vec<&str> = parts
        .iter()
        .take(last)
        .map(|p| p.as_deref().unwrap_or(""))
        .collect();
    shown.join(", ")
}

fn where_of(cond: &[String]) -> String {
    if cond.is_empty() {
        String::new()
    } else {
        format!(" ГДЕ {}", cond.join(" И "))
    }
}

fn info_slice(ex: &mut Execution, reg: &Reg, period: &Value, filter: &Value) -> CallResult<Value> {
    let date = reg.date(period, "Период")?;
    let pairs = reg.filter(filter, |_| true, None)?;
    let mut st = Stmt::default();
    let p = date.map(|d| st.param(Value::Date(d)));
    let cond = st.condition(reg, &pairs)?;
    let mut cols = Vec::new();
    if let Some((_, f)) = reg.std(StdAttr::Period) {
        cols.push(f.name.clone());
    }
    cols.extend(names_of(reg.fields(FieldRole::Dimension)));
    cols.extend(names_of(reg.fields(FieldRole::Resource)));
    let text = format!(
        "ВЫБРАТЬ {} ИЗ {}.{}({}) КАК Т",
        select_list(&cols),
        reg.table(),
        reg.method,
        vt_params(&[p, cond])
    );
    let result = st.run(ex, &text)?;
    unload(ex, &result)
}

fn info_get(ex: &mut Execution, reg: &Reg, period: &Value, filter: &Value) -> CallResult<Value> {
    if !reg.periodic() {
        return Err(reg.refuse("непериодический регистр").into());
    }
    let at = reg.period(period, "Период")?;
    let pairs = reg.filter(filter, |_| true, None)?;
    let vt = if reg.method == "ПолучитьПоследнее" {
        "СрезПоследних"
    } else {
        "СрезПервых"
    };
    let mut st = Stmt::default();
    let p = at.map(|v| st.param(v));
    let cond = st.condition(reg, &pairs)?;
    let res: Vec<(usize, &FieldDef)> = reg.fields(FieldRole::Resource).collect();
    let names: Vec<String> = res.iter().map(|(_, f)| f.name.clone()).collect();
    let text = format!(
        "ВЫБРАТЬ {} ИЗ {}.{vt}({}) КАК Т",
        select_list(&names),
        reg.table(),
        vt_params(&[p, cond])
    );
    let result = st.run(ex, &text)?;
    let table = unload(ex, &result)?;
    let row = match count(ex, &table)? {
        0 => None,
        1 => Some(row_at(ex, &table, 0)?),
        _ => return Err(reg.refuse("несколько записей среза").into()),
    };
    let out = ex.construct_by_name("Структура", &mut [])?;
    for (_, f) in res {
        let v = match &row {
            Some(row) => ex.get_member(row, &f.name)?,
            None => default_of(reg.app, f)?,
        };
        let mut a = [Arg::Val(Value::from(f.name.as_str())), Arg::Val(v)];
        ex.call_method(&out, "Вставить", &mut a, false)?;
    }
    Ok(out)
}

fn info_select(
    ex: &mut Execution,
    reg: &Reg,
    begin: &Value,
    end: &Value,
    filter: (&Value, usize),
    order: &Value,
) -> CallResult<Value> {
    let desc = reg.order(order)?;
    let (b, e) = (reg.date(begin, "Начало")?, reg.date(end, "Конец")?);
    let master = |f: &FieldDef| f.indexed || reg_child_flag(reg, f, "Master");
    let pairs = reg.filter(filter.0, master, Some(filter.1))?;
    if pairs.len() > 1 {
        return Err(value_mismatch(filter.1).into());
    }
    let mut st = Stmt::default();
    let mut cond = Vec::new();
    let period = reg.std(StdAttr::Period).map(|(_, f)| f.name.clone());
    if let Some(name) = &period {
        if let Some(d) = b {
            cond.push(format!("Т.{name} >= {}", st.param(Value::Date(d))));
        }
        if let Some(d) = e {
            cond.push(format!("Т.{name} <= {}", st.param(Value::Date(d))));
        }
    }
    cond.extend(st.equalities(reg, "Т.", &pairs)?);
    let mut cols: Vec<String> = period.iter().cloned().collect();
    let dims = names_of(reg.fields(FieldRole::Dimension));
    cols.extend(dims.iter().cloned());
    cols.extend(names_of(reg.fields(FieldRole::Resource)));
    let mut ord: Vec<String> = period.iter().cloned().collect();
    ord.extend(dims);
    let text = format!(
        "ВЫБРАТЬ {} ИЗ {} КАК Т{} УПОРЯДОЧИТЬ ПО {}",
        select_list(&cols),
        reg.table(),
        where_of(&cond),
        order_list(&ord, desc)
    );
    let result = st.run(ex, &text)?;
    Selection::create(ex, reg, &result, cols)
}

fn reg_child_flag(reg: &Reg, f: &FieldDef, name: &str) -> bool {
    let Some(id) = f.md else { return false };
    reg.app
        .model()
        .config
        .object(reg.def.md)
        .and_then(|o| o.children.iter().find(|c| c.id == id))
        .and_then(|c| c.props.iter().find(|p| p.name == name))
        .is_some_and(|p| matches!(p.value, md_model::Value::Bool(true)))
}

fn accum_balance(
    ex: &mut Execution,
    reg: &Reg,
    at: &Value,
    filter: &Value,
    dims: &Value,
    resources: &Value,
) -> CallResult<Value> {
    if reg.def.behavior.register_type != Some(AccumRegType::Balance) {
        let t = "Для оборотного регистра данная операция недоступна!";
        return Err(HostError::text(t, t).into());
    }
    let at = reg.period(at, "МоментВремени")?;
    let pairs = reg.filter(filter, |_| true, Some(2))?;
    let dims = reg.subset(dims, FieldRole::Dimension, "Измерения")?;
    let resources = reg.subset(resources, FieldRole::Resource, "Ресурсы")?;
    let mut st = Stmt::default();
    let p = at.map(|v| st.param(v));
    let cond = st.condition(reg, &pairs)?;
    let mut cols: Vec<String> = dims.iter().map(|(_, f)| format!("Т.{}", f.name)).collect();
    for (_, f) in &resources {
        cols.push(format!("Т.{0}Остаток КАК {0}", f.name));
    }
    let text = format!(
        "ВЫБРАТЬ {} ИЗ {}.Остатки({}) КАК Т",
        cols.join(", "),
        reg.table(),
        vt_params(&[p, cond])
    );
    let result = st.run(ex, &text)?;
    unload(ex, &result)
}

fn accum_turnovers(
    ex: &mut Execution,
    reg: &Reg,
    begin: &Value,
    end: &Value,
    filter: &Value,
    dims: &Value,
    resources: &Value,
) -> CallResult<Value> {
    let b = reg.period(begin, "Начало")?;
    let e = reg.period(end, "Конец")?;
    let pairs = reg.filter(filter, |_| true, None)?;
    let dims = reg.subset(dims, FieldRole::Dimension, "Измерения")?;
    let resources = reg.subset(resources, FieldRole::Resource, "Ресурсы")?;
    let mut st = Stmt::default();
    let b = b.map(|v| st.param(v));
    let e = e.map(|v| st.param(v));
    let cond = st.condition(reg, &pairs)?;
    let mut cols: Vec<String> = dims.iter().map(|(_, f)| format!("Т.{}", f.name)).collect();
    let balance = reg.def.behavior.register_type == Some(AccumRegType::Balance);
    for (_, f) in &resources {
        if balance {
            cols.push(format!("Т.{0}Приход КАК {0}Приход", f.name));
            cols.push(format!("Т.{0}Расход КАК {0}Расход", f.name));
        } else {
            cols.push(format!("Т.{0}Оборот КАК {0}", f.name));
        }
    }
    let text = format!(
        "ВЫБРАТЬ {} ИЗ {}.Обороты({}) КАК Т",
        cols.join(", "),
        reg.table(),
        vt_params(&[b, e, None, cond])
    );
    let result = st.run(ex, &text)?;
    unload(ex, &result)
}

fn accum_columns(reg: &Reg) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for a in [
        StdAttr::Period,
        StdAttr::Recorder,
        StdAttr::LineNumber,
        StdAttr::Active,
        StdAttr::RecordType,
    ] {
        if let Some((i, f)) = reg.std(a) {
            out.push((i, f.name.clone()));
        }
    }
    for role in [FieldRole::Dimension, FieldRole::Resource] {
        out.extend(reg.fields(role).map(|(i, f)| (i, f.name.clone())));
    }
    out
}

fn accum_select(
    ex: &mut Execution,
    reg: &Reg,
    begin: &Value,
    end: &Value,
    filter: &Value,
    order: &Value,
) -> CallResult<Value> {
    let desc = reg.order(order)?;
    let (b, e) = (reg.date(begin, "Начало")?, reg.date(end, "Конец")?);
    let pairs = reg.filter(filter, |f| f.indexed, None)?;
    if pairs.len() > 1 {
        return Err(reg.refuse("Отбор из нескольких полей").into());
    }
    let cols: Vec<String> = accum_columns(reg).into_iter().map(|(_, n)| n).collect();
    let period = reg.name(reg.std(StdAttr::Period).map_or(usize::MAX, |(i, _)| i))?;
    let mut st = Stmt::default();
    let mut cond = Vec::new();
    if let Some(d) = b {
        cond.push(format!("Т.{period} >= {}", st.param(Value::Date(d))));
    }
    if let Some(d) = e {
        cond.push(format!("Т.{period} <= {}", st.param(Value::Date(d))));
    }
    cond.extend(st.equalities(reg, "Т.", &pairs)?);
    let order = order_list(&record_order(reg)?, desc);
    let text = format!(
        "ВЫБРАТЬ {} ИЗ {} КАК Т{} УПОРЯДОЧИТЬ ПО {order}",
        select_list(&cols),
        reg.table(),
        where_of(&cond)
    );
    let result = st.run(ex, &text)?;
    Selection::create(ex, reg, &result, cols)
}

fn record_order(reg: &Reg) -> CallResult<Vec<String>> {
    let mut names = Vec::new();
    for a in [StdAttr::Period, StdAttr::Recorder, StdAttr::LineNumber] {
        let (i, _) = reg
            .std(a)
            .ok_or_else(|| internal("a standard field of a register"))?;
        names.push(reg.name(i)?.to_string());
    }
    Ok(names)
}

fn recorder_of(app: &Application, register: ObjId, v: &Value) -> Option<RefValue> {
    let r = as_ref(v)?;
    let doc = app.model().obj(r.obj);
    let md = app.model().obj(register).md;
    (doc.kind == ObjKind::Document && doc.behavior.register_records.contains(&md)).then_some(r)
}

fn document_of(app: &Application, v: &Value) -> Option<RefValue> {
    let r = as_ref(v)?;
    (app.model().obj(r.obj).kind == ObjKind::Document && !r.is_empty()).then_some(r)
}

fn accum_by_recorder(ex: &mut Execution, reg: &Reg, recorder: &Value) -> CallResult<Value> {
    if document_of(reg.app, recorder).is_none() {
        return Err(reg.refuse("Регистратор не ссылка на документ").into());
    }
    let all = accum_columns(reg);
    let cols: Vec<String> = all
        .iter()
        .filter(|(i, _)| reg.def.fields.get(*i).and_then(|f| f.std) != Some(StdAttr::Recorder))
        .map(|(_, n)| n.clone())
        .collect();
    let (Some((rec, _)), Some((line, _))) =
        (reg.std(StdAttr::Recorder), reg.std(StdAttr::LineNumber))
    else {
        return Err(internal("a standard field of a register").into());
    };
    let mut st = Stmt::default();
    let p = st.param(recorder.clone());
    let text = format!(
        "ВЫБРАТЬ {} ИЗ {} КАК Т ГДЕ Т.{} = {p} УПОРЯДОЧИТЬ ПО Т.{}",
        select_list(&cols),
        reg.table(),
        reg.name(rec)?,
        reg.name(line)?
    );
    let result = st.run(ex, &text)?;
    Selection::create(ex, reg, &result, cols)
}

fn journal_select(ex: &mut Execution, reg: &Reg, begin: &Value, end: &Value) -> CallResult<Value> {
    let (b, e) = (reg.date(begin, "Начало")?, reg.date(end, "Конец")?);
    let mut cols: Vec<String> = ["Ссылка", "Дата", "Номер", "Проведен", "ПометкаУдаления"]
        .iter()
        .map(ToString::to_string)
        .collect();
    cols.extend(
        reg.def
            .fields
            .iter()
            .filter(|f| f.std.is_none())
            .map(|f| f.name.clone()),
    );
    let mut st = Stmt::default();
    let mut cond = Vec::new();
    if let Some(d) = b {
        cond.push(format!("Т.Дата >= {}", st.param(Value::Date(d))));
    }
    if let Some(d) = e {
        cond.push(format!("Т.Дата <= {}", st.param(Value::Date(d))));
    }
    let text = format!(
        "ВЫБРАТЬ {} ИЗ {} КАК Т{} УПОРЯДОЧИТЬ ПО Т.Дата, Т.Ссылка",
        select_list(&cols),
        reg.table(),
        where_of(&cond)
    );
    let result = st.run(ex, &text)?;
    Selection::create(ex, reg, &result, cols)
}

fn count(ex: &mut Execution, table: &Value) -> CallResult<usize> {
    match ex.call_method(table, "Количество", &mut [], true)? {
        Value::Number(n) => {
            Ok(usize::try_from(n.trunc_to_i64_saturating()).map_err(|_| internal("a count"))?)
        }
        _ => Err(internal("a count").into()),
    }
}

fn row_at(ex: &mut Execution, table: &Value, i: usize) -> CallResult<Value> {
    let n = i64::try_from(i).map_err(|_| internal("a row index"))?;
    let mut a = [Arg::Val(Value::from(n))];
    Ok(ex.call_method(table, "Получить", &mut a, true)?)
}

fn type_of(app: &Application, obj: ObjId, cat: Category) -> Result<TypeId, HostError> {
    app.types()
        .and_then(|t| t.type_of(obj, cat))
        .ok_or_else(|| internal("no applied type of the register"))
}

fn template_member(ex: &Execution, ty: TypeId, m: MemberId) -> Option<(bool, &'static str)> {
    let t = ex.runtime().types().app_type(ty)?.template?;
    member_name(platform_catalog::catalog(), t, m).map(|(method, ru, _)| (method, ru))
}

fn position(names: &[String], name: &str) -> Option<usize> {
    let wanted = fold_name(name);
    names.iter().position(|n| fold_name(n) == wanted)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum At {
    Before,
    Row,
    After,
}

struct Selection {
    ty: TypeId,
    type_name: String,
    inner: Value,
    cols: Vec<String>,
    at: Cell<At>,
}

impl Selection {
    fn create(
        ex: &mut Execution,
        reg: &Reg,
        result: &Value,
        cols: Vec<String>,
    ) -> CallResult<Value> {
        let inner = ex.call_method(result, "Выбрать", &mut [], true)?;
        crate::read_budget::reserve(
            ex,
            cols.iter()
                .fold(256u64, |n, c| n.saturating_add(c.len() as u64)),
        )?;
        Ok(Value::Object(Rc::new(Selection {
            ty: type_of(reg.app, reg.obj, Category::Selection)?,
            type_name: format!("{}Выборка", reg.def.kind.name(Lang::Ru)),
            inner,
            cols,
            at: Cell::new(At::Before),
        })))
    }

    fn refuse(&self, member: &str) -> HostError {
        HostError::not_implemented(&self.type_name, member)
    }

    fn field(&self, ex: &mut Execution, name: &str) -> CallResult<Value> {
        let Some(i) = position(&self.cols, name) else {
            return Err(self.refuse(name).into());
        };
        if self.at.get() != At::Row {
            return Ok(Value::Undefined);
        }
        let col = self
            .cols
            .get(i)
            .ok_or_else(|| internal("a column"))?
            .clone();
        Ok(ex.get_member(&self.inner, &col)?)
    }
}

impl HostObject for Selection {
    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn presentation(&self, _: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        Ok(Some(bsl_prim::Str::new()))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        if m.0 >= FIELD_BASE {
            let i = (m.0 - FIELD_BASE) as usize;
            let name = self
                .cols
                .get(i)
                .ok_or_else(|| internal("a column"))?
                .clone();
            return self.field(ex, &name);
        }
        match template_member(ex, self.ty, m) {
            Some((false, ru)) => self.field(ex, ru),
            Some((true, ru)) => Err(self.refuse(ru).into()),
            None => Err(self.refuse(&m.0.to_string()).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        let name = template_member(ex, self.ty, m).map_or_else(
            || {
                let i = m.0.wrapping_sub(FIELD_BASE) as usize;
                self.cols.get(i).cloned().unwrap_or_default()
            },
            |n| n.1.to_string(),
        );
        Err(self.refuse(&format!("{name} (запись)")).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match template_member(ex, self.ty, m) {
            Some((true, "Следующий")) if args.is_empty() => {
                if self.at.get() == At::After {
                    return Ok(Value::Bool(false));
                }
                let next = ex.call_method(&self.inner, "Следующий", &mut [], true)?;
                let Value::Bool(b) = next else {
                    return Err(internal("Следующий").into());
                };
                self.at.set(if b { At::Row } else { At::After });
                Ok(Value::Bool(b))
            }
            Some((_, ru)) => Err(self.refuse(ru).into()),
            None => Err(self.refuse(&m.0.to_string()).into()),
        }
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let i = position(&self.cols, name)?;
        FIELD_BASE.checked_add(u32::try_from(i).ok()?).map(MemberId)
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        let i = usize::try_from(m.0.checked_sub(FIELD_BASE)?).ok()?;
        (i < self.cols.len()).then_some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }
}

struct RecordManager {
    app: Arc<Application>,
    obj: ObjId,
    ty: TypeId,
    fields: Vec<usize>,
    values: RefCell<Vec<Value>>,
    selected: Cell<bool>,
    read_key: RefCell<Option<Vec<Value>>>,
    touched: RefCell<Vec<bool>>,
}

impl RecordManager {
    fn create(ex: &mut Execution, reg: &Reg) -> CallResult<Value> {
        let mut fields: Vec<usize> = reg
            .std(StdAttr::Period)
            .map(|(i, _)| i)
            .into_iter()
            .collect();
        fields.extend(reg.fields(FieldRole::Dimension).map(|(i, _)| i));
        fields.extend(reg.fields(FieldRole::Resource).map(|(i, _)| i));
        let mut values = Vec::with_capacity(fields.len());
        for &i in &fields {
            let f = reg.def.fields.get(i).ok_or_else(|| internal("a field"))?;
            values.push(default_of(reg.app, f)?);
        }
        crate::read_budget::reserve(ex, 256u64.saturating_add(64 * fields.len() as u64))?;
        let n = fields.len();
        Ok(Value::Object(Rc::new(RecordManager {
            app: reg.app.clone(),
            obj: reg.obj,
            ty: type_of(reg.app, reg.obj, Category::RecordManager)?,
            fields,
            values: RefCell::new(values),
            selected: Cell::new(false),
            read_key: RefCell::new(None),
            touched: RefCell::new(vec![false; n]),
        })))
    }

    fn def(&self) -> &ObjDef {
        self.app.model().obj(self.obj)
    }

    fn refuse(&self, member: &str) -> HostError {
        HostError::not_implemented("РегистрСведенийМенеджерЗаписи", member)
    }

    fn slot(&self, name: &str) -> Option<usize> {
        let def = self.def();
        let wanted = fold_name(name);
        self.fields.iter().position(|&i| {
            def.fields
                .get(i)
                .is_some_and(|f| fold_name(&f.name) == wanted)
        })
    }

    fn slot_of(&self, ex: &Execution, m: MemberId) -> Option<usize> {
        if m.0 >= FIELD_BASE {
            let s = (m.0 - FIELD_BASE) as usize;
            return (s < self.fields.len()).then_some(s);
        }
        match template_member(ex, self.ty, m) {
            Some((false, ru)) => self.slot(ru),
            _ => None,
        }
    }

    fn member_ru(&self, ex: &Execution, m: MemberId) -> String {
        template_member(ex, self.ty, m).map_or_else(|| m.0.to_string(), |n| n.1.to_string())
    }

    fn read(&self, ex: &mut Execution) -> CallResult<()> {
        let app = self.app.clone();
        let reg = Reg::new(&app, self.obj, "Прочитать");
        let values = self.values.borrow().clone();
        let mut pairs = Vec::new();
        let mut names = Vec::with_capacity(self.fields.len());
        let mut undefined = false;
        for (slot, &i) in self.fields.iter().enumerate() {
            let f = reg.def.fields.get(i).ok_or_else(|| internal("a field"))?;
            names.push(f.name.clone());
            if f.role == FieldRole::Resource && f.std.is_none() {
                continue;
            }
            let v = match values.get(slot).cloned().unwrap_or(Value::Undefined) {
                Value::Date(d) if f.std == Some(StdAttr::Period) => {
                    let p = d.parts();
                    Value::Date(
                        BslDate::from_parts(p.year, p.month, p.day, 0, 0, 0)
                            .ok_or_else(|| internal("a day"))?,
                    )
                }
                v => v,
            };
            undefined |= matches!(v, Value::Undefined);
            pairs.push((i, v));
        }
        let row = if undefined {
            None
        } else {
            let mut st = Stmt::default();
            let cond = st.equalities(&reg, "Т.", &pairs)?;
            let text = format!(
                "ВЫБРАТЬ {} ИЗ {} КАК Т{}",
                select_list(&names),
                reg.table(),
                where_of(&cond)
            );
            let result = st.run(ex, &text)?;
            let table = unload(ex, &result)?;
            match count(ex, &table)? {
                0 => None,
                1 => Some(row_at(ex, &table, 0)?),
                _ => return Err(internal("a key gave several records").into()),
            }
        };
        let mut read = Vec::with_capacity(self.fields.len());
        for (slot, &i) in self.fields.iter().enumerate() {
            let f = reg.def.fields.get(i).ok_or_else(|| internal("a field"))?;
            let v = match &row {
                Some(row) => ex.get_member(row, &f.name)?,
                None => default_of(&app, f)?,
            };
            read.push((slot, v));
        }
        {
            let mut values = self.values.borrow_mut();
            for (slot, v) in read {
                if let Some(s) = values.get_mut(slot) {
                    *s = v;
                }
            }
        }
        self.selected.set(row.is_some());
        if row.is_some() {
            self.touched.borrow_mut().iter_mut().for_each(|t| *t = true);
        }
        *self.read_key.borrow_mut() = if row.is_some() {
            Some(self.key_values())
        } else {
            None
        };
        Ok(())
    }

    fn key_slots(&self) -> Vec<usize> {
        let def = self.def();
        self.fields
            .iter()
            .enumerate()
            .filter(|(_, i)| {
                def.fields.get(**i).is_some_and(|f| {
                    f.std == Some(StdAttr::Period)
                        || (f.std.is_none() && f.role == FieldRole::Dimension)
                })
            })
            .map(|(s, _)| s)
            .collect()
    }

    fn key_values(&self) -> Vec<Value> {
        let values = self.values.borrow();
        self.key_slots()
            .into_iter()
            .map(|s| values.get(s).cloned().unwrap_or(Value::Undefined))
            .collect()
    }

    fn store(&self, ex: &mut Execution, write: bool, args: &[Arg]) -> CallResult<()> {
        let member = if write {
            "Записать"
        } else {
            "Удалить"
        };
        let refuse =
            |what: &str| -> bsl_vm::CallError { self.refuse(&format!("{member}: {what}")).into() };
        let app = self.app.clone();
        let def = self.def();
        if write {
            match arg_value(ex, args, 0)? {
                Value::Undefined | Value::Bool(true) => {}
                _ => return Err(refuse("замещение не Истина")),
            }
        }
        if app
            .modules()
            .of_object(self.obj, md_model::ModuleKind::RecordSetModule)
            .is_some()
            || crate::events::has_write_subscriptions(&app, def, "RecordSet")
        {
            return Err(refuse("обработчики записи набора"));
        }
        let key = self.key_values();
        match &*self.read_key.borrow() {
            Some(read) => {
                let same = read.len() == key.len()
                    && read.iter().zip(&key).all(|(a, b)| bsl_vm::ops::equal(a, b));
                if !same {
                    return Err(refuse("ключ изменён после чтения"));
                }
            }
            None if !write => return Err(refuse("запись не прочитана")),
            None => {}
        }
        let set = crate::recordset::create_set(ex, &app, self.obj)?;
        let filter = ex.get_member(&set, "Отбор")?;
        let values = self.values.borrow().clone();
        for s in self.key_slots() {
            let i = *self.fields.get(s).ok_or_else(|| internal("a field"))?;
            let name = &def.fields.get(i).ok_or_else(|| internal("a field"))?.name;
            let item = ex.get_member(&filter, name)?;
            let mut a = [Arg::Val(values.get(s).cloned().unwrap_or(Value::Undefined))];
            ex.call_method(&item, "Установить", &mut a, false)?;
        }
        if write {
            let rec = ex.call_method(&set, "Добавить", &mut [], true)?;
            let touched = self.touched.borrow().clone();
            for (s, &i) in self.fields.iter().enumerate() {
                let f = def.fields.get(i).ok_or_else(|| internal("a field"))?;
                if f.types.atoms.iter().any(|a| {
                    matches!(
                        a,
                        crate::model::TypeAtom::ValueStorage | crate::model::TypeAtom::Binary
                    )
                }) {
                    if touched.get(s).copied().unwrap_or(false) {
                        return Err(refuse(&format!(
                            "{}: ХранилищеЗначения или ДвоичныеДанные",
                            f.name
                        )));
                    }
                    continue;
                }
                ex.set_member(
                    &rec,
                    &f.name,
                    values.get(s).cloned().unwrap_or(Value::Undefined),
                )?;
            }
        }
        let mut a = [Arg::Val(Value::Bool(true))];
        ex.call_method(&set, "Записать", &mut a, false)?;
        Ok(())
    }
}

impl HostObject for RecordManager {
    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match self.slot_of(ex, m) {
            Some(s) => self
                .values
                .borrow()
                .get(s)
                .cloned()
                .ok_or_else(|| internal("a field").into()),
            None => Err(self.refuse(&self.member_ru(ex, m)).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let Some(s) = self.slot_of(ex, m) else {
            return Err(self
                .refuse(&format!("{} (запись)", self.member_ru(ex, m)))
                .into());
        };
        let i = *self.fields.get(s).ok_or_else(|| internal("a field"))?;
        let f = self
            .def()
            .fields
            .get(i)
            .ok_or_else(|| internal("a field"))?;
        let cast = crate::values::typedesc::cast_to_field(&self.app, ex, f, v)?;
        crate::read_budget::reserve(ex, crate::read_budget::value_heap(&cast))?;
        if let Some(slot) = self.values.borrow_mut().get_mut(s) {
            *slot = cast;
        }
        if let Some(t) = self.touched.borrow_mut().get_mut(s) {
            *t = true;
        }
        Ok(())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match template_member(ex, self.ty, m) {
            Some((true, "Прочитать")) if args.is_empty() => {
                self.read(ex)?;
                Ok(Value::Undefined)
            }
            Some((true, "Выбран")) if args.is_empty() => Ok(Value::Bool(self.selected.get())),
            Some((true, "Записать")) => {
                self.store(ex, true, args)?;
                Ok(Value::Undefined)
            }
            Some((true, "Удалить")) => {
                self.store(ex, false, args)?;
                Ok(Value::Undefined)
            }
            Some((_, ru)) => Err(self.refuse(ru).into()),
            None => Err(self.refuse(&m.0.to_string()).into()),
        }
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let s = self.slot(name)?;
        FIELD_BASE.checked_add(u32::try_from(s).ok()?).map(MemberId)
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        let s = usize::try_from(m.0.checked_sub(FIELD_BASE)?).ok()?;
        (s < self.fields.len()).then_some(MemberKind::Property {
            readable: true,
            writable: true,
        })
    }
}

pub(crate) enum Recorder {
    Unset,
    Of(RefValue),
    Empty(ObjId),
}

pub(crate) struct AccumSet {
    app: Arc<Application>,
    obj: ObjId,
    ty: TypeId,
    filter: Rc<AccumFilter>,
    rows: RefCell<Option<Vec<Vec<Value>>>>,
}

impl AccumSet {
    pub(crate) fn create(
        ex: &mut Execution,
        app: &Arc<Application>,
        obj: ObjId,
        recorder: Recorder,
    ) -> CallResult<Value> {
        let reg = Reg::new(app, obj, "СоздатьНаборЗаписей");
        reg.check_shape()?;
        crate::read_budget::reserve(ex, 1024)?;
        let catalog = platform_catalog::catalog();
        let (Some(fty), Some(ity)) = (
            catalog.find_type("Отбор"),
            catalog.find_type("ЭлементОтбора"),
        ) else {
            return Err(internal("no type of the filter in the catalog").into());
        };
        let value = match recorder {
            Recorder::Unset => Value::Undefined,
            Recorder::Of(r) => crate::values::refs::ref_value(app, r)?,
            Recorder::Empty(doc) => crate::values::refs::empty_ref(app, doc)?,
        };
        let item = Rc::new(AccumFilterItem {
            app: app.clone(),
            obj,
            ty: ity,
            name: reg
                .std(StdAttr::Recorder)
                .map(|(_, f)| f.name.clone())
                .ok_or_else(|| internal("a register without a recorder"))?,
            used: Cell::new(true),
            value: RefCell::new(value),
        });
        Ok(Value::Object(Rc::new(AccumSet {
            app: app.clone(),
            obj,
            ty: type_of(app, obj, Category::RecordSet)?,
            filter: Rc::new(AccumFilter { ty: fty, item }),
            rows: RefCell::new(None),
        })))
    }

    fn refuse(&self, member: &str) -> HostError {
        HostError::not_implemented("РегистрНакопленияНаборЗаписей", member)
    }

    fn read(&self, ex: &mut Execution) -> CallResult<()> {
        let item = &self.filter.item;
        if !item.used.get() {
            return Err(self.refuse("Прочитать: отбор не используется").into());
        }
        let recorder = item.value.borrow().clone();
        if matches!(recorder, Value::Undefined) {
            *self.rows.borrow_mut() = Some(Vec::new());
            return Ok(());
        }
        if as_ref(&recorder).is_none_or(|r| r.is_empty()) {
            return Err(self.refuse("Прочитать: пустая ссылка в отборе").into());
        }
        let app = self.app.clone();
        let reg = Reg::new(&app, self.obj, "Прочитать");
        let cols = accum_columns(&reg);
        let names: Vec<String> = cols.iter().map(|(_, n)| n.clone()).collect();
        let (Some((rec, _)), Some((line, _))) =
            (reg.std(StdAttr::Recorder), reg.std(StdAttr::LineNumber))
        else {
            return Err(internal("a standard field of a register").into());
        };
        let mut st = Stmt::default();
        let p = st.param(recorder);
        let text = format!(
            "ВЫБРАТЬ {} ИЗ {} КАК Т ГДЕ Т.{} = {p} УПОРЯДОЧИТЬ ПО Т.{}",
            select_list(&names),
            reg.table(),
            reg.name(rec)?,
            reg.name(line)?
        );
        let result = st.run(ex, &text)?;
        let table = unload(ex, &result)?;
        let n = count(ex, &table)?;
        let mut rows = Vec::with_capacity(n);
        for i in 0..n {
            let row = row_at(ex, &table, i)?;
            let mut values = Vec::with_capacity(names.len());
            for name in &names {
                values.push(ex.get_member(&row, name)?);
            }
            rows.push(values);
        }
        *self.rows.borrow_mut() = Some(rows);
        Ok(())
    }

    fn count(&self) -> usize {
        self.rows.borrow().as_ref().map_or(0, Vec::len)
    }

    fn unload(&self, ex: &mut Execution) -> CallResult<Value> {
        let app = self.app.clone();
        let reg = Reg::new(&app, self.obj, "Выгрузить");
        let table = ex.construct_by_name("ТаблицаЗначений", &mut [])?;
        let list = ex.get_member(&table, "Колонки")?;
        let cols = accum_columns(&reg);
        for (i, name) in &cols {
            let ty = field_type(&app, ex, self.obj, *i)?;
            let mut a = [Arg::Val(Value::from(name.as_str())), Arg::Val(ty)];
            ex.call_method(&list, "Добавить", &mut a, true)?;
        }
        let mut a = [Arg::Val(Value::from("МоментВремени"))];
        let moment = ex.construct_by_name("ОписаниеТипов", &mut a)?;
        let mut a = [Arg::Val(Value::from("МоментВремени")), Arg::Val(moment)];
        ex.call_method(&list, "Добавить", &mut a, true)?;
        let rows = self.rows.borrow().clone().unwrap_or_default();
        for values in rows {
            let row = ex.call_method(&table, "Добавить", &mut [], true)?;
            for ((_, name), value) in cols.iter().zip(values) {
                ex.set_member(&row, name, value)?;
            }
            let empty = bsl_stdlib::api::new_point_in_time(ex, BslDate::EMPTY, &Value::Undefined)?;
            ex.set_member(&row, "МоментВремени", empty)?;
        }
        Ok(table)
    }
}

impl HostObject for AccumSet {
    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match template_member(ex, self.ty, m) {
            Some((false, "Отбор")) => {
                let f: Rc<dyn HostObject> = self.filter.clone();
                Ok(Value::Object(f))
            }
            Some((_, ru)) => Err(self.refuse(ru).into()),
            None => Err(self.refuse(&m.0.to_string()).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        let name =
            template_member(ex, self.ty, m).map_or_else(|| m.0.to_string(), |n| n.1.to_string());
        Err(self.refuse(&format!("{name} (запись)")).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match template_member(ex, self.ty, m) {
            Some((true, "Прочитать")) if args.is_empty() => {
                self.read(ex)?;
                Ok(Value::Undefined)
            }
            Some((true, "Выгрузить")) if args.is_empty() => self.unload(ex),
            Some((true, "Количество")) if args.is_empty() => {
                Ok(Value::from(i64::try_from(self.count()).unwrap_or(i64::MAX)))
            }
            Some((_, ru)) => Err(self.refuse(ru).into()),
            None => Err(self.refuse(&m.0.to_string()).into()),
        }
    }
}

struct AccumFilter {
    ty: platform_catalog::TypeIdx,
    item: Rc<AccumFilterItem>,
}

impl HostObject for AccumFilter {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        if m.0 == FIELD_BASE {
            let item: Rc<dyn HostObject> = self.item.clone();
            return Ok(Value::Object(item));
        }
        let name = member_name(platform_catalog::catalog(), self.ty, m)
            .map_or_else(|| m.0.to_string(), |n| n.1.to_string());
        Err(no_field(&name).into())
    }

    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::not_implemented("Отбор", &format!("{} (запись)", m.0)).into())
    }

    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        let name = member_name(platform_catalog::catalog(), self.ty, m)
            .map_or_else(|| m.0.to_string(), |n| n.1.to_string());
        Err(HostError::not_implemented("Отбор", &name).into())
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        (fold_name(name) == fold_name(&self.item.name)).then_some(MemberId(FIELD_BASE))
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        (m.0 == FIELD_BASE).then_some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }
}

struct AccumFilterItem {
    app: Arc<Application>,
    obj: ObjId,
    ty: platform_catalog::TypeIdx,
    name: String,
    used: Cell<bool>,
    value: RefCell<Value>,
}

impl AccumFilterItem {
    fn refuse(&self, member: &str) -> HostError {
        HostError::not_implemented("ЭлементОтбора", member)
    }

    fn set_value(&self, v: Value) -> CallResult<()> {
        if recorder_of(&self.app, self.obj, &v).is_none_or(|r| r.is_empty()) {
            return Err(self
                .refuse("Значение: не ссылка на документ-регистратор")
                .into());
        }
        *self.value.borrow_mut() = v;
        Ok(())
    }
}

impl HostObject for AccumFilterItem {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        match member_name(platform_catalog::catalog(), self.ty, m) {
            Some((false, "Имя", _)) => Ok(Value::from(self.name.as_str())),
            Some((false, "Использование", _)) => Ok(Value::Bool(self.used.get())),
            Some((false, "Значение", _)) => Ok(self.value.borrow().clone()),
            Some((_, ru, _)) => Err(self.refuse(ru).into()),
            None => Err(self.refuse(&m.0.to_string()).into()),
        }
    }

    fn set(&self, _: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        match member_name(platform_catalog::catalog(), self.ty, m) {
            Some((false, "Использование", _)) => match v {
                Value::Bool(b) => {
                    self.used.set(b);
                    Ok(())
                }
                _ => Err(self.refuse("Использование: значение не Булево").into()),
            },
            Some((false, "Значение", _)) => self.set_value(v),
            Some((_, ru, _)) => Err(self.refuse(ru).into()),
            None => Err(self.refuse(&m.0.to_string()).into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member_name(platform_catalog::catalog(), self.ty, m) {
            Some((true, "Установить", _)) => {
                let v = arg_value(ex, args, 0)?;
                let used = match arg_value(ex, args, 1)? {
                    Value::Undefined => true,
                    Value::Bool(b) => b,
                    _ => return Err(self.refuse("Установить: использование не Булево").into()),
                };
                self.set_value(v)?;
                self.used.set(used);
                Ok(Value::Undefined)
            }
            Some((_, ru, _)) => Err(self.refuse(ru).into()),
            None => Err(self.refuse(&m.0.to_string()).into()),
        }
    }
}

pub(crate) struct Movements {
    app: Arc<Application>,
    doc: ObjId,
    r: Option<RefValue>,
    ty: platform_catalog::TypeIdx,
    registers: Vec<ObjId>,
    sets: RefCell<Vec<Option<Value>>>,
}

impl Movements {
    pub(crate) fn create(
        ex: &mut Execution,
        app: &Arc<Application>,
        doc: ObjId,
        r: Option<RefValue>,
    ) -> CallResult<Value> {
        let ty = platform_catalog::catalog()
            .find_type("КоллекцияДвижений")
            .ok_or_else(|| internal("no type КоллекцияДвижений in the catalog"))?;
        let model = app.model();
        let registers: Vec<ObjId> = model
            .obj(doc)
            .behavior
            .register_records
            .iter()
            .filter_map(|md| model.objects.iter().find(|o| o.md == *md).map(|o| o.id))
            .collect();
        crate::read_budget::reserve(ex, 256u64.saturating_add(32 * registers.len() as u64))?;
        Ok(Value::Object(Rc::new(Movements {
            app: app.clone(),
            doc,
            r,
            ty,
            sets: RefCell::new(vec![None; registers.len()]),
            registers,
        })))
    }

    fn set_of(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        if let Some(Some(v)) = self.sets.borrow().get(i) {
            return Ok(v.clone());
        }
        let obj = *self
            .registers
            .get(i)
            .ok_or_else(|| internal("a register"))?;
        if self.app.model().obj(obj).kind != ObjKind::AccumulationRegister {
            let name = &self.app.model().obj(obj).name;
            return Err(HostError::not_implemented("КоллекцияДвижений", name).into());
        }
        let recorder = match self.r {
            Some(r) => Recorder::Of(r),
            None => Recorder::Empty(self.doc),
        };
        let set = AccumSet::create(ex, &self.app, obj, recorder)?;
        if let Some(slot) = self.sets.borrow_mut().get_mut(i) {
            *slot = Some(set.clone());
        }
        Ok(set)
    }
}

impl HostObject for Movements {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match m.0.checked_sub(FIELD_BASE) {
            Some(i) => self.set_of(ex, i as usize),
            None => {
                let name = member_name(platform_catalog::catalog(), self.ty, m)
                    .map_or_else(|| m.0.to_string(), |n| n.1.to_string());
                Err(HostError::not_implemented("КоллекцияДвижений", &name).into())
            }
        }
    }

    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::not_implemented("КоллекцияДвижений", &format!("{} (запись)", m.0)).into())
    }

    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        let name = member_name(platform_catalog::catalog(), self.ty, m)
            .map_or_else(|| m.0.to_string(), |n| n.1.to_string());
        Err(HostError::not_implemented("КоллекцияДвижений", &name).into())
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let wanted = fold_name(name);
        let i = self
            .registers
            .iter()
            .position(|&o| fold_name(&self.app.model().obj(o).name) == wanted)?;
        FIELD_BASE.checked_add(u32::try_from(i).ok()?).map(MemberId)
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        let i = usize::try_from(m.0.checked_sub(FIELD_BASE)?).ok()?;
        (i < self.registers.len()).then_some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }
}

pub(crate) fn criterion_manager(app: &Arc<Application>, name: &str) -> CallResult<Option<Value>> {
    let Some(ty) = app.types().and_then(|t| t.criterion_manager(name)) else {
        return Ok(None);
    };
    Ok(Some(Value::Object(Rc::new(CriterionManager {
        app: app.clone(),
        name: name.to_string(),
        ty,
    }))))
}

struct CriterionManager {
    app: Arc<Application>,
    name: String,
    ty: TypeId,
}

impl CriterionManager {
    fn refuse(&self, member: &str) -> HostError {
        HostError::not_implemented("КритерийОтбораМенеджер", member)
    }

    fn admits(&self, v: &Value) -> bool {
        let Some(r) = as_ref(v) else { return false };
        let model = self.app.model();
        let Some(md_model::Value::Type(td)) = model
            .config
            .objects
            .iter()
            .find(|o| o.kind == MdKind::FilterCriterion && o.name == self.name)
            .and_then(|o| o.prop("Type"))
        else {
            return false;
        };
        let by_type_id = |id| {
            model.objects.iter().find_map(|o| {
                o.generated
                    .iter()
                    .any(|g| g.category == "Ref" && g.type_id == id)
                    .then_some(o.id)
            })
        };
        let types = crate::model::TypeSet::of(&model.config, td, &by_type_id);
        types
            .atoms
            .iter()
            .any(|a| matches!(a, crate::model::TypeAtom::Ref(o) if *o == r.obj))
    }

    fn find(&self, ex: &mut Execution, v: Value) -> CallResult<Value> {
        if self.app.model().script != md_model::ScriptVariant::Russian {
            return Err(self
                .refuse("Найти: английский вариант встроенного языка")
                .into());
        }
        let out = ex.construct_by_name("Массив", &mut [])?;
        if matches!(v, Value::Number(_)) {
            return Ok(out);
        }
        if !self.admits(&v) {
            return Err(self
                .refuse("Найти: значение не ссылка типа критерия")
                .into());
        }
        let mut st = Stmt::default();
        let p = st.param(v);
        let text = format!(
            "ВЫБРАТЬ Т.Ссылка КАК Ссылка ИЗ КритерийОтбора.{}({p}) КАК Т",
            self.name
        );
        let result = st.run(ex, &text)?;
        let table = unload(ex, &result)?;
        let n = count(ex, &table)?;
        for i in 0..n {
            let row = row_at(ex, &table, i)?;
            let r = ex.get_member(&row, "Ссылка")?;
            let mut a = [Arg::Val(r)];
            ex.call_method(&out, "Добавить", &mut a, false)?;
        }
        Ok(out)
    }
}

impl HostObject for CriterionManager {
    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, _: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        Ok(Some(bsl_prim::Str::from(
            format!("КритерийОтбораМенеджер.{}", self.name).as_str(),
        )))
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let name =
            template_member(ex, self.ty, m).map_or_else(|| m.0.to_string(), |n| n.1.to_string());
        Err(self.refuse(&name).into())
    }

    fn set(&self, ex: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        let name =
            template_member(ex, self.ty, m).map_or_else(|| m.0.to_string(), |n| n.1.to_string());
        Err(self.refuse(&format!("{name} (запись)")).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match template_member(ex, self.ty, m) {
            Some((true, "Найти")) if args.len() == 1 => {
                let v = arg_value(ex, args, 0)?;
                self.find(ex, v)
            }
            Some((_, ru)) => Err(self.refuse(ru).into()),
            None => Err(self.refuse(&m.0.to_string()).into()),
        }
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<CriterionManager>()
                .is_some_and(|o| Arc::ptr_eq(&o.app, &self.app) && o.name == self.name),
        )
    }
}
