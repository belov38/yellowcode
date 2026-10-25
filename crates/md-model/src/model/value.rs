use brace::Guid;
use mdschema::Codec;

#[derive(Debug, Clone, PartialEq)]
pub struct Prop {
    pub name: String,
    pub codec: Codec,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    Bool(bool),
    LStr(LocalString),
    Ref(MdRef),
    Type(TypeDescription),
    Typed(TypedValue),
    RefList(Vec<MdRef>),
    FieldList(Vec<String>),
    StandardAttributes(Vec<StandardAttribute>),
    Xml(XmlElem),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LocalString(pub Vec<(String, String)>);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MdRef {
    pub id: Option<Guid>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TypeDescription {
    pub types: Vec<TypeRef>,
    pub number: Option<NumberQualifiers>,
    pub string: Option<StringQualifiers>,
    pub date: Option<DateFractions>,
    pub binary: Option<BinaryQualifiers>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRef {
    pub qname: String,
    pub set: bool,
    pub type_id: Option<Guid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NumberQualifiers {
    pub digits: u32,
    pub fraction: u32,
    pub nonnegative: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StringQualifiers {
    pub length: u32,
    pub fixed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DateFractions {
    Date,
    Time,
    DateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BinaryQualifiers {
    pub length: u32,
    pub fixed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypedValue {
    Nil,
    Str(String),
    Number(String),
    Bool(bool),
    Date(String),
    DesignTimeRef(String),
    EmptyTypeDescription,
    FixedArray(Vec<TypedValue>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct StandardAttribute {
    pub name: String,
    pub props: Vec<Prop>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct XmlElem {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub text: Option<String>,
    pub children: Vec<XmlElem>,
    pub expanded: bool,
}

impl XmlElem {
    pub fn new(name: &str) -> Self {
        XmlElem {
            name: name.into(),
            ..Default::default()
        }
    }

    pub fn leaf(name: &str, text: &str) -> Self {
        XmlElem {
            name: name.into(),
            text: Some(text.into()),
            ..Default::default()
        }
    }

    #[must_use]
    pub fn with_attr(mut self, name: &str, value: &str) -> Self {
        self.attrs.push((name.into(), value.into()));
        self
    }

    #[must_use]
    pub fn expanded(mut self) -> Self {
        self.expanded = true;
        self
    }

    #[must_use]
    pub fn child(mut self, elem: XmlElem) -> Self {
        self.children.push(elem);
        self
    }
}
