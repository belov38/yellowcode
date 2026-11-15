use std::fmt;

use crate::tabular::{TreeRow, ValueList, ValueTable, ValueTree};

pub const MAX_DEPTH: usize = 256;

pub fn fits_depth(v: &Value) -> bool {
    enum Node<'a> {
        Value(&'a Value),
        Row(&'a TreeRow),
    }
    let mut stack = vec![(Node::Value(v), 0usize)];
    while let Some((node, level)) = stack.pop() {
        match node {
            Node::Row(row) => {
                stack.extend(row.values().iter().map(|c| (Node::Value(c), level)));
                for child in row.rows() {
                    if level + 1 >= MAX_DEPTH {
                        return false;
                    }
                    stack.push((Node::Row(child), level + 1));
                }
            }
            Node::Value(v) => {
                if v.kind().is_primitive() {
                    continue;
                }
                if level >= MAX_DEPTH {
                    return false;
                }
                let inner = level + 1;
                match v {
                    Value::Array(a) => {
                        stack.extend(a.items.iter().map(|x| (Node::Value(x), inner)))
                    }
                    Value::Structure(st) => {
                        stack.extend(st.props().iter().map(|(_, x)| (Node::Value(x), inner)))
                    }
                    Value::Map(m) => {
                        for (k, x) in m.pairs() {
                            stack.push((Node::Value(k), inner));
                            stack.push((Node::Value(x), inner));
                        }
                    }
                    Value::ValueTable(t) => {
                        stack.extend(t.rows().iter().flatten().map(|x| (Node::Value(x), inner)))
                    }
                    Value::ValueTree(t) => {
                        stack.extend(t.rows().iter().map(|r| (Node::Row(r), inner)))
                    }
                    Value::ValueList(l) => {
                        stack.extend(l.items().iter().map(|i| (Node::Value(&i.value), inner)))
                    }
                    _ => {}
                }
            }
        }
    }
    true
}

pub const NS_XS: &str = "http://www.w3.org/2001/XMLSchema";
pub const NS_CORE: &str = "http://v8.1c.ru/8.1/data/core";
pub const NS_CONFIG: &str = "http://v8.1c.ru/8.1/data/enterprise/current-config";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValueError {
    #[error("not a number: {0:?}")]
    Number(String),
    #[error("not a date: {0:?}")]
    Date(String),
    #[error("not a boolean: {0:?}")]
    Bool(String),
    #[error("not a UUID: {0:?}")]
    Uuid(String),
    #[error("{0:?} is not a primitive kind")]
    NotPrimitive(Kind),
    #[error("not a structure key: {0:?}")]
    StructureKey(String),
    #[error("a map key must be a primitive value, not {0:?}")]
    MapKey(Kind),
    #[error("{0:?} needs a type name besides the text")]
    NeedsType(Kind),
    #[error("not a reference type: {0}")]
    RefType(String),
    #[error("not an enumeration value name: {0:?}")]
    EnumName(String),
    #[error("not a column name: {0:?}")]
    ColumnName(String),
    #[error("repeated column name {0:?}")]
    RepeatedColumn(String),
    #[error("a row has {values} values for {columns} columns")]
    RowWidth { columns: usize, values: usize },
    #[error("bad index column {0:?}")]
    IndexColumn(String),
    #[error("an index without columns")]
    EmptyIndex,
    #[error("repeated value list id {0}")]
    RepeatedListId(u64),
    #[error("value list lastId {0} is below -1, below an item id or the largest number")]
    LastId(i64),
    #[error("value list has no id left after {0}")]
    ListIdsExhausted(i64),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(Decimal),
    String(String),
    Date(Date),
    Uuid(Uuid),
    Ref(Ref),
    EnumRef(EnumRef),
    Array(Array),
    Structure(Structure),
    Map(Map),
    ValueTable(ValueTable),
    ValueTree(ValueTree),
    ValueList(ValueList),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Undefined,
    Null,
    Bool,
    Number,
    String,
    Date,
    Uuid,
    Ref,
    EnumRef,
    Array,
    Structure,
    Map,
    ValueTable,
    ValueTree,
    ValueList,
}

impl Kind {
    pub fn is_primitive(self) -> bool {
        !matches!(
            self,
            Kind::Array
                | Kind::Structure
                | Kind::Map
                | Kind::ValueTable
                | Kind::ValueTree
                | Kind::ValueList
        )
    }
}

impl Value {
    pub fn kind(&self) -> Kind {
        match self {
            Value::Undefined => Kind::Undefined,
            Value::Null => Kind::Null,
            Value::Bool(_) => Kind::Bool,
            Value::Number(_) => Kind::Number,
            Value::String(_) => Kind::String,
            Value::Date(_) => Kind::Date,
            Value::Uuid(_) => Kind::Uuid,
            Value::Ref(_) => Kind::Ref,
            Value::EnumRef(_) => Kind::EnumRef,
            Value::Array(_) => Kind::Array,
            Value::Structure(_) => Kind::Structure,
            Value::Map(_) => Kind::Map,
            Value::ValueTable(_) => Kind::ValueTable,
            Value::ValueTree(_) => Kind::ValueTree,
            Value::ValueList(_) => Kind::ValueList,
        }
    }

    pub fn type_name(&self) -> Option<TypeName> {
        let core = TypeName::core;
        Some(match self {
            Value::Undefined => return None,
            Value::Null => core("Null"),
            Value::Bool(_) => TypeName::xs("boolean"),
            Value::Number(_) => TypeName::xs("decimal"),
            Value::String(_) => TypeName::xs("string"),
            Value::Date(_) => TypeName::xs("dateTime"),
            Value::Uuid(_) => core("UUID"),
            Value::Ref(r) => r.type_name().clone(),
            Value::EnumRef(e) => e.type_name().clone(),
            Value::Array(a) if a.fixed => core("FixedArray"),
            Value::Array(_) => core("Array"),
            Value::Structure(st) if st.is_fixed() => core("FixedStructure"),
            Value::Structure(_) => core("Structure"),
            Value::Map(m) if m.is_fixed() => core("FixedMap"),
            Value::Map(_) => core("Map"),
            Value::ValueTable(_) => core("ValueTable"),
            Value::ValueTree(_) => core("ValueTree"),
            Value::ValueList(_) => core("ValueListType"),
        })
    }

    pub fn lexical(&self) -> Option<String> {
        match self {
            Value::Undefined | Value::Null => None,
            Value::Array(_)
            | Value::Structure(_)
            | Value::Map(_)
            | Value::ValueTable(_)
            | Value::ValueTree(_)
            | Value::ValueList(_) => None,
            Value::Ref(r) => Some(r.uuid().to_string()),
            Value::EnumRef(e) => Some(e.name().to_string()),
            Value::Bool(b) => Some(if *b { "true" } else { "false" }.to_string()),
            Value::Number(n) => Some(n.as_str().to_string()),
            Value::String(s) => Some(s.clone()),
            Value::Date(d) => Some(d.to_string()),
            Value::Uuid(u) => Some(u.to_string()),
        }
    }

    pub fn from_lexical(kind: Kind, text: &str) -> Result<Value, ValueError> {
        Ok(match kind {
            Kind::Array
            | Kind::Structure
            | Kind::Map
            | Kind::ValueTable
            | Kind::ValueTree
            | Kind::ValueList => {
                return Err(ValueError::NotPrimitive(kind));
            }
            Kind::Ref | Kind::EnumRef => return Err(ValueError::NeedsType(kind)),
            Kind::Undefined => Value::Undefined,
            Kind::Null => Value::Null,
            Kind::Bool => match text {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => return Err(ValueError::Bool(text.to_string())),
            },
            Kind::Number => Value::Number(Decimal::parse(text)?),
            Kind::String => Value::String(text.to_string()),
            Kind::Date => Value::Date(Date::parse(text)?),
            Kind::Uuid => Value::Uuid(Uuid::parse(text)?),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeName {
    ns: String,
    local: String,
}

const KNOWN_PREFIXES: [(&str, &str); 3] = [("xs", NS_XS), ("v8", NS_CORE), ("cfg", NS_CONFIG)];

impl TypeName {
    pub fn new(ns: &str, local: &str) -> TypeName {
        TypeName {
            ns: ns.to_string(),
            local: local.to_string(),
        }
    }

    pub fn xs(local: &str) -> TypeName {
        TypeName::new(NS_XS, local)
    }

    pub fn core(local: &str) -> TypeName {
        TypeName::new(NS_CORE, local)
    }

    pub fn config(local: &str) -> TypeName {
        TypeName::new(NS_CONFIG, local)
    }

    pub fn ns(&self) -> &str {
        &self.ns
    }

    pub fn local(&self) -> &str {
        &self.local
    }

    pub fn known_prefix(&self) -> Option<&'static str> {
        KNOWN_PREFIXES
            .iter()
            .find(|(_, ns)| *ns == self.ns)
            .map(|(p, _)| *p)
    }

    pub fn parse(text: &str) -> Option<TypeName> {
        let (ns, local) = if let Some(rest) = text.strip_prefix('{') {
            let (ns, local) = rest.split_once('}')?;
            (ns.to_string(), local)
        } else {
            let (prefix, local) = text.split_once(':')?;
            let (_, ns) = KNOWN_PREFIXES.iter().find(|(p, _)| *p == prefix)?;
            (ns.to_string(), local)
        };
        if local.is_empty() || local.contains([':', '{', '}']) {
            return None;
        }
        Some(TypeName {
            ns,
            local: local.to_string(),
        })
    }
}

impl fmt::Display for TypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.known_prefix() {
            Some(p) => write!(f, "{p}:{}", self.local),
            None => write!(f, "{{{}}}{}", self.ns, self.local),
        }
    }
}

pub(crate) const UUID_REF_KINDS: [&str; 8] = [
    "CatalogRef",
    "DocumentRef",
    "ChartOfCharacteristicTypesRef",
    "ChartOfAccountsRef",
    "ChartOfCalculationTypesRef",
    "ExchangePlanRef",
    "BusinessProcessRef",
    "TaskRef",
];

pub(crate) fn config_ref_class(ty: &TypeName) -> Option<bool> {
    if ty.ns != NS_CONFIG {
        return None;
    }
    let (kind, name) = ty.local.split_once('.')?;
    if name.is_empty() {
        return None;
    }
    if kind == "EnumRef" {
        Some(true)
    } else if UUID_REF_KINDS.contains(&kind) {
        Some(false)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ref {
    ty: TypeName,
    uuid: Uuid,
}

impl Ref {
    pub fn new(ty: TypeName, uuid: Uuid) -> Result<Ref, ValueError> {
        if config_ref_class(&ty) != Some(false) {
            return Err(ValueError::RefType(ty.to_string()));
        }
        Ok(Ref { ty, uuid })
    }

    pub fn type_name(&self) -> &TypeName {
        &self.ty
    }

    pub fn uuid(&self) -> Uuid {
        self.uuid
    }

    pub fn is_empty(&self) -> bool {
        self.uuid == Uuid::from_bytes([0; 16])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnumRef {
    ty: TypeName,
    name: String,
}

impl EnumRef {
    pub fn new(ty: TypeName, name: &str) -> Result<EnumRef, ValueError> {
        if config_ref_class(&ty) != Some(true) {
            return Err(ValueError::RefType(ty.to_string()));
        }
        if !name.is_empty() && !is_structure_key(name) {
            return Err(ValueError::EnumName(name.to_string()));
        }
        Ok(EnumRef {
            ty,
            name: name.to_string(),
        })
    }

    pub fn type_name(&self) -> &TypeName {
        &self.ty
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Array {
    pub fixed: bool,
    pub items: Vec<Value>,
}

impl Array {
    pub fn new(items: Vec<Value>) -> Array {
        Array {
            fixed: false,
            items,
        }
    }

    pub fn fixed(items: Vec<Value>) -> Array {
        Array { fixed: true, items }
    }
}

pub fn is_structure_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

pub(crate) fn fold_key(key: &str) -> String {
    key.chars()
        .map(|c| {
            let mut l = c.to_lowercase();
            match (l.next(), l.next()) {
                (Some(x), None) => x,
                _ => c,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Structure {
    fixed: bool,
    props: Vec<(String, Value)>,
}

impl Structure {
    pub fn new() -> Structure {
        Structure::default()
    }

    pub fn new_fixed() -> Structure {
        Structure {
            fixed: true,
            props: Vec::new(),
        }
    }

    pub fn is_fixed(&self) -> bool {
        self.fixed
    }

    pub fn set_fixed(&mut self, fixed: bool) {
        self.fixed = fixed;
    }

    fn position(&self, key: &str) -> Option<usize> {
        let folded = fold_key(key);
        self.props.iter().position(|(k, _)| fold_key(k) == folded)
    }

    pub fn insert(&mut self, key: &str, value: Value) -> Result<Option<Value>, ValueError> {
        if !is_structure_key(key) {
            return Err(ValueError::StructureKey(key.to_string()));
        }
        Ok(match self.position(key) {
            Some(i) => {
                let slot = &mut self.props[i];
                slot.0 = key.to_string();
                Some(std::mem::replace(&mut slot.1, value))
            }
            None => {
                self.props.push((key.to_string(), value));
                None
            }
        })
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.position(key).map(|i| &self.props[i].1)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.position(key).is_some()
    }

    pub fn props(&self) -> &[(String, Value)] {
        &self.props
    }

    pub fn len(&self) -> usize {
        self.props.len()
    }

    pub fn is_empty(&self) -> bool {
        self.props.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Map {
    fixed: bool,
    pairs: Vec<(Value, Value)>,
}

impl Map {
    pub fn new() -> Map {
        Map::default()
    }

    pub fn new_fixed() -> Map {
        Map {
            fixed: true,
            pairs: Vec::new(),
        }
    }

    pub fn is_fixed(&self) -> bool {
        self.fixed
    }

    pub fn set_fixed(&mut self, fixed: bool) {
        self.fixed = fixed;
    }

    pub fn insert(&mut self, key: Value, value: Value) -> Result<Option<Value>, ValueError> {
        if !key.kind().is_primitive() {
            return Err(ValueError::MapKey(key.kind()));
        }
        Ok(match self.pairs.iter().position(|(k, _)| *k == key) {
            Some(i) => Some(std::mem::replace(&mut self.pairs[i].1, value)),
            None => {
                self.pairs.push((key, value));
                None
            }
        })
    }

    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn contains(&self, key: &Value) -> bool {
        self.get(key).is_some()
    }

    pub fn pairs(&self) -> &[(Value, Value)] {
        &self.pairs
    }

    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    pub fn eq_unordered(&self, other: &Map) -> bool {
        self.fixed == other.fixed
            && self.pairs.len() == other.pairs.len()
            && self.pairs.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Decimal(String);

impl Decimal {
    pub fn parse(s: &str) -> Result<Decimal, ValueError> {
        let bad = || ValueError::Number(s.to_string());
        let (neg, body) = match s.as_bytes().first() {
            Some(b'-') => (true, &s[1..]),
            Some(b'+') => (false, &s[1..]),
            _ => (false, s),
        };
        let (int, frac) = body.split_once('.').unwrap_or((body, ""));
        if int.is_empty() && frac.is_empty() {
            return Err(bad());
        }
        if !int.bytes().chain(frac.bytes()).all(|c| c.is_ascii_digit()) {
            return Err(bad());
        }
        let int = int.trim_start_matches('0');
        let frac = frac.trim_end_matches('0');
        let mut out = String::with_capacity(int.len() + frac.len() + 3);
        if neg && !(int.is_empty() && frac.is_empty()) {
            out.push('-');
        }
        out.push_str(if int.is_empty() { "0" } else { int });
        if !frac.is_empty() {
            out.push('.');
            out.push_str(frac);
        }
        Ok(Decimal(out))
    }

    pub fn from_i64(v: i64) -> Decimal {
        Decimal(v.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

fn days_in_month(y: u32, m: u32) -> u32 {
    let leap = (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400);
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    }
}

impl Date {
    pub const EMPTY: Date = Date {
        year: 1,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
    };

    pub fn new(y: u32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<Date> {
        if !(1..=9999).contains(&y) || d < 1 || d > days_in_month(y, mo) {
            return None;
        }
        if h >= 24 || mi >= 60 || s >= 60 {
            return None;
        }
        Some(Date {
            year: y as u16,
            month: mo as u8,
            day: d as u8,
            hour: h as u8,
            minute: mi as u8,
            second: s as u8,
        })
    }

    pub fn parse(s: &str) -> Result<Date, ValueError> {
        let bad = || ValueError::Date(s.to_string());
        let b = s.as_bytes();
        if b.len() != 19
            || b[4] != b'-'
            || b[7] != b'-'
            || b[10] != b'T'
            || b[13] != b':'
            || b[16] != b':'
        {
            return Err(bad());
        }
        let num = |from: usize, to: usize| -> Result<u32, ValueError> {
            let part = &b[from..to];
            if !part.iter().all(u8::is_ascii_digit) {
                return Err(bad());
            }
            Ok(part
                .iter()
                .fold(0, |acc, &c| acc * 10 + u32::from(c - b'0')))
        };
        Date::new(
            num(0, 4)?,
            num(5, 7)?,
            num(8, 10)?,
            num(11, 13)?,
            num(14, 16)?,
            num(17, 19)?,
        )
        .ok_or_else(bad)
    }

    pub fn parse_xsd(s: &str) -> Result<Date, ValueError> {
        let bad = || ValueError::Date(s.to_string());
        let head = s.get(..19).ok_or_else(bad)?;
        let mut rest = &s[19..];
        if let Some(frac) = rest.strip_prefix('.') {
            let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return Err(bad());
            }
            rest = &frac[digits..];
        }
        let zone_ok = match rest.as_bytes() {
            [] | [b'Z'] => true,
            [b'+' | b'-', h1, h2, b':', m1, m2] => {
                [h1, h2, m1, m2].iter().all(|c| c.is_ascii_digit())
            }
            _ => false,
        };
        if !zone_ok {
            return Err(bad());
        }
        Date::parse(head).map_err(|_| bad())
    }

    pub fn is_empty(&self) -> bool {
        *self == Date::EMPTY
    }

    pub fn parts(&self) -> (u32, u32, u32, u32, u32, u32) {
        (
            u32::from(self.year),
            u32::from(self.month),
            u32::from(self.day),
            u32::from(self.hour),
            u32::from(self.minute),
            u32::from(self.second),
        )
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Uuid([u8; 16]);

impl Uuid {
    pub fn from_bytes(b: [u8; 16]) -> Uuid {
        Uuid(b)
    }

    pub fn bytes(&self) -> [u8; 16] {
        self.0
    }

    pub fn parse(s: &str) -> Result<Uuid, ValueError> {
        let bad = || ValueError::Uuid(s.to_string());
        let b = s.as_bytes();
        if b.len() != 36 || [8, 13, 18, 23].iter().any(|&i| b[i] != b'-') {
            return Err(bad());
        }
        let mut digits = b.iter().filter(|&&c| c != b'-');
        let mut out = [0u8; 16];
        for byte in &mut out {
            let (Some(&h), Some(&l)) = (digits.next(), digits.next()) else {
                return Err(bad());
            };
            let hex = |c: u8| (c as char).to_digit(16).ok_or_else(bad);
            *byte = ((hex(h)? << 4) | hex(l)?) as u8;
        }
        Ok(Uuid(out))
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, byte) in self.0.iter().enumerate() {
            if matches!(i, 4 | 6 | 8 | 10) {
                f.write_str("-")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
