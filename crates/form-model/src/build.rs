use mdconf::{Configuration, DateFractions, MdKind, MdObject, Value};

use crate::{
    DateParts, FormDesc, LStr, ModelError, Resolved, TypeDesc, TypeResolver, parse_form_xml,
};

pub type BuiltForm = (FormDesc, Vec<(String, Vec<u8>)>);

pub fn from_configuration(
    cfg: &Configuration,
    id: brace::Guid,
) -> Result<Option<BuiltForm>, ModelError> {
    let Some((text, pictures, _deferred)) = mdconf::forms::form_xml(cfg, id) else {
        return Ok(None);
    };
    Ok(Some((
        parse_form_xml(text.trim_start_matches('\u{feff}'))?,
        pictures,
    )))
}

pub fn type_desc(t: &mdconf::TypeDescription) -> TypeDesc {
    TypeDesc {
        types: t.types.iter().map(|r| r.name.to_string()).collect(),
        string_len: t.string.map(|s| s.length),
        number: t.number.map(|n| (n.digits, n.fraction)),
        date: t.date.map(|d| match d {
            DateFractions::Date => DateParts::Date,
            DateFractions::Time => DateParts::Time,
            DateFractions::DateTime => DateParts::DateTime,
        }),
    }
}

fn lstr(s: &mdconf::LocalString) -> Option<LStr> {
    (!s.0.is_empty()).then(|| LStr(s.0.clone()))
}

pub struct MdResolver<'a> {
    pub cfg: &'a Configuration,
}

const SUFFIXES: [&str; 4] = ["RecordManager", "RecordSet", "Object", "Ref"];

fn owner_object<'a>(cfg: &'a Configuration, owner: &str) -> Option<&'a MdObject> {
    let o = owner.strip_prefix("cfg:").unwrap_or(owner);
    let (head, name) = o.split_once('.')?;
    let kind = |k: &str| MdKind::all().iter().copied().find(|x| x.as_str() == k);
    let k = SUFFIXES
        .iter()
        .find_map(|s| head.strip_suffix(s).and_then(kind))
        .or_else(|| kind(head))?;
    cfg.find(k, name)
}

fn plain(t: &str) -> Resolved {
    Resolved {
        ty: TypeDesc {
            types: vec![t.to_string()],
            ..Default::default()
        },
        title: None,
    }
}

fn string(len: u32) -> TypeDesc {
    TypeDesc {
        types: vec!["xs:string".into()],
        string_len: Some(len),
        ..Default::default()
    }
}

fn number(digits: u32, fraction: u32) -> TypeDesc {
    TypeDesc {
        types: vec!["xs:decimal".into()],
        number: Some((digits, fraction)),
        ..Default::default()
    }
}

fn int_prop(o: &MdObject, name: &str) -> Option<u32> {
    match o.prop(name)? {
        Value::Text(t) => t.trim().parse().ok(),
        _ => None,
    }
}

fn text_prop<'a>(o: &'a MdObject, name: &str) -> Option<&'a str> {
    match o.prop(name)? {
        Value::Text(t) => Some(t),
        _ => None,
    }
}

fn standard(o: &MdObject, name: &str) -> Option<Resolved> {
    let (ty, title) = match name {
        "Код" | "Code" => {
            let len = int_prop(o, "CodeLength").unwrap_or(9);
            let ty = if text_prop(o, "CodeType") == Some("Number") {
                number(len, 0)
            } else {
                string(len)
            };
            (ty, "Код")
        }
        "Наименование" | "Description" => (
            string(int_prop(o, "DescriptionLength").unwrap_or(25)),
            "Наименование",
        ),
        "Номер" | "Number" => {
            let len = int_prop(o, "NumberLength").unwrap_or(9);
            let ty = if text_prop(o, "NumberType") == Some("Number") {
                number(len, 0)
            } else {
                string(len)
            };
            (ty, "Номер")
        }
        "Дата" | "Date" => (
            TypeDesc {
                types: vec!["xs:dateTime".into()],
                date: Some(DateParts::DateTime),
                ..Default::default()
            },
            "Дата",
        ),
        "ПометкаУдаления" | "DeletionMark" => {
            (plain("xs:boolean").ty, "Пометка удаления")
        }
        "Проведен" | "Posted" => (plain("xs:boolean").ty, "Проведен"),
        "Родитель" | "Parent" | "Владелец" | "Owner" | "Ссылка" | "Ref" => {
            let ty = TypeDesc {
                types: vec![format!("cfg:{}Ref.{}", o.kind.as_str(), o.name)],
                ..Default::default()
            };
            (ty, "")
        }
        "НомерСтроки" | "LineNumber" => {
            let register = matches!(
                o.kind,
                MdKind::InformationRegister | MdKind::AccumulationRegister
            );
            (
                number(5, 0),
                if register {
                    "Номер строки"
                } else {
                    "N"
                },
            )
        }
        "Период" | "Period" => {
            let seconds = o.kind == MdKind::AccumulationRegister
                || matches!(
                    text_prop(o, "InformationRegisterPeriodicity"),
                    Some("Second" | "RecorderPosition")
                );
            let part = if seconds {
                DateParts::DateTime
            } else {
                DateParts::Date
            };
            let ty = TypeDesc {
                types: vec!["xs:dateTime".into()],
                date: Some(part),
                ..Default::default()
            };
            (ty, "Период")
        }
        "Регистратор" | "Recorder" => (plain("cfg:AnyRef").ty, "Регистратор"),
        _ => return None,
    };
    Some(Resolved {
        ty,
        title: (!title.is_empty()).then(|| LStr(vec![("ru".into(), title.into())])),
    })
}

fn child_resolved(c: &mdconf::Child) -> Option<Resolved> {
    Some(Resolved {
        ty: type_desc(c.type_desc()?),
        title: lstr(&c.synonym),
    })
}

impl TypeResolver for MdResolver<'_> {
    fn resolve(&self, owner: &str, path: &[&str]) -> Option<Resolved> {
        if owner.strip_prefix("cfg:").unwrap_or(owner) == "ConstantsSet" {
            let [name] = path else { return None };
            let c = self.cfg.find(MdKind::Constant, name)?;
            let ty = match c.prop("Type") {
                Some(Value::Type(t)) => type_desc(t),
                _ => TypeDesc::default(),
            };
            return Some(Resolved {
                ty,
                title: lstr(&c.synonym),
            });
        }
        let o = owner_object(self.cfg, owner)?;
        let (first, rest) = path.split_first()?;
        let fields = || o.attributes().chain(o.dimensions()).chain(o.resources());
        if rest.is_empty() {
            if let Some(c) = fields().find(|c| c.name == *first) {
                return child_resolved(c);
            }
            return standard(o, first);
        }
        let ts = o.tabular_sections().find(|c| c.name == *first)?;
        let [col] = rest else { return None };
        match ts.children.iter().find(|c| c.name == *col) {
            Some(c) => child_resolved(c),
            None => standard(o, col).filter(|_| matches!(*col, "НомерСтроки" | "LineNumber")),
        }
    }
}
