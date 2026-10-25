use crate::host::TestBase;
use brace::Guid;
use bsl_prim::{BslDate, Number};
use query_ir::value::{Boundary, Moment};
use query_ir::{
    DateQ, NumQ, Other, ParamConst, ParamShape, ParamTypes, ParamValue, ParamValues, QStr, QType,
    QValue, RefId, StrQ, TempColumn, TempSchema, ref_id,
};
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) enum ParamErr {
    Unsupported(&'static str),
    Later {
        what: &'static str,
        stage: &'static str,
    },
    UndefinedType(String),
    Bad(String),
}

struct Parsed {
    value: ParamValue,
    shape: ParamShape,
    constant: Option<ParamConst>,
}

#[derive(Default)]
pub struct CaseParams(HashMap<String, Parsed>);

impl ParamValues for CaseParams {
    fn get(&self, name: &str) -> Option<ParamValue> {
        self.0.get(&name.to_lowercase()).map(|p| p.value.clone())
    }
}

impl ParamTypes for CaseParams {
    fn shape(&self, name: &str) -> Option<ParamShape> {
        self.0.get(&name.to_lowercase()).map(|p| p.shape.clone())
    }

    fn constant(&self, name: &str) -> Option<ParamConst> {
        self.0
            .get(&name.to_lowercase())
            .and_then(|p| p.constant.clone())
    }
}

impl query_ir::ManagedParamTypes for CaseParams {
    fn shape_lowercase(&self, name: &str) -> Option<&ParamShape> {
        self.0.get(name).map(|param| &param.shape)
    }
    fn constant_lowercase(&self, name: &str) -> Option<&ParamConst> {
        self.0.get(name).and_then(|param| param.constant.as_ref())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamError {
    Unsupported(&'static str),
    Bad(String),
}

impl From<ParamErr> for ParamError {
    fn from(e: ParamErr) -> ParamError {
        match e {
            ParamErr::Unsupported(w) | ParamErr::Later { what: w, .. } => {
                ParamError::Unsupported(w)
            }
            ParamErr::Bad(m) => ParamError::Bad(m),
            ParamErr::UndefinedType(name) => ParamError::Bad(format!("undefined type: {name}")),
        }
    }
}

pub fn parse_params(base: &TestBase, pairs: &[(&str, &str)]) -> Result<CaseParams, ParamError> {
    let mut out = HashMap::new();
    for (name, repr) in pairs {
        out.insert(name.to_lowercase(), parse(base, repr)?);
    }
    Ok(CaseParams(out))
}

pub(crate) fn case_params(
    host: &crate::exec_host::TestHost<'_>,
    case: &serde_json::Value,
) -> Result<CaseParams, ParamErr> {
    let base = host.base;
    let mut out = HashMap::new();
    if let Some(reprs) = case["params_repr"].as_object() {
        for (name, repr) in reprs {
            let repr = repr.as_str().unwrap_or("");
            let p = if repr.trim_start().starts_with("ТаблицаЗначений<") {
                let src = case["params"]
                    .get(name.as_str())
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                value_table(base, Some(host), repr, src)?
            } else {
                parse(base, repr)?
            };
            out.insert(name.to_lowercase(), p);
        }
    }
    if case["params_repr"].is_null()
        && let Some(params) = case["params"].as_object()
    {
        for (name, source) in params {
            let source = source.as_str().unwrap_or("").trim();
            let inner = call(source, "Тип")
                .or_else(|| call(source, "Type"))
                .and_then(string_literal)
                .ok_or(ParamErr::Unsupported("parameter code without params_repr"))?;
            let repr = format!("Тип({inner})");
            out.insert(name.to_lowercase(), parse(base, &repr)?);
        }
    }
    Ok(CaseParams(out))
}

fn string_literal(text: &str) -> Option<String> {
    let inner = text.trim().strip_prefix('"')?.strip_suffix('"')?;
    let mut chars = inner.chars();
    let mut decoded = String::new();
    while let Some(c) = chars.next() {
        if c == '"' && chars.next() != Some('"') {
            return None;
        }
        if matches!(c, '\r' | '\n') {
            return None;
        }
        decoded.push(c);
    }
    Some(decoded)
}

fn number0() -> QType {
    QType::num(NumQ {
        digits: 0,
        scale: 0,
        nonneg: false,
    })
}

fn opaque(shape: ParamShape) -> Parsed {
    Parsed {
        value: ParamValue::Value(QValue::Null),
        shape,
        constant: None,
    }
}

fn parse(base: &TestBase, repr: &str) -> Result<Parsed, ParamErr> {
    let r = repr.trim();
    for prefix in ["Массив[", "ФиксированныйМассив[", "СписокЗначений["]
    {
        if let Some(body) = r.strip_prefix(prefix).and_then(|b| b.strip_suffix(']')) {
            let mut values = Vec::new();
            let mut ty = QType::undefined();
            for el in split_top(body) {
                let (v, t) = scalar(base, el)?;
                ty = ty.merge(&t);
                values.push(v);
            }
            let constant = values
                .iter()
                .filter(|v| matches!(v, QValue::Number(_) | QValue::String(_)))
                .cloned()
                .collect();
            return Ok(Parsed {
                value: ParamValue::List(values),
                shape: ParamShape::List(ty),
                constant: Some(ParamConst::List(constant)),
            });
        }
    }
    if r.starts_with("ДеревоЗначений<") {
        return Err(ParamErr::Later {
            what: "value table",
            stage: "Q3",
        });
    }
    if r.starts_with("Структура{") || r.starts_with("ФиксированнаяСтруктура{")
    {
        return Ok(opaque(ParamShape::Structure));
    }
    if r.starts_with("ОписаниеТипов(") {
        return Ok(opaque(ParamShape::TypeDescription));
    }
    let (v, t) = scalar(base, r)?;
    let constant =
        matches!(v, QValue::Number(_) | QValue::String(_)).then(|| ParamConst::Value(v.clone()));
    Ok(Parsed {
        value: ParamValue::Value(v),
        shape: ParamShape::Value(t),
        constant,
    })
}

fn value_table(
    base: &TestBase,
    host: Option<&crate::exec_host::TestHost<'_>>,
    repr: &str,
    src: &str,
) -> Result<Parsed, ParamErr> {
    let r = repr.trim();
    let rest = r
        .strip_prefix("ТаблицаЗначений<")
        .ok_or_else(|| bad("value table", r))?;
    let (names, rows) = rest.split_once('>').ok_or_else(|| bad("value table", r))?;
    let body = rows
        .trim()
        .strip_prefix('[')
        .and_then(|b| b.strip_suffix(']'))
        .ok_or_else(|| bad("value table rows", r))?;
    let mut columns = Vec::new();
    if let Some(text) = unload_text(src) {
        let host = host.ok_or_else(|| bad("a table unloaded from a query", src))?;
        columns = unloaded_columns(host, &text)?;
    } else {
        let spec = column_spec(src)
            .ok_or_else(|| bad("value table without a column list in params", src))?;
        for c in spec.split(';') {
            let (name, ty) = match c.split_once(':') {
                Some((n, t)) => (n, Some(column_type(base, t)?)),
                None => (c, None),
            };
            columns.push(TempColumn {
                name: name.trim().to_string(),
                ty,
                unknown: false,
                stored: None,
            });
        }
    }
    if split_top(names).len() != columns.len() {
        return Err(bad(
            "value table columns differ in params and params_repr",
            r,
        ));
    }
    let mut table = Vec::new();
    for row in split_top(body) {
        let inner = row
            .strip_prefix('(')
            .and_then(|x| x.strip_suffix(')'))
            .ok_or_else(|| bad("value table row", row))?;
        let mut values = Vec::with_capacity(columns.len());
        for el in split_top(inner) {
            values.push(scalar(base, el)?.0);
        }
        if values.len() != columns.len() {
            return Err(bad("value table row width", row));
        }
        for (v, c) in values.iter().zip(&columns) {
            if let Some(t) = &c.ty
                && !fits_column(base, v, t)
            {
                return Err(bad("value table cell does not fit its column type", row));
            }
        }
        table.push(values);
    }
    let schema = TempSchema {
        name: String::new(),
        columns: columns.clone(),
        open: false,
    };
    Ok(Parsed {
        value: ParamValue::Table {
            columns,
            rows: table,
        },
        shape: ParamShape::Table(Arc::new(schema)),
        constant: None,
    })
}

fn unload_text(src: &str) -> Option<String> {
    let body = src.trim().strip_prefix("Выгр(\"")?;
    let mut text = String::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            if chars.peek() == Some(&'"') {
                chars.next();
                text.push('"');
                continue;
            }
            break;
        }
        text.push(c);
    }
    Some(text)
}

fn unloaded_columns(
    host: &crate::exec_host::TestHost<'_>,
    text: &str,
) -> Result<Vec<TempColumn>, ParamErr> {
    let mut temp = query_exec::TempTables::new();
    let runs = query_exec::run_batch_detailed(
        host,
        text,
        &CaseParams::default(),
        &mut temp,
        query_exec::RunMode::Batch,
    )
    .map_err(|_| bad("the query of a table unloaded from it fails", text))?;
    let Some(query_exec::StmtOutcome::Result(res)) = runs.last().map(|r| &r.outcome) else {
        return Err(bad("the query of a table unloaded from it", text));
    };
    Ok(res
        .columns
        .iter()
        .map(|c| TempColumn {
            name: c.name.clone(),
            ty: Some(c.ty.clone().without_null()),
            unknown: false,
            stored: None,
        })
        .collect())
}

fn column_spec(src: &str) -> Option<&str> {
    let rest = src.trim_start().strip_prefix("ТЗ")?.trim_start();
    let rest = rest.strip_prefix('(')?.trim_start().strip_prefix('"')?;
    rest.split_once('"')
        .map(|(s, _)| s)
        .filter(|s| !s.is_empty())
}

fn fits_column(base: &TestBase, v: &QValue, t: &QType) -> bool {
    match v {
        QValue::Null | QValue::Undefined => true,
        QValue::Number(_) => t.number.is_some(),
        QValue::String(_) => t.string.is_some(),
        QValue::Date(_) => t.date.is_some(),
        QValue::Bool(_) => t.boolean,
        QValue::Ref { table, .. } => t
            .refs
            .iter()
            .any(|a| base.storage.ref_number(*a) == Some(*table)),
        _ => false,
    }
}

fn column_type(base: &TestBase, names: &str) -> Result<QType, ParamErr> {
    let mut t = QType::undefined();
    for n in names.split(',') {
        let n = n.trim();
        let one = match n {
            "Число" => number0(),
            "Строка" => QType::str_(StrQ {
                len: 0,
                fixed: false,
            }),
            "Дата" => QType::date(DateQ::DateTime),
            "Булево" => QType::boolean(),
            _ => {
                let (kind, obj) = n.split_once('.').ok_or_else(|| bad("column type", n))?;
                let kind = match kind {
                    "СправочникСсылка" => "Справочник",
                    "ДокументСсылка" => "Документ",
                    "ПеречислениеСсылка" => "Перечисление",
                    _ => return Err(bad("column type", n)),
                };
                base.schema
                    .type_by_name(&[kind, obj])
                    .ok_or_else(|| bad("column type", n))?
            }
        };
        t = t.merge(&one);
    }
    Ok(t)
}

fn split_top(s: &str) -> Vec<&str> {
    let (mut out, mut depth, mut quote, mut start) = (Vec::new(), 0i32, false, 0usize);
    for (i, c) in s.char_indices() {
        match c {
            '"' => quote = !quote,
            '(' | '[' | '{' | '<' if !quote => depth += 1,
            ')' | ']' | '}' | '>' if !quote => depth -= 1,
            ',' if !quote && depth == 0 => {
                out.push(s.get(start..i).unwrap_or("").trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = s.get(start..).unwrap_or("").trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn bad(what: &str, r: &str) -> ParamErr {
    ParamErr::Bad(format!("{what}: {r}"))
}

fn scalar(base: &TestBase, repr: &str) -> Result<(QValue, QType), ParamErr> {
    let r = repr.trim();
    if let Some(inner) = r.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        let text = inner.replace("\"\"", "\"");
        let len = u32::try_from(text.encode_utf16().count())
            .unwrap_or(u32::MAX)
            .max(1);
        return Ok((
            QValue::String(QStr::from_str(&text)),
            QType::str_(StrQ { len, fixed: false }),
        ));
    }
    match r {
        "Истина" => return Ok((QValue::Bool(true), QType::boolean())),
        "Ложь" => return Ok((QValue::Bool(false), QType::boolean())),
        "NULL" => return Ok((QValue::Null, QType::null_only())),
        "Неопределено" => return Ok((QValue::Undefined, QType::undefined())),
        _ => {}
    }
    if let Some(inner) = r.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
        return Ok((
            QValue::Date(date(inner).ok_or_else(|| bad("date", r))?),
            QType::date(DateQ::DateTime),
        ));
    }
    if r.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        let n = Number::parse(r).ok_or_else(|| bad("number", r))?;
        return Ok((QValue::Number(n), number0()));
    }
    if let Some(inner) = call(r, "Тип") {
        let t = match base.schema.ref_by_name(inner) {
            Some(a) => QType::reference(a),
            None => match inner {
                "Не определено" => QType::undefined(),
                "Null" => QType::null_only(),
                "Таблица значений" | "ТаблицаЗначений" | "ValueTable" => {
                    QType::other(Other::ValueTable)
                }
                _ => base
                    .schema
                    .type_by_name(&[inner])
                    .or_else(|| base.schema.type_by_name(&[&inner.replace(' ', "")]))
                    .ok_or_else(|| ParamErr::UndefinedType(inner.to_string()))?,
            },
        };
        return Ok((QValue::Type(t), QType::other(Other::Type)));
    }
    if let Some(inner) = call(r, "УникальныйИдентификатор") {
        let g = Guid::parse(inner).ok_or_else(|| bad("uuid", r))?;
        return Ok((QValue::Uuid(*g.as_bytes()), QType::other(Other::Uuid)));
    }
    if let Some(inner) = call(r, "ДвоичныеДанные") {
        let bytes = hex(inner).ok_or_else(|| bad("binary", r))?;
        return Ok((QValue::Binary(bytes.into()), QType::other(Other::Binary)));
    }
    if let Some(inner) = call(r, "МоментВремени") {
        let m = moment(base, inner).ok_or_else(|| bad("point in time", r))?;
        return Ok((
            QValue::PointInTime(Box::new(m)),
            QType::other(Other::PointInTime),
        ));
    }
    if let Some(inner) = call(r, "Граница") {
        let parts = split_top(inner);
        let [point, kind] = parts.as_slice() else {
            return Err(bad("boundary", r));
        };
        let include = match kind.trim() {
            "Включая" => true,
            "Исключая" => false,
            _ => return Err(bad("boundary kind", r)),
        };
        let (point, _) = scalar(base, point.trim())?;
        if !matches!(point, QValue::Date(_) | QValue::PointInTime(_)) {
            return Err(bad("boundary point", r));
        }
        return Ok((
            QValue::Boundary(Box::new(Boundary { point, include })),
            QType::other(Other::Boundary),
        ));
    }
    reference(base, r)
}

fn moment(base: &TestBase, inner: &str) -> Option<Moment> {
    let parts = split_top(inner);
    let [d, r] = parts.as_slice() else {
        return None;
    };
    let d = d.trim().strip_prefix('\'')?.strip_suffix('\'')?;
    let date = date(d)?;
    let reference = match r.trim() {
        "Неопределено" => None,
        r => match reference(base, r).ok()? {
            (QValue::Ref { table, id }, _) => Some((table, id)),
            _ => return None,
        },
    };
    Some(Moment { date, reference })
}

fn call<'a>(r: &'a str, name: &str) -> Option<&'a str> {
    r.strip_prefix(name)?.strip_prefix('(')?.strip_suffix(')')
}

impl TestBase {
    pub fn parse_reference(&self, text: &str, bare_table: u32) -> Option<(u32, RefId)> {
        if let Some(g) = Guid::parse(text) {
            return Some((bare_table, ref_id(g)));
        }
        let (head, rest) = text.split_once('(')?;
        let inner = rest.strip_suffix(')')?;
        if let Some(g) = Guid::parse(inner) {
            let atom = self.schema.ref_by_name(head)?;
            return Some((self.storage.ref_number(atom)?, ref_id(g)));
        }
        match reference(self, text) {
            Ok((QValue::Ref { table, id }, _)) => Some((table, id)),
            _ => None,
        }
    }
}

fn reference(base: &TestBase, r: &str) -> Result<(QValue, QType), ParamErr> {
    let (head, rest) = r.split_once('(').ok_or_else(|| bad("parameter", r))?;
    let inner = rest.strip_suffix(')').ok_or_else(|| bad("parameter", r))?;
    let atom = base
        .schema
        .ref_by_name(head)
        .ok_or_else(|| bad("reference type", r))?;
    let info = base.schema.ref_info(atom);
    let number = base
        .storage
        .ref_number(atom)
        .ok_or_else(|| bad("reference type without storage", head))?;
    let id: RefId = if let Some(name) = inner.strip_prefix('@') {
        match base.predefined_refs.get(&(info.table, name.to_string())) {
            Some(QValue::Ref { id, .. }) => *id,
            _ => return Err(bad("predefined item", r)),
        }
    } else if info.kind == md_model::MdKind::Enum {
        if inner.is_empty() {
            [0; 16]
        } else {
            *base
                .enum_ids
                .get(&(number, inner.to_lowercase()))
                .ok_or_else(|| bad("enumeration value", r))?
        }
    } else {
        ref_id(Guid::parse(inner).ok_or_else(|| bad("reference", r))?)
    };
    Ok((QValue::Ref { table: number, id }, QType::reference(atom)))
}

fn hex(s: &str) -> Option<Vec<u8>> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn date(s: &str) -> Option<BslDate> {
    let (d, t) = s.split_once('T')?;
    let mut dp = d.split('-');
    let mut tp = t.split(':');
    let next = |it: &mut std::str::Split<'_, char>| it.next()?.parse::<u32>().ok();
    let (y, mo, da) = (next(&mut dp)?, next(&mut dp)?, next(&mut dp)?);
    let (h, mi, se) = (next(&mut tp)?, next(&mut tp)?, next(&mut tp)?);
    BslDate::from_parts(y, mo, da, h, mi, se)
}
