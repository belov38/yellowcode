use crate::LStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum DateParts {
    Date,
    Time,
    DateTime,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct TypeDesc {
    pub types: Vec<String>,
    pub string_len: Option<u32>,
    pub number: Option<(u32, u32)>,
    pub date: Option<DateParts>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeKind {
    None,
    String(u32),
    Number { digits: u32, fraction: u32 },
    Boolean,
    Date(DateParts),
    Ref(String),
    Enum(String),
    ValueTable,
    ValueTree,
    ValueList,
    DynamicList,
    Composite,
    Other(String),
}

impl TypeDesc {
    pub fn kind(&self) -> TypeKind {
        let t = match self.types.as_slice() {
            [] => return TypeKind::None,
            [t] => t.as_str(),
            _ => return TypeKind::Composite,
        };
        match t {
            "xs:string" => TypeKind::String(self.string_len.unwrap_or(0)),
            "xs:decimal" => {
                let (digits, fraction) = self.number.unwrap_or((0, 0));
                TypeKind::Number { digits, fraction }
            }
            "xs:boolean" => TypeKind::Boolean,
            "xs:dateTime" => TypeKind::Date(self.date.unwrap_or(DateParts::DateTime)),
            "v8:ValueTable" => TypeKind::ValueTable,
            "v8:ValueTree" => TypeKind::ValueTree,
            "v8:ValueListType" => TypeKind::ValueList,
            "cfg:DynamicList" => TypeKind::DynamicList,
            "cfg:AnyRef" | "cfg:AnyIBRef" => TypeKind::Ref(t.to_string()),
            _ if t.starts_with("cfg:EnumRef.") => TypeKind::Enum(t.to_string()),
            _ if t.starts_with("cfg:") && (t.contains("Ref.") || t.ends_with("Ref")) => {
                TypeKind::Ref(t.to_string())
            }
            _ => TypeKind::Other(t.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub ty: TypeDesc,
    pub title: Option<LStr>,
}

pub trait TypeResolver {
    fn resolve(&self, owner: &str, path: &[&str]) -> Option<Resolved>;
}

pub struct NoConfig;

impl TypeResolver for NoConfig {
    fn resolve(&self, _: &str, _: &[&str]) -> Option<Resolved> {
        None
    }
}
