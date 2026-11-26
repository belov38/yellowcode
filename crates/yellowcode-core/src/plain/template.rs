use std::collections::{HashMap, HashSet};
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use mdconf::templates::{Template, TemplateType};
use mdconf::{Configuration, MdKind, SubKind};
use serde::Serialize;
use serde_json::json;

use super::metadata::Format;
use super::module::write_json;
use super::scope::{Scope, ScopeArgs};
use crate::viewer::names::{fold_path_name, kind_from_name, sub_kind_en};
use crate::viewer::path::{self, Target};
use crate::viewer::source;
use crate::viewer::template_view::{
    DcsFilterItem, DcsOrderItem, DcsSelectedItem, DcsSet, DcsSettings, DcsValue, TemplateView,
    view_of,
};

const UNKNOWN_KIND: &str = "Unknown";

pub struct TemplateRef<'a> {
    pub path: String,
    pub kind: Option<TemplateType>,
    pub template: &'a Template,
}

fn template_ref<'a>(path: String, template: &'a Template) -> TemplateRef<'a> {
    TemplateRef {
        path,
        kind: template.kind,
        template,
    }
}

pub fn templates<'a>(cfg: &'a Configuration, scope: &Scope) -> Vec<TemplateRef<'a>> {
    let mut out = Vec::new();
    let mut add = |path: String, template: &'a Template| {
        if scope.contains(&path) {
            out.push(template_ref(path, template));
        }
    };
    for o in &cfg.objects {
        let path = format!("{}.{}", o.kind.as_str(), o.name);
        if o.kind == MdKind::CommonTemplate
            && let Some(t) = &o.template
        {
            add(path.clone(), t);
        }
        for s in o.templates.iter().filter(|s| s.kind == SubKind::Template) {
            if let Some(t) = &s.template {
                add(format!("{path}.{}.{}", sub_kind_en(s.kind), s.name), t);
            }
        }
    }
    out
}

pub fn resolve_template<'a>(cfg: &'a Configuration, path: &str) -> crate::Result<TemplateRef<'a>> {
    let target = path::resolve(cfg, path)?;
    let template = match &target {
        Target::Object(o) if o.kind == MdKind::CommonTemplate => o.template.as_ref(),
        Target::Sub { sub, .. } if sub.kind == SubKind::Template => sub.template.as_ref(),
        _ => {
            return Err(format!("{} is not a template", path::canonical(cfg, &target)).into());
        }
    };
    let canonical = path::canonical(cfg, &target);
    match template {
        Some(t) => Ok(template_ref(canonical, t)),
        None => Err(format!("{canonical} has no template data").into()),
    }
}

/// A `--type` filter of `template list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum TypeFilter {
    Dcs,
    Sheet,
    Text,
    Html,
    Binary,
    /// Every kind that the other filters do not name, and a template of an unknown kind.
    Other,
}

impl TypeFilter {
    pub fn matches(&self, kind: Option<TemplateType>) -> bool {
        use TemplateType::{
            BinaryData, DataCompositionSchema, HTMLDocument, SpreadsheetDocument, TextDocument,
        };
        match self {
            TypeFilter::Dcs => kind == Some(DataCompositionSchema),
            TypeFilter::Sheet => kind == Some(SpreadsheetDocument),
            TypeFilter::Text => kind == Some(TextDocument),
            TypeFilter::Html => kind == Some(HTMLDocument),
            TypeFilter::Binary => kind == Some(BinaryData),
            TypeFilter::Other => !matches!(
                kind,
                Some(
                    DataCompositionSchema
                        | SpreadsheetDocument
                        | TextDocument
                        | HTMLDocument
                        | BinaryData
                )
            ),
        }
    }
}

pub enum Command {
    List {
        source: PathBuf,
        scope: ScopeArgs,
        types: Vec<TypeFilter>,
        format: Format,
    },
    Show {
        source: PathBuf,
        template: String,
        query_of: Option<String>,
        format: Format,
    },
}

fn kind_name(kind: Option<TemplateType>) -> &'static str {
    kind.map_or(UNKNOWN_KIND, TemplateType::name)
}

pub fn query_sets(view: &TemplateView) -> Vec<(&str, &str)> {
    fn walk<'a>(sets: &'a [DcsSet], out: &mut Vec<(&'a str, &'a str)>) {
        for s in sets {
            if let Some(q) = s.query.as_deref().filter(|q| !q.is_empty()) {
                out.push((&s.name, q));
            }
            walk(&s.items, out);
        }
    }
    let mut out = Vec::new();
    if let TemplateView::Dcs { sets, .. } = view {
        walk(sets, &mut out);
    }
    out
}

fn put(out: &mut dyn Write, indent: usize, line: &str) -> crate::Result<()> {
    writeln!(out, "{:indent$}{line}", "")?;
    Ok(())
}

fn data_set_text(s: &DcsSet, union: Option<&str>, out: &mut dyn Write) -> crate::Result<()> {
    let mut head = s.kind.clone();
    if !s.data_source.is_empty() {
        head.push_str(&format!(", источник {}", s.data_source));
    }
    if let Some(o) = s.object_name.as_deref().filter(|o| !o.is_empty()) {
        head.push_str(&format!(", объект {o}"));
    }
    if let Some(u) = union {
        head.push_str(&format!(", в наборе {u}"));
    }
    put(out, 0, &format!("{} ({head})", s.name))?;
    if !s.fields.is_empty() {
        let fields: Vec<String> = s
            .fields
            .iter()
            .map(|f| {
                if f.title.is_empty() {
                    f.path.clone()
                } else {
                    format!("{} ({})", f.path, f.title)
                }
            })
            .collect();
        put(out, 2, &format!("Поля: {}", fields.join(", ")))?;
    }
    if let Some(q) = s.query.as_deref().filter(|q| !q.is_empty()) {
        put(out, 2, "Запрос:")?;
        for line in q.lines() {
            put(out, 4, line)?;
        }
    }
    for item in &s.items {
        data_set_text(item, Some(&s.name), out)?;
    }
    Ok(())
}

fn value_text(v: &Option<DcsValue>) -> String {
    match v {
        None => String::new(),
        Some(v) if v.text.is_empty() => v
            .items
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        Some(v) => v.text.clone(),
    }
}

fn selected_text(i: &DcsSelectedItem) -> String {
    if !i.field.is_empty() {
        i.field.clone()
    } else if i.kind.ends_with("Auto") {
        "Авто".into()
    } else if !i.items.is_empty() {
        format!("({})", enabled(&i.items, |i| i.r#use, selected_text))
    } else {
        i.kind.clone()
    }
}

fn filter_text(i: &DcsFilterItem) -> String {
    if i.items.is_empty() && i.group_type.is_empty() {
        [
            value_text(&i.left),
            i.comparison_type.clone(),
            value_text(&i.right),
        ]
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
    } else {
        format!(
            "{}({})",
            i.group_type,
            enabled(&i.items, |i| i.r#use, filter_text)
        )
    }
}

fn order_text(i: &DcsOrderItem) -> String {
    let dir = match i.order_type.as_str() {
        "Asc" => "Возр",
        "Desc" => "Убыв",
        other => other,
    };
    if i.field.is_empty() {
        i.kind.clone()
    } else {
        format!("{} {dir}", i.field)
    }
}

fn enabled<T>(
    items: &[T],
    in_use: impl Fn(&T) -> Option<bool>,
    text: impl Fn(&T) -> String,
) -> String {
    items
        .iter()
        .filter(|i| in_use(i) != Some(false))
        .map(text)
        .collect::<Vec<_>>()
        .join(", ")
}

fn settings_clauses(s: &DcsSettings) -> Vec<String> {
    let mut parts = Vec::new();
    for (label, text) in [
        ("выбор", enabled(&s.selection, |i| i.r#use, selected_text)),
        ("отбор", enabled(&s.filter, |i| i.r#use, filter_text)),
        ("порядок", enabled(&s.order, |i| i.r#use, order_text)),
    ] {
        if !text.is_empty() {
            parts.push(format!("{label} {text}"));
        }
    }
    parts
}

fn dcs_text(view: &TemplateView, out: &mut dyn Write) -> crate::Result<()> {
    let TemplateView::Dcs {
        sets,
        links,
        resources,
        parameters,
        variants,
        calculated_fields,
        ..
    } = view
    else {
        return Ok(());
    };
    if !sets.is_empty() {
        put(out, 0, "[Наборы данных]")?;
        for s in sets {
            data_set_text(s, None, out)?;
        }
    }
    if !links.is_empty() {
        put(out, 0, "[Связи]")?;
        for l in links {
            let mut line = format!(
                "{} -> {}: {} = {}",
                l.source, l.destination, l.source_expression, l.destination_expression
            );
            if !l.parameter.is_empty() {
                line.push_str(&format!(" (параметр {})", l.parameter));
            }
            put(out, 2, &line)?;
        }
    }
    if !resources.is_empty() {
        put(out, 0, "[Ресурсы]")?;
        for r in resources {
            let mut line = format!("{} = {}", r.path, r.expression);
            if !r.groups.is_empty() {
                line.push_str(&format!(" по {}", r.groups.join(", ")));
            }
            put(out, 2, &line)?;
        }
    }
    if !calculated_fields.is_empty() {
        put(out, 0, "[Вычисляемые поля]")?;
        for c in calculated_fields {
            let mut line = format!("{} = {}", c.path, c.expression);
            if !c.title.is_empty() {
                line.push_str(&format!(" ({})", c.title));
            }
            put(out, 2, &line)?;
        }
    }
    if !parameters.is_empty() {
        put(out, 0, "[Параметры]")?;
        for p in parameters {
            let mut line = format!("{}: {}", p.name, p.value_types.join(", "));
            if !p.title.is_empty() {
                line.push_str(&format!(" ({})", p.title));
            }
            if let Some(v) = p.value.as_deref().filter(|v| !v.is_empty()) {
                line.push_str(&format!(" = {v}"));
            }
            if !p.expression.is_empty() {
                line.push_str(&format!("; выражение {}", p.expression));
            }
            put(out, 2, &line)?;
        }
    }
    if !variants.is_empty() {
        put(out, 0, "[Варианты]")?;
        for v in variants {
            let mut line = if v.presentation.is_empty() {
                v.name.clone()
            } else {
                format!("{} ({})", v.name, v.presentation)
            };
            let clauses = settings_clauses(&v.settings);
            if !clauses.is_empty() {
                line.push_str(": ");
                line.push_str(&clauses.join("; "));
            }
            put(out, 2, &line)?;
        }
    }
    Ok(())
}

fn sheet_text(view: &TemplateView, out: &mut dyn Write) -> crate::Result<()> {
    let TemplateView::Sheet {
        rows, first_row, ..
    } = view
    else {
        return Ok(());
    };
    for (row, cells) in (first_row + 1..).zip(rows) {
        for c in cells {
            write!(out, "R{row}C{}: {}", c.col + 1, c.text)?;
            if let Some(p) = &c.parameter {
                write!(out, " [{p}]")?;
            }
            writeln!(out)?;
        }
    }
    Ok(())
}

pub fn render_text(view: &TemplateView, out: &mut dyn Write) -> crate::Result<()> {
    match view {
        TemplateView::Dcs { .. } => dcs_text(view, out),
        TemplateView::Sheet { .. } => sheet_text(view, out),
        TemplateView::Other {
            files,
            text,
            too_large,
            ..
        } => {
            if *too_large {
                writeln!(out, "Template.xml is larger than the viewer limit")?;
            } else if let Some(text) = text {
                out.write_all(text.as_bytes())?;
            } else {
                for (name, size) in files {
                    writeln!(out, "{name}  {size}")?;
                }
            }
            Ok(())
        }
    }
}

#[derive(Serialize)]
struct TemplateRow<'a> {
    path: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct TemplateListOutput<'a> {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    templates: Vec<TemplateRow<'a>>,
}

fn listed_kind(
    template: Option<&Template>,
    props: &[mdconf::Prop],
    sub_hdr_non_empty: bool,
) -> Option<Option<TemplateType>> {
    if let Some(t) = template {
        return Some(t.kind);
    }
    if !sub_hdr_non_empty {
        return None;
    }
    let name = props
        .iter()
        .find_map(|p| match (&p.value, p.name.as_str()) {
            (mdconf::Value::Text(s), "TemplateType") => Some(s.as_str()),
            _ => None,
        })?;
    TemplateType::from_name(name).map(Some)
}

fn write_template_list(
    rows: &[(String, Option<TemplateType>)],
    format: Format,
    out: &mut dyn Write,
) -> crate::Result<()> {
    let mut w = BufWriter::with_capacity(64 * 1024, out);
    match format {
        Format::Text => {
            for (path, kind) in rows {
                writeln!(w, "{path}\t{}", kind_name(*kind))?;
            }
        }
        Format::Json => {
            let payload = TemplateListOutput {
                schema_version: 1,
                templates: rows
                    .iter()
                    .map(|(path, kind)| TemplateRow {
                        path: path.as_str(),
                        kind: kind_name(*kind),
                    })
                    .collect(),
            };
            serde_json::to_writer_pretty(&mut w, &payload)?;
            writeln!(w)?;
        }
    }
    w.flush()?;
    Ok(())
}

fn list_templates(
    cfg: &Configuration,
    scope: &ScopeArgs,
    types: &[TypeFilter],
    format: Format,
    out: &mut dyn Write,
) -> crate::Result<()> {
    let scope = Scope::resolve(cfg, scope)?;
    let mut rows = Vec::new();
    let mut add = |path: String, kind: Option<TemplateType>| {
        if scope.contains(&path) && (types.is_empty() || types.iter().any(|f| f.matches(kind))) {
            rows.push((path, kind));
        }
    };
    for o in &cfg.objects {
        let path = format!("{}.{}", o.kind.as_str(), o.name);
        if o.kind == MdKind::CommonTemplate
            && let Some(kind) = listed_kind(o.template.as_ref(), &o.props, true)
        {
            add(path.clone(), kind);
        }
        for s in o.templates.iter().filter(|s| s.kind == SubKind::Template) {
            let hdr_ok = cfg.index.sub_hdr.get(&s.id).is_none_or(|h| !h.is_empty());
            if let Some(kind) = listed_kind(s.template.as_ref(), &s.props, hdr_ok) {
                add(format!("{path}.{}.{}", sub_kind_en(s.kind), s.name), kind);
            }
        }
    }
    write_template_list(&rows, format, out)
}

fn find_collection<'n>(node: &'n brace::Node, class: &str) -> Option<&'n [brace::Node]> {
    for e in node.as_list()? {
        let Some(l) = e.as_list() else { continue };
        let (Some(head), Some(cnt)) = (l.first(), l.get(1)) else {
            continue;
        };
        if head.text() != Some(class) || cnt.is_list() {
            continue;
        }
        let Some(n) = cnt.text().and_then(|t| t.trim().parse::<usize>().ok()) else {
            continue;
        };
        let items = l.get(2..)?;
        return Some(items.get(..n.min(items.len())).unwrap_or(items));
    }
    None
}

fn find_header(tree: &brace::Node, uuid: &str) -> Option<Vec<usize>> {
    let is_header = |n: &brace::Node| {
        n.as_list().is_some_and(|l| {
            l.len() >= 5
                && matches!(l.get(2), Some(brace::Node::Str(_)))
                && l.get(1).and_then(brace::Node::as_list).is_some_and(|h| {
                    h.len() == 3 && h.get(2).and_then(brace::Node::text) == Some(uuid)
                })
        })
    };
    if is_header(tree) {
        return Some(Vec::new());
    }
    let mut stack: Vec<(&[brace::Node], usize)> = vec![(tree.as_list()?, 0)];
    while let Some((l, i)) = stack.last_mut() {
        let Some(c) = l.get(*i) else {
            stack.pop();
            if let Some((_, j)) = stack.last_mut() {
                *j += 1;
            }
            continue;
        };
        if is_header(c) {
            return Some(stack.iter().map(|(_, j)| *j).collect());
        }
        match c.as_list() {
            Some(cl) => stack.push((cl, 0)),
            None => *i += 1,
        }
    }
    None
}

fn name_at(t: &brace::Node, header: &[usize]) -> String {
    let mut p = Vec::with_capacity(header.len() + 1);
    p.extend_from_slice(header);
    p.push(2);
    t.get(&p)
        .and_then(brace::Node::text)
        .unwrap_or_default()
        .to_string()
}

fn kind_has_templates(kind: MdKind) -> bool {
    kind == MdKind::CommonTemplate
        || mdconf::startup::kind_spec(kind.as_str())
            .and_then(|spec| spec.field("subobjects"))
            .and_then(|subs| subs.get("Template"))
            .and_then(|v| v.as_str())
            .is_some()
}

fn parse_kind_filter(raw: &[String]) -> crate::Result<Vec<MdKind>> {
    let mut kinds = Vec::new();
    for item in raw.iter().flat_map(|a| a.split(',')) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        kinds.push(kind_from_name(item).ok_or_else(|| format!("unknown metadata kind: {item}"))?);
    }
    Ok(kinds)
}

fn decode_brace(
    src: &dyn mdconf::EntrySource,
    present: &HashSet<String>,
    name: &str,
) -> Option<brace::Doc> {
    if !present.contains(name) {
        return None;
    }
    let stored = src.stored_owned(name).ok().flatten()?;
    match mdconf::decode_stored(&stored).ok()? {
        mdconf::EntryData::Brace(doc) => Some(doc),
        _ => None,
    }
}

fn list_templates_direct(
    source_kind: source::SourceKind,
    src: &dyn mdconf::EntrySource,
    kinds_filter: &[MdKind],
    types: &[TypeFilter],
) -> crate::Result<Vec<(String, Option<TemplateType>)>> {
    let list = src.list()?;
    if source_kind == source::SourceKind::Infobase && list.is_empty() {
        return Ok(Vec::new());
    }
    let root_cfg = mdconf::root_facts::configuration_root(src)?;
    let present: HashSet<String> = list.into_iter().map(|m| m.name).collect();
    let cfg_entry = root_cfg.id.to_string();
    let Some(cfg_doc) = decode_brace(src, &present, &cfg_entry) else {
        return Err(format!("{cfg_entry}: not a brace document").into());
    };
    let mut class_to_kind: HashMap<&'static str, MdKind> = HashMap::new();
    for &kind in MdKind::all() {
        if let Some(cls) = mdconf::startup::kind_spec(kind.as_str()).and_then(|s| s.class()) {
            class_to_kind.insert(cls, kind);
        }
    }
    let common_type_map = mdconf::startup::kind_spec("CommonTemplate")
        .and_then(|spec| spec.props().find(|(n, _)| *n == "TemplateType"))
        .and_then(|(_, prop)| prop.field("map"));
    let sub_type_map = mdconf::startup::kind_spec("Template")
        .and_then(|spec| spec.props().find(|(n, _)| *n == "TemplateType"))
        .and_then(|(_, prop)| prop.field("map"));

    let mut class_lists: Vec<(MdKind, Vec<brace::Guid>)> = Vec::new();
    let mut stack = vec![&cfg_doc.root];
    while let Some(node) = stack.pop() {
        let Some(l) = node.as_list() else { continue };
        if let (Some(head), Some(cnt)) = (l.first(), l.get(1))
            && let Some(kind) = head.text().and_then(|c| class_to_kind.get(c).copied())
            && !cnt.is_list()
            && let Some(c) = cnt.text().and_then(|t| t.trim().parse::<usize>().ok())
        {
            let items = l.get(2..).unwrap_or_default();
            let items = items.get(..c.min(items.len())).unwrap_or(items);
            class_lists.push((
                kind,
                items.iter().filter_map(brace::Node::as_guid).collect(),
            ));
            continue;
        }
        stack.extend(l.iter().rev());
    }

    let mut rows = Vec::new();
    let mut push_if = |path: String, kind: TemplateType| {
        let k = Some(kind);
        if types.is_empty() || types.iter().any(|f| f.matches(k)) {
            rows.push((path, k));
        }
    };

    for (kind, uuids) in class_lists {
        if !kinds_filter.is_empty() && !kinds_filter.contains(&kind) {
            continue;
        }
        let Some(spec) = mdconf::startup::kind_spec(kind.as_str()) else {
            continue;
        };
        let Some(header_path) = spec.header() else {
            continue;
        };
        let tpl_class = spec
            .field("subobjects")
            .and_then(|subs| subs.get("Template"))
            .and_then(|v| v.as_str());
        if kind != MdKind::CommonTemplate && tpl_class.is_none() {
            continue;
        }
        let header: Vec<usize> = header_path.iter().collect();
        for u in uuids {
            let entry = u.to_string();
            let Some(doc) = decode_brace(src, &present, &entry) else {
                continue;
            };
            let normalized;
            let tree: &brace::Node = if kind == MdKind::AccountingRegister
                && doc.root.get(&[1, 0]).and_then(brace::Node::as_i64) == Some(22)
            {
                let mut t = doc.root.clone();
                if let Some(body) = t.get_mut(&[1]).and_then(brace::Node::as_list_mut)
                    && body.len() > 1
                {
                    body.remove(1);
                }
                normalized = t;
                &normalized
            } else {
                &doc.root
            };
            let obj_name = name_at(tree, &header);
            if kind == MdKind::CommonTemplate
                && let Some(key) = doc.root.get(&[1, 2]).and_then(brace::Node::text)
                && let Some(tt) = common_type_map
                    .and_then(|m| m.get(key))
                    .and_then(|v| v.as_str())
                    .and_then(TemplateType::from_name)
            {
                push_if(format!("CommonTemplate.{obj_name}"), tt);
            }
            if let Some(tpl_class) = tpl_class {
                for su in find_collection(tree, tpl_class)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(brace::Node::as_guid)
                {
                    let sub_entry = su.to_string();
                    let Some(sdoc) = decode_brace(src, &present, &sub_entry) else {
                        continue;
                    };
                    let Some(h) = find_header(&sdoc.root, &sub_entry) else {
                        continue;
                    };
                    let sub_name = name_at(&sdoc.root, &h);
                    let Some(prefix_len) = h.len().checked_sub(1) else {
                        continue;
                    };
                    let Some(prefix) = h.get(..prefix_len) else {
                        continue;
                    };
                    let mut type_path = Vec::with_capacity(prefix.len() + 1);
                    type_path.extend_from_slice(prefix);
                    type_path.push(1);
                    if let Some(key) = sdoc.root.get(&type_path).and_then(brace::Node::text)
                        && let Some(tt) = sub_type_map
                            .and_then(|m| m.get(key))
                            .and_then(|v| v.as_str())
                            .and_then(TemplateType::from_name)
                    {
                        push_if(
                            format!("{}.{obj_name}.Template.{sub_name}", kind.as_str()),
                            tt,
                        );
                    }
                }
            }
        }
    }
    Ok(rows)
}

fn show_query(
    t: &TemplateRef<'_>,
    set: &str,
    format: Format,
    out: &mut dyn Write,
) -> crate::Result<()> {
    let view = view_of(t.template);
    if !matches!(view, TemplateView::Dcs { .. }) {
        return Err(format!("{} is not a data composition schema", t.path).into());
    }
    let sets = query_sets(&view);
    let wanted = fold_path_name(set);
    let Some((name, query)) = sets.iter().find(|(name, _)| fold_path_name(name) == wanted) else {
        let names: Vec<&str> = sets.iter().map(|(n, _)| *n).collect();
        return Err(format!(
            "no data set {set} with a query (sets with queries: {})",
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            }
        )
        .into());
    };
    match format {
        Format::Text => {
            out.write_all(query.as_bytes())?;
            if !query.ends_with('\n') {
                writeln!(out)?;
            }
        }
        Format::Json => write_json(
            out,
            &json!({
                "schemaVersion": 1,
                "path": t.path,
                "dataSet": name,
                "query": query,
            }),
        )?,
    }
    Ok(())
}

fn show_template(
    cfg: &Configuration,
    template: &str,
    query_of: Option<&str>,
    format: Format,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> crate::Result<()> {
    let t = resolve_template(cfg, template)?;
    if let Some(set) = query_of {
        return show_query(&t, set, format, out);
    }
    let view = view_of(t.template);
    if let TemplateView::Sheet {
        rows,
        first_row,
        total_rows,
        truncated: true,
        ..
    } = &view
    {
        writeln!(
            err,
            "yellowcode: shown rows {}–{} of {total_rows}",
            first_row + 1,
            first_row + u32::try_from(rows.len()).unwrap_or(u32::MAX)
        )?;
    }
    match format {
        Format::Text => render_text(&view, out),
        Format::Json => write_json(
            out,
            &json!({
                "schemaVersion": 1,
                "path": t.path,
                "type": kind_name(t.kind),
                "view": view,
            }),
        ),
    }
}

pub fn run_with_memory_limit(
    command: Command,
    limit: Option<String>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> crate::Result<()> {
    let memory = source::OpenMemory::resolve(limit)?;
    match command {
        Command::List {
            source,
            scope,
            types,
            format,
        } => {
            let detected = source::detect(&source::resolve(&source));
            if scope.paths.is_empty()
                && scope.subsystems.is_empty()
                && let Ok(
                    kind @ (source::SourceKind::Cf
                    | source::SourceKind::Dt
                    | source::SourceKind::Infobase),
                ) = detected
            {
                let kinds_filter = parse_kind_filter(&scope.kinds)?;
                let rows = source::with_config_entries_with_memory(&source, &memory, |src| {
                    list_templates_direct(kind, src, &kinds_filter, &types)
                })?;
                return write_template_list(&rows, format, out);
            }
            if detected.is_ok() {
                let _ = parse_kind_filter(&scope.kinds)?;
            }
            let opts = super::load_options(super::Reads::TemplateList);
            let cfg = if scope.subsystems.is_empty() && !scope.paths.is_empty() {
                let paths: Vec<&str> = scope.paths.iter().map(String::as_str).collect();
                super::load_objects(
                    &paths,
                    true,
                    &opts,
                    &|cfg| Scope::resolve(cfg, &scope).is_ok(),
                    &mut |o| {
                        source::open_with_memory_bounded(&source, o, super::BOUNDED_CACHE, &memory)
                    },
                )?
            } else if !scope.subsystems.is_empty() {
                let path_kinds: Vec<MdKind> = scope
                    .paths
                    .iter()
                    .filter_map(|p| p.split('.').next().and_then(|k| kind_from_name(k.trim())))
                    .collect();
                let filter = mdconf::ObjectFilter {
                    index_kinds: None,
                    keep: std::sync::Arc::new(move |kind, _| {
                        kind == MdKind::Subsystem
                            || kind_has_templates(kind)
                            || path_kinds.contains(&kind)
                    }),
                };
                let filtered = source::open_with_memory_bounded(
                    &source,
                    &mdconf::LoadOptions {
                        objects: Some(filter),
                        ..opts.clone()
                    },
                    super::BOUNDED_CACHE,
                    &memory,
                )?;
                if Scope::resolve(&filtered, &scope).is_ok() {
                    filtered
                } else {
                    source::open_with_memory_bounded(&source, &opts, super::BOUNDED_CACHE, &memory)?
                }
            } else {
                source::open_with_memory_bounded(&source, &opts, super::BOUNDED_CACHE, &memory)?
            };
            list_templates(&cfg, &scope, &types, format, out)
        }
        Command::Show {
            source,
            template,
            query_of,
            format,
        } => {
            let cfg = open_for_show(&source, &template, &memory)?;
            show_template(&cfg, &template, query_of.as_deref(), format, out, err)
        }
    }
}

fn open_for_show(
    source: &std::path::Path,
    template: &str,
    memory: &source::OpenMemory,
) -> crate::Result<Configuration> {
    if let Some(filter) = super::objects_of(&[template], true) {
        let mut opts = super::load_options(super::Reads::Templates);
        opts.objects = Some(filter);
        let cfg = source::open_with_memory_bounded(source, &opts, super::BOUNDED_CACHE, memory)?;
        if path::resolve(&cfg, template).is_ok() {
            return Ok(cfg);
        }
    }
    source::open_with_memory_bounded(
        source,
        &super::load_options(super::Reads::TemplateList),
        super::BOUNDED_CACHE,
        memory,
    )
}
