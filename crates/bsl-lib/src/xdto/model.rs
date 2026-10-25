use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct QName {
    pub ns: String,
    pub name: String,
}

impl QName {
    pub fn new(ns: &str, name: &str) -> QName {
        QName {
            ns: ns.to_string(),
            name: name.to_string(),
        }
    }

    pub fn clark(&self) -> String {
        format!("{{{}}}{}", self.ns, self.name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhiteSpace {
    Preserve,
    Replace,
    Collapse,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Facets {
    pub length: Option<u64>,
    pub min_length: Option<u64>,
    pub max_length: Option<u64>,
    pub patterns: Vec<String>,
    pub enumeration: Vec<String>,
    pub white_space: Option<WhiteSpace>,
    pub min_inclusive: Option<String>,
    pub max_inclusive: Option<String>,
    pub min_exclusive: Option<String>,
    pub max_exclusive: Option<String>,
    pub total_digits: Option<u32>,
    pub fraction_digits: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Variety {
    Atomic,
    List { item: QName },
    Union { members: Vec<QName> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueType {
    pub name: QName,
    pub base: Option<QName>,
    pub variety: Variety,
    pub facets: Facets,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectType {
    pub name: QName,
    pub base: Option<QName>,
    pub open: bool,
    pub abstract_: bool,
    pub mixed: bool,
    pub sequenced: bool,
    pub ordered: bool,
    pub properties: Vec<Property>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeDef {
    Value(ValueType),
    Object(ObjectType),
}

impl TypeDef {
    pub fn name(&self) -> &QName {
        match self {
            TypeDef::Value(v) => &v.name,
            TypeDef::Object(o) => &o.name,
        }
    }

    pub fn base(&self) -> Option<&QName> {
        match self {
            TypeDef::Value(v) => v.base.as_ref(),
            TypeDef::Object(o) => o.base.as_ref(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Attribute,
    Element,
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub ty: QName,
    pub lower: u32,
    pub upper: i64,
    pub nillable: bool,
    pub form: Form,
    pub qualified: bool,
    pub default: Option<String>,
    pub local_name: Option<String>,
    pub fixed: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Package {
    pub ns: String,
    pub element_qualified: bool,
    pub attribute_qualified: bool,
    pub types: Vec<TypeDef>,
    pub anonymous_types: Vec<usize>,
    pub globals: Vec<Property>,
}

impl Package {
    pub fn named_type_indices(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.types.len()).filter(|i| !self.anonymous_types.contains(i))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Model {
    pub packages: Vec<Arc<Package>>,
    index: HashMap<String, usize>,
    types: Vec<Arc<HashMap<String, usize>>>,
}

fn type_index(p: &Package) -> HashMap<String, usize> {
    let mut m = HashMap::with_capacity(p.types.len());
    for (j, t) in p.types.iter().enumerate() {
        m.entry(t.name().name.clone()).or_insert(j);
    }
    m
}

impl Model {
    pub fn new(packages: Vec<Package>) -> Model {
        let types = packages.iter().map(|p| Arc::new(type_index(p))).collect();
        Model::assemble(packages.into_iter().map(Arc::new).collect(), types)
    }

    fn assemble(packages: Vec<Arc<Package>>, types: Vec<Arc<HashMap<String, usize>>>) -> Model {
        let mut index = HashMap::with_capacity(packages.len());
        for (i, p) in packages.iter().enumerate() {
            index.entry(p.ns.clone()).or_insert(i);
        }
        Model {
            packages,
            index,
            types,
        }
    }

    pub fn package(&self, ns: &str) -> Option<&Package> {
        self.index.get(ns).map(|&i| self.packages[i].as_ref())
    }

    pub(crate) fn type_at(&self, q: &QName) -> Option<(&Arc<Package>, usize)> {
        let &pi = self.index.get(q.ns.as_str())?;
        let &ti = self.types[pi].get(q.name.as_str())?;
        Some((&self.packages[pi], ti))
    }

    pub fn type_def(&self, q: &QName) -> Option<&TypeDef> {
        let (p, ti) = self.type_at(q)?;
        p.types.get(ti)
    }

    pub fn with(&self, extra: Vec<Package>) -> Model {
        let mut packages = self.packages.clone();
        let mut types = self.types.clone();
        for p in extra {
            match packages.iter().position(|q| q.ns == p.ns) {
                Some(i) => {
                    let mut merged = (*packages[i]).clone();
                    let offset = merged.types.len();
                    merged
                        .anonymous_types
                        .extend(p.anonymous_types.into_iter().map(|j| offset + j));
                    merged.types.extend(p.types);
                    merged.globals.extend(p.globals);
                    types[i] = Arc::new(type_index(&merged));
                    packages[i] = Arc::new(merged);
                }
                None => {
                    types.push(Arc::new(type_index(&p)));
                    packages.push(Arc::new(p));
                }
            }
        }
        Model::assemble(packages, types)
    }
}
