use std::io::Write;
use std::path::PathBuf;

use mdconf::templates::{Template, TemplateType};
use mdconf::{Configuration, LoadOptions, MdKind, SubKind};
use serde_json::json;

use super::metadata::Format;
use super::module::write_json;
use super::scope::{Scope, ScopeArgs};
use crate::viewer::names::{fold_path_name, sub_kind_en};
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

fn list_templates(
    cfg: &Configuration,
    scope: &ScopeArgs,
    types: &[TypeFilter],
    format: Format,
    out: &mut dyn Write,
) -> crate::Result<()> {
    let scope = Scope::resolve(cfg, scope)?;
    let refs: Vec<TemplateRef<'_>> = templates(cfg, &scope)
        .into_iter()
        .filter(|t| types.is_empty() || types.iter().any(|f| f.matches(t.kind)))
        .collect();
    match format {
        Format::Text => {
            for r in &refs {
                writeln!(out, "{}\t{}", r.path, kind_name(r.kind))?;
            }
        }
        Format::Json => {
            let rows: Vec<_> = refs
                .iter()
                .map(|r| json!({"path": r.path, "type": kind_name(r.kind)}))
                .collect();
            write_json(out, &json!({"schemaVersion": 1, "templates": rows}))?;
        }
    }
    Ok(())
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
    let open = |path: &PathBuf| {
        source::open_with_memory(path, &LoadOptions::default(), &memory, &mut |_| Ok(()))
    };
    match command {
        Command::List {
            source,
            scope,
            types,
            format,
        } => list_templates(&open(&source)?, &scope, &types, format, out),
        Command::Show {
            source,
            template,
            query_of,
            format,
        } => show_template(
            &open(&source)?,
            &template,
            query_of.as_deref(),
            format,
            out,
            err,
        ),
    }
}
