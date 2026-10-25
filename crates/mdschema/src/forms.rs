use crate::SchemaError;
use serde_json::Map;
pub use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FormCompat {
    #[default]
    Base,
    V8_3_12,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PropEntry {
    pub path: Option<Vec<Value>>,
    pub codec: String,
    pub table: BTreeMap<String, TableValue>,
    pub default: Vec<Value>,
    pub moved: bool,
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableValue {
    pub raw: Value,
    pub parsed: Option<Value>,
}

impl TableValue {
    pub fn new(raw: Value) -> TableValue {
        let parsed = raw.as_str().and_then(|s| serde_json::from_str(s).ok());
        TableValue { raw, parsed }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub order: Vec<String>,
    pub props: Vec<(String, PropEntry)>,
}

#[derive(Debug)]
pub struct FormTables {
    pub learned: BTreeMap<String, Context>,
    pub paths: Value,
    pub events: Value,
    pub misc: Value,
    pub types: Value,
}

fn err(file: &str, msg: impl std::fmt::Display) -> SchemaError {
    SchemaError(format!("forms/{file}: {msg}"))
}

fn parse(file: &str, text: &str) -> Result<Value, SchemaError> {
    serde_json::from_str(text).map_err(|e| err(file, e))
}

fn read_private(dir: &Path, name: &str) -> Result<Option<Value>, SchemaError> {
    let p = dir.join(format!("{name}.json"));
    if !p.is_file() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&p).map_err(|e| err(name, format!("{}: {e}", p.display())))?;
    parse(name, &text).map(Some)
}

fn obj_mut<'a>(v: &'a mut Value, file: &str) -> Result<&'a mut Map<String, Value>, SchemaError> {
    v.as_object_mut()
        .ok_or_else(|| err(file, "object expected"))
}

fn merge_learned(pubv: &mut Value, privv: &Value, file: &str) -> Result<(), SchemaError> {
    let pubm = obj_mut(pubv, file)?;
    let Some(privm) = privv.as_object() else {
        return Err(err(file, "private: object expected"));
    };
    for (ctx, d) in privm {
        let Some(props) = d.get("props").and_then(Value::as_object) else {
            continue;
        };
        let c = pubm
            .entry(ctx.clone())
            .or_insert_with(|| serde_json::json!({"order": [], "props": {}}));
        let cp = obj_mut(c, file)?
            .entry("props")
            .or_insert_with(|| Value::Object(Map::new()));
        for (prop, xe) in props {
            let pe = obj_mut(cp, file)?
                .entry(prop.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            let pe = obj_mut(pe, file)?;
            if let Some(xt) = xe.get("table").and_then(Value::as_object) {
                let t = pe
                    .entry("table")
                    .or_insert_with(|| Value::Object(Map::new()));
                let t = obj_mut(t, file)?;
                for (k, v) in xt {
                    t.insert(k.clone(), v.clone());
                }
            }
            if let Some(xd) = xe.get("default").and_then(Value::as_array) {
                let old = pe
                    .get("default")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut new = old.clone();
                new.extend(xd.iter().filter(|b| !old.contains(b)).cloned());
                pe.insert("default".into(), Value::Array(new));
            }
            if let Some(ex) = xe.get("examples") {
                pe.insert("examples".into(), ex.clone());
            }
        }
    }
    Ok(())
}

fn merge_paths(pubv: &mut Value, privv: &Value, file: &str) -> Result<(), SchemaError> {
    let pubm = obj_mut(pubv, file)?;
    for sec in ["std", "sys"] {
        let Some(items) = privv.get(sec).and_then(Value::as_object) else {
            continue;
        };
        let s = pubm.entry(sec).or_insert_with(|| Value::Object(Map::new()));
        let s = obj_mut(s, file)?;
        for (k, v) in items {
            s.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    Ok(())
}

fn apply_overlay(base: &mut Value, ov: &Value, v85: bool, file: &str) -> Result<(), SchemaError> {
    let basem = obj_mut(base, file)?;
    let Some(ovm) = ov.as_object() else {
        return Err(err(file, "overlay: object expected"));
    };
    for (ctx, d) in ovm {
        let c = basem
            .entry(ctx.clone())
            .or_insert_with(|| serde_json::json!({"order": [], "props": {}}));
        let c = obj_mut(c, file)?;
        if let Some(o) = d
            .get("order")
            .filter(|o| o.as_array().is_some_and(|a| !a.is_empty()))
        {
            c.insert("order".into(), o.clone());
        }
        let Some(props) = d.get("props").and_then(Value::as_object) else {
            continue;
        };
        let cp = c
            .entry("props")
            .or_insert_with(|| Value::Object(Map::new()));
        let cp = obj_mut(cp, file)?;
        for (prop, e) in props {
            let moved = e.get("moved").is_some_and(truthy);
            if v85 || !moved {
                cp.insert(prop.clone(), e.clone());
            }
        }
    }
    Ok(())
}

fn apply_compat(base: &mut Value, compat: &Value) -> Result<(), SchemaError> {
    let file = "compat8312";
    let basem = obj_mut(base, file)?;
    let Some(cm) = compat.as_object() else {
        return Err(err(file, "object expected"));
    };
    for (ctx, d) in cm.iter().filter(|(k, _)| !k.starts_with('_')) {
        let Some(props) = d.get("props").and_then(Value::as_object) else {
            return Err(err(file, format!("{ctx}: props expected")));
        };
        for (prop, e) in props {
            let target = basem
                .get_mut(ctx)
                .and_then(|c| c.get_mut("props"))
                .and_then(|p| p.get_mut(prop))
                .and_then(Value::as_object_mut)
                .ok_or_else(|| err(file, format!("{ctx}.{prop}: not in the learned table")))?;
            if let Some(dflt) = e.get("default") {
                target.insert("default".into(), dflt.clone());
            }
            if let Some(t) = e.get("table").and_then(Value::as_object) {
                let tt = target
                    .entry("table")
                    .or_insert_with(|| Value::Object(Map::new()));
                let tt = obj_mut(tt, file)?;
                for (k, v) in t {
                    tt.insert(k.clone(), v.clone());
                }
            }
        }
    }
    Ok(())
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn typed_context(ctx: &str, d: &Value) -> Result<Context, SchemaError> {
    let order = d
        .get("order")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let mut props = Vec::new();
    if let Some(m) = d.get("props").and_then(Value::as_object) {
        for (prop, e) in m {
            if !e.is_object() {
                return Err(err("learned", format!("{ctx}.{prop}: object expected")));
            }
            props.push((
                prop.clone(),
                PropEntry {
                    path: e.get("path").and_then(Value::as_array).cloned(),
                    codec: e
                        .get("codec")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    table: e
                        .get("table")
                        .and_then(Value::as_object)
                        .map(|t| {
                            t.iter()
                                .map(|(k, v)| (k.clone(), TableValue::new(v.clone())))
                                .collect()
                        })
                        .unwrap_or_default(),
                    default: e
                        .get("default")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                    moved: e.get("moved").is_some_and(truthy),
                    raw: e.clone(),
                },
            ));
        }
    }
    Ok(Context { order, props })
}

fn build(
    resource: &'static crate::SchemaResource,
    v85: bool,
    compat: FormCompat,
    private: Option<&Path>,
) -> Result<FormTables, SchemaError> {
    let mut learned = parse("learned", resource.json("forms/learned.json")?)?;
    let mut ov = parse("learned85", resource.json("forms/learned85.json")?)?;
    let mut paths = parse("paths", resource.json("forms/paths.json")?)?;
    if let Some(dir) = private {
        if let Some(p) = read_private(dir, "learned")? {
            merge_learned(&mut learned, &p, "learned")?;
        }
        if let Some(p) = read_private(dir, "learned85")? {
            merge_learned(&mut ov, &p, "learned85")?;
        }
        if let Some(p) = read_private(dir, "paths")? {
            merge_paths(&mut paths, &p, "paths")?;
        }
    }
    apply_overlay(&mut learned, &ov, v85, "learned")?;
    let mut types = parse("types", resource.json("forms/types.json")?)?;
    if compat == FormCompat::V8_3_12 {
        let c = parse("compat8312", resource.json("forms/compat8312.json")?)?;
        apply_compat(&mut learned, &c)?;
        if let Some(t) = c.get("_types").and_then(Value::as_object) {
            let tm = obj_mut(&mut types, "types")?;
            for (k, v) in t {
                tm.insert(k.clone(), v.clone());
            }
        }
    }
    let learned = learned
        .as_object()
        .ok_or_else(|| err("learned", "object expected"))?
        .iter()
        .map(|(ctx, d)| typed_context(ctx, d).map(|c| (ctx.clone(), c)))
        .collect::<Result<_, _>>()?;
    Ok(FormTables {
        learned,
        paths,
        events: parse("events", resource.json("forms/events.json")?)?,
        misc: parse("misc", resource.json("forms/misc.json")?)?,
        types,
    })
}

pub(crate) type Key = (bool, FormCompat, Option<PathBuf>);

pub fn form_tables(v85: bool, private: Option<&Path>) -> Result<Arc<FormTables>, SchemaError> {
    form_tables_mode(v85, FormCompat::Base, private)
}

pub fn form_tables_mode(
    v85: bool,
    compat: FormCompat,
    private: Option<&Path>,
) -> Result<Arc<FormTables>, SchemaError> {
    crate::embedded_resource()?.form_tables_mode(v85, compat, private)
}
impl crate::SchemaResource {
    pub fn form_tables_mode(
        &'static self,
        v85: bool,
        compat: FormCompat,
        private: Option<&Path>,
    ) -> Result<Arc<FormTables>, SchemaError> {
        let key = (v85, compat, private.map(Path::to_path_buf));
        let mut cache = self.forms.lock().map_err(|_| err("cache", "poisoned"))?;
        if let Some(t) = cache.get(&key) {
            return Ok(Arc::clone(t));
        }
        let t = Arc::new(build(self, v85, compat, private)?);
        cache.insert(key, Arc::clone(&t));
        Ok(t)
    }
}

pub type KindStdAttrs = (String, Vec<(String, String)>);

pub fn std_attrs() -> Result<&'static [KindStdAttrs], SchemaError> {
    crate::embedded_resource()?.std_attrs()
}
impl crate::SchemaResource {
    pub fn std_attrs(&'static self) -> Result<&'static [KindStdAttrs], SchemaError> {
        self.stdattrs
            .get_or_init(|| {
                let v: Value = serde_json::from_str(self.json("stdattrs.json").map_err(|e| e.0)?)
                    .map_err(|e| format!("stdattrs.json: {e}"))?;
                let top = v.as_object().ok_or("stdattrs.json: object expected")?;
                let mut out = Vec::new();
                for (kind, m) in top {
                    let Some(m) = m.as_object() else { continue };
                    let mut ids = Vec::with_capacity(m.len());
                    for (id, nm) in m {
                        let nm = nm.as_str().ok_or_else(|| {
                            format!("stdattrs.json: {kind}.{id}: string expected")
                        })?;
                        ids.push((id.clone(), nm.to_string()));
                    }
                    out.push((kind.clone(), ids));
                }
                Ok(out)
            })
            .as_ref()
            .map(Vec::as_slice)
            .map_err(|e| SchemaError(e.clone()))
    }
}

const NAME_TOKEN: &str = "⟨N⟩";
const AUTO_WORDS: [&str; 8] = [
    "ПанельДействийВыделенныхСтрок",
    "УправлениеПоиском",
    "СтрокаПоиска",
    "СостояниеПросмотра",
    "КонтекстноеМеню",
    "РасширеннаяПодсказка",
    "КоманднаяПанель",
    "ДействияСтроки",
];
const CFG_KINDS: &str = "Catalog|Document|Enum|InformationRegister|AccumulationRegister|\
AccountingRegister|CalculationRegister|DataProcessor|Report|ChartOfAccounts|\
ChartOfCharacteristicTypes|ChartOfCalculationTypes|Task|BusinessProcess|ExchangePlan|\
Constant|CommonPicture|CommonCommand|CommonForm|CommonTemplate|StyleItem|DefinedType|\
FunctionalOption|SessionParameter|Role|CommonModule|DocumentJournal|FilterCriterion|\
SettingsStorage|Subsystem|WebService|HTTPService|XDTOPackage|CommandGroup";

struct PrivateRes {
    cyr: regex::Regex,
    cfgref: regex::Regex,
    derived: regex::Regex,
}

static PRIVATE_RES: LazyLock<Option<PrivateRes>> = LazyLock::new(|| {
    let derived = format!("\"{NAME_TOKEN}(?:{})*\"", AUTO_WORDS.join("|"));
    Some(PrivateRes {
        cyr: regex::Regex::new("[А-Яа-яЁё]").ok()?,
        cfgref: regex::Regex::new(&format!(r"\b(?:cfg:\w+\.\w|(?:{CFG_KINDS})\.\w)")).ok()?,
        derived: regex::Regex::new(&derived).ok()?,
    })
});

pub fn is_private(s: &str) -> bool {
    let Some(r) = PRIVATE_RES.as_ref() else {
        return true;
    };
    let s = r.derived.replace_all(s, "\"\"");
    r.cyr.is_match(&s) || r.cfgref.is_match(&s)
}
