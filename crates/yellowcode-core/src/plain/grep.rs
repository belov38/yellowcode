use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use mdconf::{Configuration, LoadOptions};
use serde::Serialize;
use serde_json::json;

use super::metadata::Format;
use super::module::write_json;
use super::modules::modules;
use super::scope::{Scope, ScopeArgs};
use super::template::{query_sets, templates};
use crate::viewer::find::{FindRegex, compile_with_case, find, fold};
use crate::viewer::module::split_lines;
use crate::viewer::source;
use crate::viewer::template_view::{TemplateView, view_of};
use mdconf::templates::TemplateType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    Code,
    Queries,
    Names,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Lines,
    FilesWithMatches,
    Count,
}

#[derive(Clone)]
pub struct GrepArgs {
    pub pattern: String,
    pub fixed: bool,
    pub case_sensitive: bool,
    pub places: Vec<Where>,
    pub context: usize,
    pub output: Output,
    pub max_count: usize,
    pub format: Format,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Match {
    pub path: String,
    pub line: Option<u32>,
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub before: Vec<(u32, String)>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<(u32, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathCount {
    pub path: String,
    pub count: usize,
}

#[derive(Debug, Default)]
pub struct Found {
    pub matches: Vec<Match>,
    pub total: usize,
    pub paths: Vec<PathCount>,
    pub warnings: Vec<String>,
}

enum LineMatcher {
    Folded(String),
    Exact(String),
    Regex(Box<FindRegex>),
}

impl LineMatcher {
    fn new(args: &GrepArgs) -> crate::Result<Self> {
        Ok(match (args.fixed, args.case_sensitive) {
            (true, false) => Self::Folded(fold(&args.pattern)),
            (true, true) => Self::Exact(args.pattern.clone()),
            (false, case_sensitive) => {
                Self::Regex(Box::new(compile_with_case(&args.pattern, !case_sensitive)?))
            }
        })
    }

    fn is_match(&mut self, line: &str) -> bool {
        match self {
            Self::Folded(p) => fold(line).contains(p.as_str()),
            Self::Exact(p) => line.contains(p.as_str()),
            Self::Regex(r) => r.is_match(line),
        }
    }
}

fn number(index: usize) -> u32 {
    u32::try_from(index + 1).unwrap_or(u32::MAX)
}

fn numbered(lines: &[&str], range: std::ops::Range<usize>) -> Vec<(u32, String)> {
    range
        .map(|i| {
            (
                number(i),
                lines.get(i).copied().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn grep_text(
    path: &str,
    text: &str,
    matcher: &mut LineMatcher,
    args: &GrepArgs,
    found: &mut Found,
) {
    let keep_lines = args.output == Output::Lines;
    let lines = split_lines(text);
    let mut count = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if !matcher.is_match(line) {
            continue;
        }
        count += 1;
        if keep_lines && found.matches.len() < args.max_count {
            found.matches.push(Match {
                path: path.to_string(),
                line: Some(number(i)),
                text: (*line).to_string(),
                before: numbered(&lines, i.saturating_sub(args.context)..i),
                after: numbered(
                    &lines,
                    i + 1..lines.len().min((i + 1).saturating_add(args.context)),
                ),
            });
        }
    }
    if count > 0 {
        found.total += count;
        found.paths.push(PathCount {
            path: path.to_string(),
            count,
        });
    }
}

fn grep_code(
    cfg: &Configuration,
    scope: &Scope,
    matcher: &mut LineMatcher,
    args: &GrepArgs,
    found: &mut Found,
) {
    for m in modules(cfg, scope) {
        grep_text(&m.path, &m.module.text(), matcher, args, found);
    }
}

fn grep_queries(
    cfg: &Configuration,
    scope: &Scope,
    matcher: &mut LineMatcher,
    args: &GrepArgs,
    found: &mut Found,
) {
    for t in templates(cfg, scope) {
        if t.kind != Some(TemplateType::DataCompositionSchema) {
            continue;
        }
        let view = view_of(t.template);
        if matches!(
            view,
            TemplateView::Other {
                too_large: true,
                ..
            }
        ) {
            found.warnings.push(format!(
                "{}: Template.xml is larger than the viewer limit, not searched",
                t.path
            ));
            continue;
        }
        for (set, query) in query_sets(&view) {
            grep_text(&format!("{}#{set}", t.path), query, matcher, args, found);
        }
    }
}

fn grep_names(
    cfg: &Configuration,
    scope: &Scope,
    args: &GrepArgs,
    found: &mut Found,
) -> crate::Result<()> {
    let keep_lines = args.output == Output::Lines;
    for hit in find(cfg, &args.pattern, !args.fixed, true)? {
        if !scope.contains(&hit.path) {
            continue;
        }
        found.total += 1;
        if keep_lines && found.matches.len() < args.max_count {
            found.matches.push(Match {
                path: hit.path.clone(),
                line: None,
                text: hit.text,
                before: Vec::new(),
                after: Vec::new(),
            });
        }
        found.paths.push(PathCount {
            path: hit.path,
            count: 1,
        });
    }
    Ok(())
}

pub fn grep(cfg: &Configuration, scope: &Scope, args: &GrepArgs) -> crate::Result<Found> {
    let mut found = Found::default();
    if args.places.contains(&Where::Code) || args.places.contains(&Where::Queries) {
        let mut matcher = LineMatcher::new(args)?;
        if args.places.contains(&Where::Code) {
            grep_code(cfg, scope, &mut matcher, args, &mut found);
        }
        if args.places.contains(&Where::Queries) {
            grep_queries(cfg, scope, &mut matcher, args, &mut found);
        }
    }
    if args.places.contains(&Where::Names) {
        grep_names(cfg, scope, args, &mut found)?;
    }
    Ok(found)
}

fn render_group(
    group: &[Match],
    with_context: bool,
    separate_first: bool,
    out: &mut dyn Write,
) -> crate::Result<()> {
    let Some(first) = group.first() else {
        return Ok(());
    };
    let mut rows: BTreeMap<u32, (bool, &str)> = BTreeMap::new();
    for m in group {
        for (n, t) in m.before.iter().chain(&m.after) {
            rows.entry(*n).or_insert((false, t.as_str()));
        }
    }
    for m in group {
        if let Some(n) = m.line {
            rows.insert(n, (true, m.text.as_str()));
        }
    }
    let mut previous: Option<u32> = None;
    for (n, (is_match, text)) in rows {
        let gap = with_context && previous.map_or(separate_first, |p| n != p.saturating_add(1));
        if gap {
            writeln!(out, "--")?;
        }
        let mark = if is_match { ':' } else { '-' };
        writeln!(out, "{}{mark}{n}{mark}{text}", first.path)?;
        previous = Some(n);
    }
    Ok(())
}

fn render_lines(found: &Found, args: &GrepArgs, out: &mut dyn Write) -> crate::Result<()> {
    let with_context = args.context > 0;
    let mut printed = false;
    for group in found
        .matches
        .chunk_by(|a, b| a.line.is_some() && b.line.is_some() && a.path == b.path)
    {
        if let [m] = group
            && m.line.is_none()
        {
            if with_context && printed {
                writeln!(out, "--")?;
            }
            writeln!(out, "{}:{}", m.path, m.text)?;
        } else {
            render_group(group, with_context, printed, out)?;
        }
        printed = true;
    }
    Ok(())
}

pub fn render(found: &Found, args: &GrepArgs, out: &mut dyn Write) -> crate::Result<()> {
    match (args.format, args.output) {
        (Format::Json, Output::Lines) => write_json(
            out,
            &json!({
                "schemaVersion": 1,
                "total": found.total,
                "truncated": found.total > found.matches.len(),
                "matches": found.matches,
            }),
        ),
        (Format::Json, _) => write_json(
            out,
            &json!({"schemaVersion": 1, "total": found.total, "paths": found.paths}),
        ),
        (Format::Text, Output::Lines) => render_lines(found, args, out),
        (Format::Text, Output::FilesWithMatches) => {
            for p in &found.paths {
                writeln!(out, "{}", p.path)?;
            }
            Ok(())
        }
        (Format::Text, Output::Count) => {
            for p in &found.paths {
                writeln!(out, "{}:{}", p.path, p.count)?;
            }
            Ok(())
        }
    }
}

fn announce(found: &Found, args: &GrepArgs, err: &mut dyn Write) -> crate::Result<()> {
    for warning in &found.warnings {
        writeln!(err, "yellowcode: {warning}")?;
    }
    if args.output == Output::Lines && found.total > found.matches.len() {
        writeln!(
            err,
            "yellowcode: shown {} of {} matches; raise --max-count",
            found.matches.len(),
            found.total
        )?;
    }
    Ok(())
}

pub fn run_with_memory_limit(
    source: PathBuf,
    scope: ScopeArgs,
    args: GrepArgs,
    limit: Option<String>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> crate::Result<i32> {
    let memory = source::OpenMemory::resolve(limit)?;
    let cfg = source::open_with_memory(&source, &LoadOptions::default(), &memory, &mut |_| Ok(()))?;
    let scope = Scope::resolve(&cfg, &scope)?;
    let found = grep(&cfg, &scope, &args)?;
    render(&found, &args, out)?;
    announce(&found, &args, err)?;
    Ok(if found.total > 0 { 0 } else { 1 })
}
