use roxmltree::Node;

use crate::answer::NS_LF;
use crate::answer::{
    AnswerError, NS_APP, NS_CMI, Reader, Unrecognized, attr, boolean, guid, is, read, text_of,
    unsigned,
};
use crate::formdescr::{FieldType, FormData, FormKind, platform_type};

pub const HIDDEN_CMI: &[&str] = &[
    "defaultNPHandleInfo",
    "urls",
    "panels",
    "commandsWithShortCuts",
];

#[derive(Debug, Clone, Copy)]
pub struct MdAnswers<'a> {
    pub types: &'a [u8],
    pub view: Option<&'a [u8]>,
    pub cmi: &'a [u8],
    pub commands: Option<&'a [u8]>,
    pub panel: Option<&'a [u8]>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    pub types: Vec<MdType>,
    pub objects: Vec<MdObject>,
    pub sections: Vec<MdSection>,
    pub unrecognized: Vec<Unrecognized>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MdKind {
    Enum,
    Catalog,
    Document,
    ExchangePlan,
    ChartOfCharacteristicTypes,
    ChartOfAccounts,
    ChartOfCalculationTypes,
    BusinessProcess,
    Task,
    InformationRegister,
    AccumulationRegister,
    Constant,
    Report,
    DataProcessor,
    DocumentJournal,
    CommonCommand,
    Other(String),
}

const KINDS: &[(MdKind, &str, &str)] = &[
    (MdKind::Enum, "Enum", "Перечисление"),
    (MdKind::Catalog, "Catalog", "Справочник"),
    (MdKind::Document, "Document", "Документ"),
    (MdKind::ExchangePlan, "ExchangePlan", "ПланОбмена"),
    (
        MdKind::ChartOfCharacteristicTypes,
        "ChartOfCharacteristicTypes",
        "ПланВидовХарактеристик",
    ),
    (MdKind::ChartOfAccounts, "ChartOfAccounts", "ПланСчетов"),
    (
        MdKind::ChartOfCalculationTypes,
        "ChartOfCalculationTypes",
        "ПланВидовРасчета",
    ),
    (MdKind::BusinessProcess, "BusinessProcess", "БизнесПроцесс"),
    (MdKind::Task, "Task", "Задача"),
    (
        MdKind::InformationRegister,
        "InformationRegister",
        "РегистрСведений",
    ),
    (
        MdKind::AccumulationRegister,
        "AccumulationRegister",
        "РегистрНакопления",
    ),
    (MdKind::Constant, "Constant", "Константа"),
    (MdKind::Report, "Report", "Отчет"),
    (MdKind::DataProcessor, "DataProcessor", "Обработка"),
    (
        MdKind::DocumentJournal,
        "DocumentJournal",
        "ЖурналДокументов",
    ),
    (MdKind::CommonCommand, "CommonCommand", "ОбщаяКоманда"),
];

impl MdKind {
    pub(crate) fn of_ru(ru: &str) -> Option<MdKind> {
        KINDS
            .iter()
            .find(|(_, _, r)| *r == ru)
            .map(|(k, _, _)| k.clone())
    }

    pub(crate) fn of_stem(stem: &str) -> MdKind {
        KINDS
            .iter()
            .find(|(_, en, _)| *en == stem)
            .map_or_else(|| MdKind::Other(stem.to_string()), |(k, _, _)| k.clone())
    }

    pub fn form_kind(&self) -> Option<FormKind> {
        match self {
            MdKind::Catalog
            | MdKind::Document
            | MdKind::ExchangePlan
            | MdKind::ChartOfCharacteristicTypes
            | MdKind::ChartOfAccounts
            | MdKind::ChartOfCalculationTypes
            | MdKind::BusinessProcess
            | MdKind::Task => Some(FormKind::Object),
            MdKind::InformationRegister => Some(FormKind::Record),
            MdKind::AccumulationRegister => Some(FormKind::RecordSet),
            MdKind::Enum
            | MdKind::Constant
            | MdKind::Report
            | MdKind::DataProcessor
            | MdKind::DocumentJournal
            | MdKind::CommonCommand
            | MdKind::Other(_) => None,
        }
    }

    pub fn object_form(&self) -> Option<FormKind> {
        match self {
            MdKind::Report | MdKind::DataProcessor => Some(FormKind::Object),
            MdKind::DocumentJournal => Some(FormKind::List),
            _ => None,
        }
    }

    pub fn value_type(&self) -> Option<(String, String)> {
        let (en, ru) = match self {
            MdKind::Other(_)
            | MdKind::Report
            | MdKind::DataProcessor
            | MdKind::DocumentJournal
            | MdKind::CommonCommand => return None,
            MdKind::InformationRegister | MdKind::AccumulationRegister => {
                ("RecordKey", "КлючЗаписи")
            }
            MdKind::Constant => ("ValueKey", "КлючЗначения"),
            _ => ("Ref", "Ссылка"),
        };
        Some((format!("{}{en}", self.en()), format!("{}{ru}", self.ru())))
    }

    pub fn en(&self) -> &str {
        match self {
            MdKind::Other(s) => s,
            k => KINDS
                .iter()
                .find(|(x, _, _)| x == k)
                .map_or("", |(_, en, _)| en),
        }
    }

    pub fn ru(&self) -> &str {
        match self {
            MdKind::Other(s) => s,
            k => KINDS
                .iter()
                .find(|(x, _, _)| x == k)
                .map_or("", |(_, _, ru)| ru),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdType {
    pub kind: MdKind,
    pub name: String,
    pub type_id: String,
    pub table: Option<u32>,
    pub view: Option<String>,
    pub has_access: Option<bool>,
    pub values: Vec<EnumValue>,
    pub recorder: Option<String>,
    pub periodic: Option<bool>,
    pub dims: Vec<Dim>,
    pub attributes: Option<MdAttributes>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdAttributes {
    Read {
        form_id: String,
        data: FormData,
    },
    NoCommand,
    Ambiguous {
        uuids: Vec<String>,
    },
    Columns {
        form_id: String,
        columns: Vec<ListColumn>,
    },
    Failed {
        form_id: String,
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListColumn {
    pub name: String,
    pub path: Option<String>,
    pub ty: XmlType,
    pub hidden: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XmlType {
    pub names: Vec<String>,
    pub length: Option<u32>,
    pub digits: Option<u32>,
    pub fraction: Option<u32>,
    pub nonnegative: bool,
    pub date_fractions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdObject {
    pub kind: MdKind,
    pub name: String,
    pub view: Option<String>,
    pub uuids: Vec<String>,
    pub commands: Vec<ObjectCommand>,
    pub attributes: Option<MdAttributes>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectCommand {
    pub section: String,
    pub section_text: Option<String>,
    pub command: Command,
}

fn named_object(c: &Command) -> Option<(MdKind, String, Vec<String>)> {
    let (name, kind_of): (&str, fn(&str) -> Option<MdKind>) = match (&c.name, &c.name_ru) {
        (Some(n), _) => (n, |k| Some(MdKind::of_stem(k))),
        (None, Some(n)) => (n, MdKind::of_ru),
        (None, None) => return None,
    };
    let mut parts = name.split('.');
    let kind = kind_of(parts.next()?)?;
    let object = parts.next().filter(|o| !o.is_empty())?.to_string();
    Some((kind, object, parts.map(str::to_string).collect()))
}

const STANDARD_COMMANDS: &[&str] = &["Open", "OpenList", "Открыть", "ОткрытьСписок"];

fn same_name(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

fn command_uuid(c: &Command) -> Option<String> {
    let uuid = c.id.strip_prefix("0:")?;
    crate::value::Uuid::parse(uuid).ok()?;
    Some(uuid.to_ascii_lowercase())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumValue {
    pub id: String,
    pub name: String,
    pub synonym: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dim {
    pub name: String,
    pub type_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdSection {
    pub id: String,
    pub name: Option<String>,
    pub text: Option<String>,
    pub include: Option<bool>,
    pub commands: Vec<Command>,
    pub children: Vec<MdSection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub id: String,
    pub name: Option<String>,
    pub name_ru: Option<String>,
    pub md: Option<String>,
    pub url: Option<String>,
    pub text: Option<String>,
    pub group: Option<String>,
    pub category: Option<String>,
}

impl Command {
    pub fn in_panel(&self) -> bool {
        self.category.as_deref() == Some("panel")
    }
}

impl Metadata {
    pub fn md_uuids(&self) -> Vec<(usize, Vec<String>)> {
        fn walk<'m>(s: &'m [MdSection], cmds: &mut Vec<&'m Command>) {
            for x in s {
                cmds.extend(x.commands.iter());
                walk(&x.children, cmds);
            }
        }
        let mut cmds = Vec::new();
        walk(&self.sections, &mut cmds);
        let mut out: Vec<(usize, Vec<String>)> = Vec::new();
        for c in cmds {
            if c.in_panel() {
                continue;
            }
            let (Some(name), Some(uuid)) = (c.name.as_deref(), c.id.strip_prefix("0:")) else {
                continue;
            };
            if crate::value::Uuid::parse(uuid).is_err() {
                continue;
            }
            let parts: Vec<&str> = name.split('.').collect();
            let [kind, object, _] = parts.as_slice() else {
                continue;
            };
            let Some(i) = self
                .types
                .iter()
                .position(|t| t.kind.en() == *kind && t.name == *object)
            else {
                continue;
            };
            let uuid = uuid.to_ascii_lowercase();
            match out.iter_mut().find(|(j, _)| *j == i) {
                Some((_, all)) if !all.contains(&uuid) => all.push(uuid),
                Some(_) => {}
                None => out.push((i, vec![uuid])),
            }
        }
        out.sort_by_key(|(i, _)| *i);
        out
    }

    fn objects_of(types: &[MdType], sections: &[MdSection]) -> Vec<MdObject> {
        fn walk<'m>(s: &'m [MdSection], out: &mut Vec<(&'m MdSection, &'m Command)>) {
            for x in s {
                out.extend(x.commands.iter().map(|c| (x, c)));
                walk(&x.children, out);
            }
        }
        let mut cmds = Vec::new();
        walk(sections, &mut cmds);
        let mut out: Vec<MdObject> = Vec::new();
        for (section, c) in cmds {
            let Some((kind, name, rest)) = named_object(c) else {
                continue;
            };
            if types
                .iter()
                .any(|t| t.kind.en() == kind.en() && same_name(&t.name, &name))
            {
                continue;
            }
            let standard = match rest.as_slice() {
                [] => kind == MdKind::CommonCommand,
                [command] => STANDARD_COMMANDS.contains(&command.as_str()),
                _ => false,
            };
            let i = match out
                .iter()
                .position(|o| o.kind == kind && same_name(&o.name, &name))
            {
                Some(i) => i,
                None => {
                    out.push(MdObject {
                        kind,
                        name,
                        view: None,
                        uuids: Vec::new(),
                        commands: Vec::new(),
                        attributes: None,
                    });
                    out.len() - 1
                }
            };
            let o = &mut out[i];
            if standard {
                if o.view.is_none() {
                    o.view.clone_from(&c.text);
                }
                if let Some(u) = command_uuid(c)
                    && !o.uuids.contains(&u)
                {
                    o.uuids.push(u);
                }
            }
            o.commands.push(ObjectCommand {
                section: section.id.clone(),
                section_text: section.text.clone(),
                command: c.clone(),
            });
        }
        out
    }

    pub fn xml_type_text(&self, ty: &XmlType) -> String {
        if ty.names.is_empty() {
            return "Произвольный".into();
        }
        ty.names
            .iter()
            .map(|n| {
                let (name, q): (String, Vec<String>) = match n.as_str() {
                    "xs:string" => (
                        "Строка".into(),
                        ty.length
                            .filter(|l| *l > 0)
                            .map(|l| l.to_string())
                            .into_iter()
                            .collect(),
                    ),
                    "xs:decimal" => (
                        "Число".into(),
                        number_qualifiers(&FieldType {
                            digits: ty.digits,
                            fraction: ty.fraction,
                            nonnegative: ty.nonnegative,
                            ..FieldType::default()
                        }),
                    ),
                    "xs:boolean" => ("Булево".into(), Vec::new()),
                    "xs:dateTime" => (
                        "Дата".into(),
                        ty.date_fractions
                            .clone()
                            .filter(|f| f != "DateTime")
                            .into_iter()
                            .collect(),
                    ),
                    "v8:Type" => ("Тип".into(), Vec::new()),
                    "v8:UUID" => ("УникальныйИдентификатор".into(), Vec::new()),
                    other => (
                        xml_ref_text(other).unwrap_or_else(|| other.to_string()),
                        Vec::new(),
                    ),
                };
                if q.is_empty() {
                    name
                } else {
                    format!("{name}({})", q.join(", "))
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn type_names(&self, id: &str) -> Option<(String, String)> {
        if let Some((en, ru)) = platform_type(id) {
            return Some((en.to_string(), ru.to_string()));
        }
        let t = self
            .types
            .iter()
            .find(|t| t.type_id.eq_ignore_ascii_case(id))?;
        let (en, ru) = t.kind.value_type()?;
        Some((format!("{en}.{}", t.name), format!("{ru}.{}", t.name)))
    }

    pub fn type_text(&self, ty: &FieldType) -> String {
        if ty.ids.is_empty() {
            return "Произвольный".into();
        }
        ty.ids
            .iter()
            .map(|id| {
                let name = self.type_names(id).map_or_else(|| id.clone(), |(_, ru)| ru);
                let q = match platform_type(id).map(|(en, _)| en) {
                    Some("String") => ty
                        .length
                        .filter(|n| *n > 0)
                        .map(|n| n.to_string())
                        .into_iter()
                        .collect(),
                    Some("Number") => number_qualifiers(ty),
                    Some("Date") => ty.date_fraction.clone().into_iter().collect(),
                    _ => Vec::new(),
                };
                if q.is_empty() {
                    name
                } else {
                    format!("{name}({})", q.join(", "))
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn from_answers(a: &MdAnswers, root: Option<&str>) -> Result<Metadata, AnswerError> {
        let mut unrecognized = Vec::new();
        let mut types = read("types", a.types, NS_APP, &[], &mut unrecognized, read_types)?;
        if let Some(body) = a.view {
            let views = read("types/view", body, NS_APP, &[], &mut unrecognized, views)?;
            for t in &mut types {
                t.view = views
                    .iter()
                    .find(|(id, _)| *id == t.type_id)
                    .and_then(|(_, v)| v.clone());
            }
        }
        let mut sections = read(
            "cmiStartData",
            a.cmi,
            NS_CMI,
            HIDDEN_CMI,
            &mut unrecognized,
            tree,
        )?;
        let answers = [
            ("cmi/commands", a.commands),
            ("cmi/commands/panel", a.panel),
        ];
        for (answer, body) in answers {
            let Some(body) = body else {
                continue;
            };
            let found = read(answer, body, NS_CMI, &[], &mut unrecognized, |r| {
                commands(r, root)
            })?;
            for f in found {
                if let Some(s) = find(&mut sections, &f.id) {
                    s.commands.extend(f.commands);
                    continue;
                }
                match f.parent.as_deref().and_then(|p| find(&mut sections, p)) {
                    Some(parent) => parent.children.push(MdSection {
                        id: f.id,
                        name: f.name,
                        text: f.text,
                        include: f.include,
                        commands: f.commands,
                        children: Vec::new(),
                    }),
                    None => unrecognized.push(Unrecognized {
                        answer,
                        name: format!("section {}", f.id),
                    }),
                }
            }
        }
        let objects = Metadata::objects_of(&types, &sections);
        Ok(Metadata {
            types,
            objects,
            sections,
            unrecognized,
        })
    }
}

fn xml_ref_text(name: &str) -> Option<String> {
    let (stem, object) = name.strip_prefix("cfg:")?.split_once('.')?;
    let (_, ru) = MdKind::of_stem(stem.strip_suffix("Ref")?).value_type()?;
    if !ru.ends_with("Ссылка") {
        return None;
    }
    Some(format!("{ru}.{object}"))
}

pub fn list_columns(body: &[u8]) -> Result<Vec<ListColumn>, AnswerError> {
    read(
        "logForm/elements",
        body,
        crate::answer::NS_WS,
        &[],
        &mut Vec::new(),
        |r| {
            let tables: Vec<Node> = r
                .root
                .descendants()
                .filter(|n| is(*n, NS_LF, "table"))
                .collect();
            let Some(table) = tables
                .iter()
                .find(|t| t.attribute("defItem") == Some("true"))
                .or(tables.first())
            else {
                return Err(r.missing("table"));
            };
            let mut out = Vec::new();
            for n in crate::dlist::table_fields(*table) {
                let Some(path) = lf_text(n, "propName") else {
                    continue;
                };
                out.push(ListColumn {
                    name: attr(n, "name").ok_or_else(|| r.missing("field name"))?,
                    path: Some(path),
                    ty: n
                        .children()
                        .find(|c| is(*c, NS_LF, "tdp"))
                        .map(|t| xml_type(r, t))
                        .transpose()?
                        .unwrap_or_default(),
                    hidden: r.attr_parsed(n, "visible", "field visible", boolean)? == Some(false),
                });
            }
            Ok(out)
        },
    )
}

const NS_V8: &str = "http://v8.1c.ru/8.1/data/core";

fn lf_text(n: Node, local: &str) -> Option<String> {
    n.children()
        .find(|c| is(*c, NS_LF, local))
        .and_then(text_of)
}

fn xml_type(r: &Reader, t: Node) -> Result<XmlType, AnswerError> {
    let v8 = |n: Node, local: &str| -> Option<String> {
        n.descendants()
            .find(|c| is(*c, NS_V8, local))
            .and_then(text_of)
    };
    let num = |local: &str, field: &str| -> Result<Option<u32>, AnswerError> {
        match v8(t, local) {
            None => Ok(None),
            Some(s) => unsigned(&s).map(Some).ok_or_else(|| r.bad(field, &s)),
        }
    };
    Ok(XmlType {
        names: t
            .children()
            .filter(|c| is(*c, NS_V8, "Type") || is(*c, NS_V8, "TypeSet"))
            .filter_map(text_of)
            .collect(),
        length: num("Length", "tdp Length")?,
        digits: num("Digits", "tdp Digits")?,
        fraction: num("FractionDigits", "tdp FractionDigits")?,
        nonnegative: v8(t, "AllowedSign").as_deref() == Some("Nonnegative"),
        date_fractions: v8(t, "DateFractions"),
    })
}

fn number_qualifiers(ty: &FieldType) -> Vec<String> {
    let mut q = Vec::new();
    if ty.digits.unwrap_or(0) > 0 || ty.fraction.unwrap_or(0) > 0 {
        q.push(ty.digits.unwrap_or(0).to_string());
        if let Some(f) = ty.fraction.filter(|f| *f > 0) {
            q.push(f.to_string());
        }
    }
    if ty.nonnegative {
        q.push("неотрицательное".into());
    }
    q
}

pub(crate) fn read_types(r: &Reader) -> Result<Vec<MdType>, AnswerError> {
    r.children(NS_APP, "typeSet");
    let mut out = Vec::new();
    let groups = ["enums", "refs", "regs", "infoRegs", "constValueKeys"];
    for g in r.children_of(NS_APP, &groups) {
        let kind = match g.tag_name().name() {
            "enums" => MdKind::Enum,
            "infoRegs" => MdKind::InformationRegister,
            "constValueKeys" => MdKind::Constant,
            "refs" => {
                let name = attr(g, "name").ok_or_else(|| r.missing("refs name"))?;
                let stem = name.strip_prefix("cfg:").unwrap_or(&name);
                MdKind::of_stem(stem.strip_suffix("Ref").unwrap_or(stem))
            }
            local => match attr(g, "en") {
                Some(en) => MdKind::of_stem(en.strip_suffix("RecordKey").unwrap_or(&en)),
                None => MdKind::Other(local.to_string()),
            },
        };
        for t in g.children().filter(|n| is(*n, NS_APP, "type")) {
            out.push(md_type(r, t, kind.clone())?);
        }
    }
    Ok(out)
}

fn md_type(r: &Reader, t: Node, kind: MdKind) -> Result<MdType, AnswerError> {
    let type_id = r
        .attr_parsed(t, "clsid", "type clsid", guid)?
        .ok_or_else(|| r.missing("type clsid"))?;
    let name = attr(t, "code").ok_or_else(|| r.missing("type code"))?;
    let mut values = Vec::new();
    let mut dims = Vec::new();
    for c in t.children().filter(Node::is_element) {
        if is(c, NS_APP, "val") {
            values.push(EnumValue {
                id: r
                    .attr_parsed(c, "uuid", "val uuid", guid)?
                    .ok_or_else(|| r.missing("val uuid"))?,
                name: attr(c, "code").ok_or_else(|| r.missing("val code"))?,
                synonym: None,
            });
        } else if is(c, NS_APP, "dim") {
            dims.push(Dim {
                name: attr(c, "name").ok_or_else(|| r.missing("dim name"))?,
                type_name: attr(c, "type"),
            });
        }
    }
    Ok(MdType {
        kind,
        name,
        type_id,
        table: r.attr_parsed(t, "table", "type table", unsigned)?,
        view: None,
        has_access: r.attr_parsed(t, "hasAccess", "type hasAccess", boolean)?,
        values,
        recorder: attr(t, "recorder"),
        periodic: r.attr_parsed(t, "period", "type period", boolean)?,
        dims,
        attributes: None,
    })
}

fn views(r: &Reader) -> Result<Vec<(String, Option<String>)>, AnswerError> {
    let mut out = Vec::new();
    for t in r.children(NS_APP, "type") {
        let id = r
            .attr_parsed(t, "clsid", "type clsid", guid)?
            .ok_or_else(|| r.missing("type clsid"))?;
        out.push((id, attr(t, "view")));
    }
    Ok(out)
}

fn tree(r: &Reader) -> Result<Vec<MdSection>, AnswerError> {
    match r.one(NS_CMI, "toplevelSubsystems")? {
        Some(top) => sections_of(r, top),
        None => Ok(Vec::new()),
    }
}

fn sections_of(r: &Reader, parent: Node) -> Result<Vec<MdSection>, AnswerError> {
    let mut out = Vec::new();
    for n in parent.children().filter(|n| is(*n, NS_CMI, "section")) {
        out.push(MdSection {
            id: section_id(r, n)?,
            name: attr(n, "name"),
            text: child_text(n, "text"),
            include: r.attr_parsed(n, "include", "section include", boolean)?,
            commands: Vec::new(),
            children: sections_of(r, n)?,
        });
    }
    Ok(out)
}

fn section_id(r: &Reader, n: Node) -> Result<String, AnswerError> {
    r.attr_parsed(n, "id", "section id", guid)?
        .ok_or_else(|| r.missing("section id"))
}

fn child_text(n: Node, local: &str) -> Option<String> {
    n.children()
        .find(|c| is(*c, NS_CMI, local))
        .and_then(text_of)
}

struct FoundSection {
    id: String,
    parent: Option<String>,
    name: Option<String>,
    text: Option<String>,
    include: Option<bool>,
    commands: Vec<Command>,
}

fn commands(r: &Reader, root: Option<&str>) -> Result<Vec<FoundSection>, AnswerError> {
    let mut out = Vec::new();
    let top = r.children_of(NS_CMI, &["section", "group"]);
    let groups: Vec<Node> = top
        .iter()
        .copied()
        .filter(|n| n.tag_name().name() == "group")
        .collect();
    if !groups.is_empty() {
        let id = root.ok_or_else(|| r.missing("fragment of the root groups"))?;
        out.push(FoundSection {
            id: id.to_string(),
            parent: None,
            name: None,
            text: None,
            include: None,
            commands: group_commands(r, &groups)?,
        });
    }
    for s in top.iter().filter(|n| n.tag_name().name() == "section") {
        section_commands(r, *s, root.map(str::to_string), &mut out)?;
    }
    Ok(out)
}

fn section_commands(
    r: &Reader,
    s: Node,
    parent: Option<String>,
    out: &mut Vec<FoundSection>,
) -> Result<(), AnswerError> {
    let id = section_id(r, s)?;
    let groups: Vec<Node> = s.children().filter(|n| is(*n, NS_CMI, "group")).collect();
    out.push(FoundSection {
        id: id.clone(),
        parent,
        name: attr(s, "name"),
        text: child_text(s, "text"),
        include: r.attr_parsed(s, "include", "section include", boolean)?,
        commands: group_commands(r, &groups)?,
    });
    for c in s.children().filter(|n| is(*n, NS_CMI, "section")) {
        section_commands(r, c, Some(id.clone()), out)?;
    }
    Ok(())
}

fn group_commands(r: &Reader, groups: &[Node]) -> Result<Vec<Command>, AnswerError> {
    let mut out = Vec::new();
    for g in groups {
        let group = child_text(*g, "text");
        let category = attr(*g, "cat");
        for c in g.children().filter(|n| is(*n, NS_CMI, "command")) {
            out.push(Command {
                id: attr(c, "id").ok_or_else(|| r.missing("command id"))?,
                name: attr(c, "nameInt"),
                name_ru: attr(c, "nameRus"),
                md: attr(c, "md"),
                url: attr(c, "url"),
                text: child_text(c, "text"),
                group: group.clone(),
                category: category.clone(),
            });
        }
    }
    Ok(out)
}

fn find<'s>(sections: &'s mut [MdSection], id: &str) -> Option<&'s mut MdSection> {
    for s in sections {
        if s.id == id {
            return Some(s);
        }
        if let Some(found) = find(&mut s.children, id) {
            return Some(found);
        }
    }
    None
}
