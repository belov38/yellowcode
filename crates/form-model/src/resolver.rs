use crate::{DateParts, LStr, Resolved, TypeDesc};
use mdconf::{DateFractions, LocalString, MdKind, MdObject, TypeDescription, TypeRef, Value};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolverOwner<'a> {
    pub kind: MdKind,
    pub name: &'a str,
    constants: bool,
}
impl<'a> ResolverOwner<'a> {
    pub fn parse(owner: &'a str, path: &[&'a str]) -> Option<Self> {
        let owner = owner.strip_prefix("cfg:").unwrap_or(owner);
        if owner == "ConstantsSet" {
            let [name] = path else { return None };
            return Some(Self {
                kind: MdKind::Constant,
                name,
                constants: true,
            });
        }
        let (head, name) = owner.split_once('.')?;
        let kind = |name: &str| {
            MdKind::all()
                .iter()
                .copied()
                .find(|kind| kind.as_str() == name)
        };
        let kind = ["RecordManager", "RecordSet", "Object", "Ref"]
            .iter()
            .find_map(|suffix| head.strip_suffix(suffix).and_then(kind))
            .or_else(|| kind(head))?;
        Some(Self {
            kind,
            name,
            constants: false,
        })
    }
    pub fn resolve_selected<'o>(
        &self,
        object: &'o MdObject,
        path: &[&str],
    ) -> Option<ResolvedView<'o>> {
        if object.kind != self.kind || object.name != self.name {
            return None;
        }
        if self.constants {
            let [name] = path else { return None };
            if *name != self.name {
                return None;
            }
            let ty = match object.prop("Type") {
                Some(Value::Type(ty)) => TypeView::declared(ty),
                _ => TypeView::empty(),
            };
            return Some(ResolvedView {
                ty,
                title: TitleView::declared(&object.synonym),
            });
        }
        let (first, rest) = path.split_first()?;
        if rest.is_empty() {
            if let Some(child) = object
                .attributes()
                .chain(object.dimensions())
                .chain(object.resources())
                .find(|child| child.name == *first)
            {
                return child_resolved(child);
            }
            return standard(object, first);
        }
        let section = object
            .tabular_sections()
            .find(|child| child.name == *first)?;
        let [column] = rest else { return None };
        match section.children.iter().find(|child| child.name == *column) {
            Some(child) => child_resolved(child),
            None => {
                standard(object, column).filter(|_| matches!(*column, "НомерСтроки" | "LineNumber"))
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TypeNamesView<'a> {
    Empty,
    Declared(&'a [TypeRef]),
    Scalar(&'static str),
    Reference { kind: MdKind, name: &'a str },
}
struct ReferenceName<'a> {
    kind: MdKind,
    name: &'a str,
}
impl fmt::Display for ReferenceName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cfg:{}Ref.{}", self.kind.as_str(), self.name)
    }
}
impl Serialize for ReferenceName<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}
impl Serialize for TypeNamesView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let len = match self {
            Self::Empty => 0,
            Self::Declared(names) => names.len(),
            _ => 1,
        };
        let mut seq = serializer.serialize_seq(Some(len))?;
        match self {
            Self::Empty => {}
            Self::Declared(names) => {
                for name in *names {
                    seq.serialize_element(&format_args!("{}", name.name))?;
                }
            }
            Self::Scalar(name) => seq.serialize_element(name)?,
            Self::Reference { kind, name } => {
                seq.serialize_element(&ReferenceName { kind: *kind, name })?
            }
        }
        seq.end()
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct TypeView<'a> {
    pub types: TypeNamesView<'a>,
    pub string_len: Option<u32>,
    pub number: Option<(u32, u32)>,
    pub date: Option<DateParts>,
}
impl<'a> TypeView<'a> {
    fn empty() -> Self {
        Self {
            types: TypeNamesView::Empty,
            string_len: None,
            number: None,
            date: None,
        }
    }
    fn scalar(name: &'static str) -> Self {
        Self {
            types: TypeNamesView::Scalar(name),
            ..Self::empty()
        }
    }
    fn string(length: u32) -> Self {
        Self {
            string_len: Some(length),
            ..Self::scalar("xs:string")
        }
    }
    fn number(digits: u32, fraction: u32) -> Self {
        Self {
            number: Some((digits, fraction)),
            ..Self::scalar("xs:decimal")
        }
    }
    fn declared(ty: &'a TypeDescription) -> Self {
        Self {
            types: TypeNamesView::Declared(&ty.types),
            string_len: ty.string.map(|qualifier| qualifier.length),
            number: ty
                .number
                .map(|qualifier| (qualifier.digits, qualifier.fraction)),
            date: ty.date.map(|parts| match parts {
                DateFractions::Date => DateParts::Date,
                DateFractions::Time => DateParts::Time,
                DateFractions::DateTime => DateParts::DateTime,
            }),
        }
    }
    pub fn into_owned(self) -> TypeDesc {
        let types = match self.types {
            TypeNamesView::Empty => Vec::new(),
            TypeNamesView::Declared(names) => {
                names.iter().map(|name| name.name.to_string()).collect()
            }
            TypeNamesView::Scalar(name) => vec![name.to_owned()],
            TypeNamesView::Reference { kind, name } => {
                vec![ReferenceName { kind, name }.to_string()]
            }
        };
        TypeDesc {
            types,
            string_len: self.string_len,
            number: self.number,
            date: self.date,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub enum TitleView<'a> {
    Declared(&'a [(String, String)]),
    Russian(&'static str),
}
impl<'a> TitleView<'a> {
    fn declared(title: &'a LocalString) -> Option<Self> {
        (!title.0.is_empty()).then_some(Self::Declared(&title.0))
    }
    fn into_owned(self) -> LStr {
        LStr(match self {
            Self::Declared(pairs) => pairs.to_owned(),
            Self::Russian(title) => vec![("ru".to_owned(), title.to_owned())],
        })
    }
}
impl Serialize for TitleView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Declared(pairs) => pairs.serialize(serializer),
            Self::Russian(title) => [("ru", *title)].serialize(serializer),
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ResolvedView<'a> {
    pub ty: TypeView<'a>,
    pub title: Option<TitleView<'a>>,
}
impl ResolvedView<'_> {
    pub fn into_owned(self) -> Resolved {
        Resolved {
            ty: self.ty.into_owned(),
            title: self.title.map(TitleView::into_owned),
        }
    }
}
fn text_prop<'a>(object: &'a MdObject, name: &str) -> Option<&'a str> {
    match object.prop(name)? {
        Value::Text(text) => Some(text),
        _ => None,
    }
}
fn int_prop(object: &MdObject, name: &str) -> Option<u32> {
    text_prop(object, name)?.trim().parse().ok()
}
fn standard<'a>(object: &'a MdObject, name: &str) -> Option<ResolvedView<'a>> {
    let (ty, title) = match name {
        "Код" | "Code" => {
            let length = int_prop(object, "CodeLength").unwrap_or(9);
            (
                if text_prop(object, "CodeType") == Some("Number") {
                    TypeView::number(length, 0)
                } else {
                    TypeView::string(length)
                },
                "Код",
            )
        }
        "Наименование" | "Description" => (
            TypeView::string(int_prop(object, "DescriptionLength").unwrap_or(25)),
            "Наименование",
        ),
        "Номер" | "Number" => {
            let length = int_prop(object, "NumberLength").unwrap_or(9);
            (
                if text_prop(object, "NumberType") == Some("Number") {
                    TypeView::number(length, 0)
                } else {
                    TypeView::string(length)
                },
                "Номер",
            )
        }
        "Дата" | "Date" => (
            TypeView {
                date: Some(DateParts::DateTime),
                ..TypeView::scalar("xs:dateTime")
            },
            "Дата",
        ),
        "ПометкаУдаления" | "DeletionMark" => {
            (TypeView::scalar("xs:boolean"), "Пометка удаления")
        }
        "Проведен" | "Posted" => (TypeView::scalar("xs:boolean"), "Проведен"),
        "Родитель" | "Parent" | "Владелец" | "Owner" | "Ссылка" | "Ref" => (
            TypeView {
                types: TypeNamesView::Reference {
                    kind: object.kind,
                    name: &object.name,
                },
                ..TypeView::empty()
            },
            "",
        ),
        "НомерСтроки" | "LineNumber" => (
            TypeView::number(5, 0),
            if matches!(
                object.kind,
                MdKind::InformationRegister | MdKind::AccumulationRegister
            ) {
                "Номер строки"
            } else {
                "N"
            },
        ),
        "Период" | "Period" => {
            let seconds = object.kind == MdKind::AccumulationRegister
                || matches!(
                    text_prop(object, "InformationRegisterPeriodicity"),
                    Some("Second" | "RecorderPosition")
                );
            (
                TypeView {
                    date: Some(if seconds {
                        DateParts::DateTime
                    } else {
                        DateParts::Date
                    }),
                    ..TypeView::scalar("xs:dateTime")
                },
                "Период",
            )
        }
        "Регистратор" | "Recorder" => (TypeView::scalar("cfg:AnyRef"), "Регистратор"),
        _ => return None,
    };
    Some(ResolvedView {
        ty,
        title: (!title.is_empty()).then_some(TitleView::Russian(title)),
    })
}
fn child_resolved(child: &mdconf::Child) -> Option<ResolvedView<'_>> {
    Some(ResolvedView {
        ty: TypeView::declared(child.type_desc()?),
        title: TitleView::declared(&child.synonym),
    })
}
