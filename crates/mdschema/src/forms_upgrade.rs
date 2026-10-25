use crate::SchemaError;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Domain {
    Int,
    Atom,
    List,
    Set(Vec<i64>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub new: usize,
    pub op: String,
    pub old: Option<usize>,
    pub ty: Option<String>,
    pub name: Option<String>,
    pub value: Option<String>,
    pub domain: Option<Domain>,
    pub flag: Option<usize>,
    pub inputs: Vec<usize>,
    pub domains: Vec<Domain>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub reader: String,
    pub writer: String,
    pub new_version: String,
    pub old_len: usize,
    pub new_len: usize,
    pub ops: Vec<Op>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdditionSub {
    pub len: usize,
    pub types: BTreeMap<usize, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WriterSet {
    pub dcs_names: BTreeMap<String, String>,
    pub pal_namespace: String,
    pub form_bag_with_key7: BTreeMap<String, String>,
    pub table_bag_20_value: String,
    pub table_bag_21_types: Vec<String>,
    pub table_bag_21_value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeTables {
    pub items: BTreeMap<String, Context>,
    pub sublists: BTreeMap<String, Context>,
    pub addition_sublists: BTreeMap<String, AdditionSub>,
    pub created: BTreeMap<String, String>,
    pub terms: BTreeMap<String, String>,
    pub guids: BTreeMap<String, String>,
    pub defaults: BTreeMap<String, String>,
    pub writer_set: WriterSet,
}

fn err(msg: impl std::fmt::Display) -> SchemaError {
    SchemaError(format!("forms/upgrade.json: {msg}"))
}

fn usize_of(v: &Value, what: &str) -> Result<usize, SchemaError> {
    v.as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| err(format!("{what}: integer expected")))
}

fn str_of<'a>(v: &'a Value, what: &str) -> Result<&'a str, SchemaError> {
    v.as_str()
        .ok_or_else(|| err(format!("{what}: string expected")))
}

fn str_map(v: Option<&Value>, what: &str) -> Result<BTreeMap<String, String>, SchemaError> {
    let obj = v
        .and_then(Value::as_object)
        .ok_or_else(|| err(format!("{what}: object expected")))?;
    obj.iter()
        .map(|(k, v)| Ok((k.clone(), str_of(v, what)?.to_string())))
        .collect()
}

fn domain(v: &Value, what: &str) -> Result<Domain, SchemaError> {
    match v {
        Value::String(s) => match s.as_str() {
            "int" => Ok(Domain::Int),
            "atom" => Ok(Domain::Atom),
            "list" => Ok(Domain::List),
            other => Err(err(format!("{what}: domain {other}"))),
        },
        Value::Array(a) => a
            .iter()
            .map(|x| {
                x.as_i64()
                    .ok_or_else(|| err(format!("{what}: domain value")))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Domain::Set),
        _ => Err(err(format!("{what}: domain"))),
    }
}

fn op(v: &Value, what: &str) -> Result<Op, SchemaError> {
    let o = v
        .as_object()
        .ok_or_else(|| err(format!("{what}: op object expected")))?;
    let field = |k: &str| o.get(k);
    let new = usize_of(field("new").unwrap_or(&Value::Null), what)?;
    let what = format!("{what}[{new}]");
    let opt_usize = |k: &str| -> Result<Option<usize>, SchemaError> {
        field(k).map(|v| usize_of(v, &what)).transpose()
    };
    let opt_str = |k: &str| -> Result<Option<String>, SchemaError> {
        field(k)
            .map(|v| str_of(v, &what).map(String::from))
            .transpose()
    };
    Ok(Op {
        new,
        op: str_of(field("op").unwrap_or(&Value::Null), &what)?.to_string(),
        old: opt_usize("old")?,
        ty: opt_str("type")?,
        name: opt_str("name")?,
        value: opt_str("value")?,
        domain: field("domain").map(|d| domain(d, &what)).transpose()?,
        flag: opt_usize("flag")?,
        inputs: field("inputs")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|x| usize_of(x, &what)).collect())
            .transpose()?
            .unwrap_or_default(),
        domains: field("domains")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|d| domain(d, &what)).collect())
            .transpose()?
            .unwrap_or_default(),
    })
}

fn context(name: &str, v: &Value) -> Result<Context, SchemaError> {
    let o = v
        .as_object()
        .ok_or_else(|| err(format!("{name}: object expected")))?;
    let get = |k: &str| o.get(k).unwrap_or(&Value::Null);
    let ops = get("ops")
        .as_array()
        .ok_or_else(|| err(format!("{name}: ops expected")))?
        .iter()
        .map(|x| op(x, name))
        .collect::<Result<Vec<_>, _>>()?;
    let new_len = usize_of(get("new_len"), name)?;
    if ops.len() != new_len || ops.iter().enumerate().any(|(i, o)| o.new != i) {
        return Err(err(format!("{name}: ops must cover 0..{new_len} in order")));
    }
    Ok(Context {
        reader: str_of(get("reader"), name)?.to_string(),
        writer: str_of(get("writer"), name)?.to_string(),
        new_version: str_of(get("new_version"), name)?.to_string(),
        old_len: usize_of(get("old_len"), name)?,
        new_len,
        ops,
    })
}

fn contexts(v: Option<&Value>, what: &str) -> Result<BTreeMap<String, Context>, SchemaError> {
    v.and_then(Value::as_object)
        .ok_or_else(|| err(format!("{what}: object expected")))?
        .iter()
        .map(|(k, v)| Ok((k.clone(), context(k, v)?)))
        .collect()
}

fn build(resource: &crate::SchemaResource) -> Result<UpgradeTables, SchemaError> {
    let root: Value = serde_json::from_str(resource.json("forms/upgrade.json")?).map_err(err)?;
    let get = |k: &str| root.get(k);
    let addition_sublists = get("addition_sublists")
        .and_then(Value::as_object)
        .ok_or_else(|| err("addition_sublists: object expected"))?
        .iter()
        .map(|(k, v)| {
            let what = format!("addition_sublists[{k}]");
            let types = v
                .get("types")
                .and_then(Value::as_object)
                .ok_or_else(|| err(format!("{what}: types")))?
                .iter()
                .map(|(p, t)| {
                    Ok((
                        p.parse::<usize>()
                            .map_err(|_| err(format!("{what}: position {p}")))?,
                        str_of(t, &what)?.to_string(),
                    ))
                })
                .collect::<Result<BTreeMap<_, _>, SchemaError>>()?;
            Ok((
                k.clone(),
                AdditionSub {
                    len: usize_of(v.get("len").unwrap_or(&Value::Null), &what)?,
                    types,
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>, SchemaError>>()?;
    let ws = get("writer_set")
        .and_then(Value::as_object)
        .ok_or_else(|| err("writer_set: object expected"))?;
    let ws_get = |k: &str| ws.get(k).unwrap_or(&Value::Null);
    let writer_set = WriterSet {
        dcs_names: str_map(ws.get("dcs_names"), "writer_set.dcs_names")?,
        pal_namespace: str_of(ws_get("pal_namespace"), "writer_set")?.to_string(),
        form_bag_with_key7: str_map(
            ws.get("form_bag_with_key7"),
            "writer_set.form_bag_with_key7",
        )?,
        table_bag_20_value: str_of(ws_get("table_bag_20_value"), "writer_set")?.to_string(),
        table_bag_21_types: ws_get("table_bag_21_types")
            .as_array()
            .ok_or_else(|| err("writer_set.table_bag_21_types"))?
            .iter()
            .map(|g| str_of(g, "writer_set.table_bag_21_types").map(String::from))
            .collect::<Result<Vec<_>, _>>()?,
        table_bag_21_value: str_of(ws_get("table_bag_21_value"), "writer_set")?.to_string(),
    };
    Ok(UpgradeTables {
        writer_set,
        items: contexts(get("items"), "items")?,
        sublists: contexts(get("sublists"), "sublists")?,
        addition_sublists,
        created: str_map(get("created"), "created")?,
        terms: str_map(get("terms"), "terms")?,
        guids: str_map(get("guids"), "guids")?,
        defaults: str_map(get("defaults"), "defaults")?,
    })
}

pub fn upgrade_tables() -> Result<&'static UpgradeTables, SchemaError> {
    crate::embedded_resource()?.upgrade_tables()
}
impl crate::SchemaResource {
    pub fn upgrade_tables(&'static self) -> Result<&'static UpgradeTables, SchemaError> {
        self.upgrade
            .get_or_init(|| build(self))
            .as_ref()
            .map_err(Clone::clone)
    }
}
