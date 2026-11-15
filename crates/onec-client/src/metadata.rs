use roxmltree::Node;

use crate::answer::{
    AnswerError, NS_APP, NS_CMI, Reader, Unrecognized, attr, boolean, guid, is, read, text_of,
    unsigned,
};

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
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    pub types: Vec<MdType>,
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
];

impl MdKind {
    fn of_stem(stem: &str) -> MdKind {
        KINDS
            .iter()
            .find(|(_, en, _)| *en == stem)
            .map_or_else(|| MdKind::Other(stem.to_string()), |(k, _, _)| k.clone())
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
}

impl Metadata {
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
        if let Some(body) = a.commands {
            let found = read("cmi/commands", body, NS_CMI, &[], &mut unrecognized, |r| {
                commands(r, root)
            })?;
            for (id, cmds) in found {
                match find(&mut sections, &id) {
                    Some(s) => s.commands.extend(cmds),
                    None => unrecognized.push(Unrecognized {
                        answer: "cmi/commands",
                        name: format!("section {id}"),
                    }),
                }
            }
        }
        Ok(Metadata {
            types,
            sections,
            unrecognized,
        })
    }
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

fn commands(r: &Reader, root: Option<&str>) -> Result<Vec<(String, Vec<Command>)>, AnswerError> {
    let mut out = Vec::new();
    let top = r.children_of(NS_CMI, &["section", "group"]);
    let groups: Vec<Node> = top
        .iter()
        .copied()
        .filter(|n| n.tag_name().name() == "group")
        .collect();
    if !groups.is_empty() {
        let id = root.ok_or_else(|| r.missing("fragment of the root groups"))?;
        out.push((id.to_string(), group_commands(r, &groups)?));
    }
    for s in top.iter().filter(|n| n.tag_name().name() == "section") {
        section_commands(r, *s, &mut out)?;
    }
    Ok(out)
}

fn section_commands(
    r: &Reader,
    s: Node,
    out: &mut Vec<(String, Vec<Command>)>,
) -> Result<(), AnswerError> {
    let id = section_id(r, s)?;
    let groups: Vec<Node> = s.children().filter(|n| is(*n, NS_CMI, "group")).collect();
    out.push((id, group_commands(r, &groups)?));
    for c in s.children().filter(|n| is(*n, NS_CMI, "section")) {
        section_commands(r, c, out)?;
    }
    Ok(())
}

fn group_commands(r: &Reader, groups: &[Node]) -> Result<Vec<Command>, AnswerError> {
    let mut out = Vec::new();
    for g in groups {
        let group = child_text(*g, "text");
        for c in g.children().filter(|n| is(*n, NS_CMI, "command")) {
            out.push(Command {
                id: attr(c, "id").ok_or_else(|| r.missing("command id"))?,
                name: attr(c, "nameInt"),
                name_ru: attr(c, "nameRus"),
                md: attr(c, "md"),
                url: attr(c, "url"),
                text: child_text(c, "text"),
                group: group.clone(),
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
