use onec_client::{
    AllowedLength, AllowedSign, Column, DateFractions, DateQualifiers, EnumRef, ListItem,
    NumberQualifiers, Picture, Ref, StringQualifiers, TreeRow, TypeDescription, TypeName, Uuid,
    Value, ValueList, ValueTable, ValueTree,
};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use super::JsonValue;

fn type_name(text: &str) -> Result<TypeName, String> {
    TypeName::parse(text).ok_or_else(|| {
        format!("not a type name: {text:?} (use xs:, v8:, cfg: or {{namespace}}local)")
    })
}

pub(super) fn reference((ty, uuid): (String, String)) -> Result<Value, String> {
    let uuid = Uuid::parse(&uuid).map_err(|e| e.to_string())?;
    Ref::new(type_name(&ty)?, uuid)
        .map(Value::Ref)
        .map_err(|e| e.to_string())
}

pub(super) fn enum_value((ty, name): (String, String)) -> Result<Value, String> {
    EnumRef::new(type_name(&ty)?, &name)
        .map(Value::EnumRef)
        .map_err(|e| e.to_string())
}

#[derive(Deserialize, Clone, Copy)]
enum JsonSign {
    Any,
    Nonnegative,
}

#[derive(Deserialize, Clone, Copy)]
enum JsonLength {
    Variable,
    Fixed,
}

#[derive(Deserialize, Clone, Copy)]
enum JsonFractions {
    Date,
    Time,
    DateTime,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonNumber {
    digits: u32,
    fraction: u32,
    sign: JsonSign,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonString {
    length: u32,
    allowed: JsonLength,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct JsonType {
    #[serde(default)]
    types: Vec<String>,
    number: Option<JsonNumber>,
    string: Option<JsonString>,
    date: Option<JsonFractions>,
}

impl JsonType {
    fn into_model(self) -> Result<TypeDescription, String> {
        let mut types: Vec<TypeName> = Vec::with_capacity(self.types.len());
        for text in &self.types {
            let t = type_name(text)?;
            if types.contains(&t) {
                return Err(format!("repeated type {t}"));
            }
            types.push(t);
        }
        Ok(TypeDescription {
            types,
            number: self.number.map(|q| NumberQualifiers {
                digits: q.digits,
                fraction_digits: q.fraction,
                sign: match q.sign {
                    JsonSign::Any => AllowedSign::Any,
                    JsonSign::Nonnegative => AllowedSign::Nonnegative,
                },
            }),
            string: self.string.map(|q| StringQualifiers {
                length: q.length,
                allowed: match q.allowed {
                    JsonLength::Variable => AllowedLength::Variable,
                    JsonLength::Fixed => AllowedLength::Fixed,
                },
            }),
            date: self.date.map(|f| DateQualifiers {
                fractions: match f {
                    JsonFractions::Date => DateFractions::Date,
                    JsonFractions::Time => DateFractions::Time,
                    JsonFractions::DateTime => DateFractions::DateTime,
                },
            }),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonColumnFull {
    name: String,
    #[serde(rename = "type", default)]
    ty: JsonType,
    #[serde(default)]
    title: String,
    #[serde(default)]
    width: u64,
}

struct JsonColumn(JsonColumnFull);

impl<'de> Deserialize<'de> for JsonColumn {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = JsonColumn;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a column name or a column object")
            }
            fn visit_str<E>(self, v: &str) -> Result<JsonColumn, E> {
                Ok(JsonColumn(JsonColumnFull {
                    name: v.to_string(),
                    ty: JsonType::default(),
                    title: String::new(),
                    width: 0,
                }))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<JsonColumn, A::Error> {
                JsonColumnFull::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(JsonColumn)
            }
        }
        d.deserialize_any(V)
    }
}

fn columns(cols: Vec<JsonColumn>) -> Result<Vec<Column>, String> {
    cols.into_iter()
        .map(|JsonColumn(c)| {
            Ok(Column {
                name: c.name,
                value_type: c.ty.into_model()?,
                title: c.title,
                width: c.width,
            })
        })
        .collect()
}

fn cells(values: Vec<JsonValue>) -> Vec<Value> {
    values.into_iter().map(|JsonValue(v)| v).collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JsonTable {
    #[serde(default)]
    columns: Vec<JsonColumn>,
    #[serde(default)]
    indexes: Vec<Vec<String>>,
    #[serde(default)]
    rows: Vec<Vec<JsonValue>>,
}

pub(super) fn table(t: JsonTable) -> Result<Value, String> {
    let mut table = ValueTable::new(columns(t.columns)?).map_err(|e| e.to_string())?;
    for index in t.indexes {
        table.add_index(index).map_err(|e| e.to_string())?;
    }
    for (i, row) in t.rows.into_iter().enumerate() {
        table
            .push_row(cells(row))
            .map_err(|e| format!("row {}: {e}", i + 1))?;
    }
    Ok(Value::ValueTable(table))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonTreeRow {
    #[serde(default)]
    values: Vec<JsonValue>,
    #[serde(default)]
    rows: Vec<JsonTreeRow>,
}

fn tree_row(r: JsonTreeRow) -> TreeRow {
    let mut row = TreeRow::new(cells(r.values));
    for child in r.rows {
        row.push_child(tree_row(child));
    }
    row
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JsonTree {
    #[serde(default)]
    columns: Vec<JsonColumn>,
    #[serde(default)]
    rows: Vec<JsonTreeRow>,
}

pub(super) fn tree(t: JsonTree) -> Result<Value, String> {
    let mut tree = ValueTree::new(columns(t.columns)?).map_err(|e| e.to_string())?;
    for row in t.rows {
        tree.push_row(tree_row(row)).map_err(|e| e.to_string())?;
    }
    Ok(Value::ValueTree(tree))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonPicture {
    t: Option<String>,
    #[serde(rename = "ref")]
    reference: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonItem {
    value: JsonValue,
    #[serde(default)]
    presentation: String,
    #[serde(default)]
    check: bool,
    picture: Option<JsonPicture>,
    id: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JsonList {
    #[serde(rename = "type", default)]
    ty: JsonType,
    #[serde(default)]
    items: Vec<JsonItem>,
    last_id: Option<i64>,
}

pub(super) fn list(l: JsonList) -> Result<Value, String> {
    let mut list = ValueList::new(l.ty.into_model()?);
    for item in l.items {
        let JsonValue(value) = item.value;
        let mut it = ListItem::new(value);
        it.presentation = item.presentation;
        it.check = item.check;
        it.picture = match item.picture {
            Some(p) => Some(Picture {
                t: p.t,
                reference: p.reference.as_deref().map(type_name).transpose()?,
            }),
            None => None,
        };
        match item.id {
            Some(id) => list.push_with_id(it, id),
            None => list.push(it),
        }
        .map_err(|e| e.to_string())?;
    }
    if let Some(last) = l.last_id {
        list.set_last_id(last).map_err(|e| e.to_string())?;
    }
    Ok(Value::ValueList(list))
}

pub(super) fn json_nesting(j: &Json) -> usize {
    let mut deepest = 0;
    let mut stack = vec![(j, 0usize)];
    while let Some((node, level)) = stack.pop() {
        let children: Vec<&Json> = match node {
            Json::Array(items) => items.iter().collect(),
            Json::Object(map) => map.values().collect(),
            _ => continue,
        };
        deepest = deepest.max(level + 1);
        stack.extend(children.into_iter().map(|c| (c, level + 1)));
    }
    deepest
}

pub(super) fn to_json(v: &Value) -> Json {
    match v {
        Value::Undefined => Json::Null,
        Value::Null => json!({"null": null}),
        Value::Bool(b) => json!(b),
        Value::String(s) => json!(s),
        Value::Number(n) => json!({"n": n.as_str()}),
        Value::Date(d) => json!({"d": d.to_string()}),
        Value::Uuid(u) => json!({"u": u.to_string()}),
        Value::Ref(r) => json!({"ref": [r.type_name().to_string(), r.uuid().to_string()]}),
        Value::EnumRef(e) => json!({"enum": [e.type_name().to_string(), e.name()]}),
        Value::Array(a) => {
            let items: Vec<Json> = a.items.iter().map(to_json).collect();
            if a.fixed {
                json!({ "fix": items })
            } else {
                Json::Array(items)
            }
        }
        Value::Structure(st) => {
            let props: serde_json::Map<String, Json> = st
                .props()
                .iter()
                .map(|(k, x)| (k.clone(), to_json(x)))
                .collect();
            let tag = if st.is_fixed() { "fixst" } else { "st" };
            json!({ tag: props })
        }
        Value::Map(m) => {
            let pairs: Vec<Json> = m
                .pairs()
                .iter()
                .map(|(k, x)| json!([to_json(k), to_json(x)]))
                .collect();
            let tag = if m.is_fixed() { "fixmap" } else { "map" };
            json!({ tag: pairs })
        }
        Value::ValueTable(t) => {
            let mut body = serde_json::Map::new();
            body.insert("columns".into(), columns_json(t.columns()));
            if !t.indexes().is_empty() {
                body.insert("indexes".into(), json!(t.indexes()));
            }
            let rows: Vec<Json> = t
                .rows()
                .iter()
                .map(|r| Json::Array(r.iter().map(to_json).collect()))
                .collect();
            body.insert("rows".into(), Json::Array(rows));
            json!({ "vt": body })
        }
        Value::ValueTree(t) => json!({"tree": {
            "columns": columns_json(t.columns()),
            "rows": t.rows().iter().map(tree_row_json).collect::<Vec<_>>(),
        }}),
        Value::ValueList(l) => {
            let mut body = serde_json::Map::new();
            if !l.value_type.is_empty() {
                body.insert("type".into(), type_json(&l.value_type));
            }
            let items: Vec<Json> = l.items().iter().map(item_json).collect();
            body.insert("items".into(), Json::Array(items));
            body.insert("last_id".into(), json!(l.last_id()));
            json!({ "vl": body })
        }
    }
}

fn type_json(td: &TypeDescription) -> Json {
    let mut o = serde_json::Map::new();
    if !td.types.is_empty() {
        let types: Vec<String> = td.types.iter().map(ToString::to_string).collect();
        o.insert("types".into(), json!(types));
    }
    if let Some(q) = td.number {
        o.insert(
            "number".into(),
            json!({"digits": q.digits, "fraction": q.fraction_digits, "sign": sign_name(q.sign)}),
        );
    }
    if let Some(q) = td.string {
        o.insert(
            "string".into(),
            json!({"length": q.length, "allowed": length_name(q.allowed)}),
        );
    }
    if let Some(q) = td.date {
        o.insert("date".into(), json!(fractions_name(q.fractions)));
    }
    Json::Object(o)
}

fn sign_name(s: AllowedSign) -> &'static str {
    match s {
        AllowedSign::Any => "Any",
        AllowedSign::Nonnegative => "Nonnegative",
    }
}

fn length_name(l: AllowedLength) -> &'static str {
    match l {
        AllowedLength::Variable => "Variable",
        AllowedLength::Fixed => "Fixed",
    }
}

fn fractions_name(f: DateFractions) -> &'static str {
    match f {
        DateFractions::Date => "Date",
        DateFractions::Time => "Time",
        DateFractions::DateTime => "DateTime",
    }
}

fn columns_json(cols: &[Column]) -> Json {
    Json::Array(
        cols.iter()
            .map(|c| {
                if c.value_type.is_empty() && c.title.is_empty() && c.width == 0 {
                    return json!(c.name);
                }
                let mut o = serde_json::Map::new();
                o.insert("name".into(), json!(c.name));
                if !c.value_type.is_empty() {
                    o.insert("type".into(), type_json(&c.value_type));
                }
                if !c.title.is_empty() {
                    o.insert("title".into(), json!(c.title));
                }
                if c.width != 0 {
                    o.insert("width".into(), json!(c.width));
                }
                Json::Object(o)
            })
            .collect(),
    )
}

fn tree_row_json(r: &TreeRow) -> Json {
    let mut o = serde_json::Map::new();
    o.insert(
        "values".into(),
        Json::Array(r.values().iter().map(to_json).collect()),
    );
    if !r.rows().is_empty() {
        o.insert(
            "rows".into(),
            Json::Array(r.rows().iter().map(tree_row_json).collect()),
        );
    }
    Json::Object(o)
}

fn item_json(it: &ListItem) -> Json {
    let mut o = serde_json::Map::new();
    o.insert("value".into(), to_json(&it.value));
    if !it.presentation.is_empty() {
        o.insert("presentation".into(), json!(it.presentation));
    }
    if it.check {
        o.insert("check".into(), json!(true));
    }
    if let Some(p) = &it.picture {
        let mut po = serde_json::Map::new();
        if let Some(t) = &p.t {
            po.insert("t".into(), json!(t));
        }
        if let Some(r) = &p.reference {
            po.insert("ref".into(), json!(r.to_string()));
        }
        o.insert("picture".into(), Json::Object(po));
    }
    o.insert("id".into(), json!(it.id()));
    Json::Object(o)
}

pub(super) fn show_type(td: &TypeDescription) -> String {
    let number = td
        .number
        .map(|q| format!("({},{},{})", q.digits, q.fraction_digits, sign_name(q.sign)));
    let string = td
        .string
        .map(|q| format!("({},{})", q.length, length_name(q.allowed)));
    let date = td
        .date
        .map(|q| format!("({})", fractions_name(q.fractions)));
    const XS: &str = "http://www.w3.org/2001/XMLSchema";
    let has = |local: &str| td.types.iter().any(|t| t.ns() == XS && t.local() == local);
    let mut parts: Vec<String> = td
        .types
        .iter()
        .map(|t| {
            let q = match (t.ns(), t.local()) {
                (XS, "decimal") => &number,
                (XS, "string") => &string,
                (XS, "dateTime") => &date,
                _ => return t.to_string(),
            };
            format!("{t}{}", q.as_deref().unwrap_or(""))
        })
        .collect();
    for (name, local, q) in [
        ("number", "decimal", &number),
        ("string", "string", &string),
        ("date", "dateTime", &date),
    ] {
        if let (false, Some(q)) = (has(local), q) {
            parts.push(format!("{name}{q}"));
        }
    }
    if parts.is_empty() {
        "any".to_string()
    } else {
        parts.join(", ")
    }
}

fn indent(s: &str, by: usize) -> String {
    s.replace('\n', &format!("\n{}", " ".repeat(by)))
}

fn column_line(c: &Column) -> String {
    let mut line = format!("  column {}: {}", c.name, show_type(&c.value_type));
    if !c.title.is_empty() {
        line.push_str(&format!(" title={:?}", c.title));
    }
    if c.width != 0 {
        line.push_str(&format!(" width={}", c.width));
    }
    line
}

fn row_text(values: &[Value], cols: &[Column], show: &dyn Fn(&Value) -> String) -> String {
    let cells: Vec<String> = values
        .iter()
        .zip(cols)
        .map(|(v, c)| format!("{}={}", c.name, show(v)))
        .collect();
    if cells.iter().any(|c| c.contains('\n')) {
        cells
            .iter()
            .map(|c| format!("\n  {}", indent(c, 2)))
            .collect()
    } else {
        format!(" {}", cells.join(" | "))
    }
}

pub(super) fn show_table(t: &ValueTable, show: &dyn Fn(&Value) -> String) -> String {
    let mut lines = vec![format!(
        "ValueTable: {} columns, {} rows",
        t.columns().len(),
        t.rows().len()
    )];
    lines.extend(t.columns().iter().map(column_line));
    lines.extend(
        t.indexes()
            .iter()
            .map(|i| format!("  index {}", i.join(", "))),
    );
    for (i, row) in t.rows().iter().enumerate() {
        lines.push(format!(
            "  row {}:{}",
            i + 1,
            indent(&row_text(row, t.columns(), show), 2)
        ));
    }
    lines.join("\n")
}

fn tree_lines(
    row: &TreeRow,
    cols: &[Column],
    level: usize,
    show: &dyn Fn(&Value) -> String,
    out: &mut Vec<String>,
) {
    let pad = "  ".repeat(level);
    out.push(format!(
        "{pad}row:{}",
        indent(&row_text(row.values(), cols, show), pad.len())
    ));
    for child in row.rows() {
        tree_lines(child, cols, level + 1, show, out);
    }
}

pub(super) fn show_tree(t: &ValueTree, show: &dyn Fn(&Value) -> String) -> String {
    let mut lines = vec![format!(
        "ValueTree: {} columns, {} top rows",
        t.columns().len(),
        t.rows().len()
    )];
    lines.extend(t.columns().iter().map(column_line));
    for row in t.rows() {
        tree_lines(row, t.columns(), 1, show, &mut lines);
    }
    lines.join("\n")
}

pub(super) fn show_list(l: &ValueList, show: &dyn Fn(&Value) -> String) -> String {
    let mut lines = vec![format!(
        "ValueList: {} items, type {}, lastId {}",
        l.items().len(),
        show_type(&l.value_type),
        l.last_id()
    )];
    for it in l.items() {
        let mut line = format!(
            "  [{}] {}",
            if it.check { "x" } else { " " },
            indent(&show(&it.value), 6)
        );
        if !it.presentation.is_empty() {
            line.push_str(&format!(" presentation={:?}", it.presentation));
        }
        if let Some(p) = &it.picture {
            line.push_str(" picture=");
            line.push_str(
                &p.reference
                    .as_ref()
                    .map_or("?".to_string(), ToString::to_string),
            );
            if let Some(t) = &p.t {
                line.push_str(&format!(",t={t:?}"));
            }
        }
        line.push_str(&format!(" id={}", it.id()));
        lines.push(line);
    }
    lines.join("\n")
}
