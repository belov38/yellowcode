use std::collections::{BTreeMap, BTreeSet, HashMap};

use form_model::stdcmd::{self, FormRole, NameLang, std_name};
use form_model::{FormDesc, Item};

use super::{button_name, near, record_kind};
use crate::{BoxKey, BoxKind, ItemBox, Layout, Part};

#[derive(Debug, Clone, serde::Deserialize)]
pub struct GenRecord {
    pub id: String,
    pub iface: String,
    pub form: String,
    pub key: Option<serde_json::Value>,
    pub viewport: [f32; 2],
    #[serde(default)]
    pub cfg: Option<String>,
    #[serde(default)]
    pub floating: Option<bool>,
    #[serde(default)]
    pub window: Option<[f32; 4]>,
    #[serde(default)]
    pub workspace: Option<[f32; 4]>,
    #[serde(default)]
    pub content: Option<[f32; 4]>,
    #[serde(default)]
    pub commandbar: Option<[f32; 4]>,
    pub elements: Vec<GenElement>,
    pub commands: Vec<String>,
    pub hidden: Vec<String>,
    #[serde(default)]
    pub grids: BTreeMap<String, GenGrid>,
    #[serde(default)]
    pub menu: BTreeMap<String, Vec<String>>,
}

pub type GenElement = (String, String, Option<String>, [f32; 4], Vec<String>);

#[derive(Debug, Clone, serde::Deserialize)]
pub struct GenGrid {
    pub head: Vec<String>,
    #[serde(default)]
    pub rows: Vec<Vec<String>>,
    #[serde(default)]
    pub r: Option<[f32; 4]>,
    #[serde(default)]
    pub cols: Vec<f32>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GenResult {
    pub id: String,
    pub composition_ok: bool,
    pub window_ok: bool,
    pub geometry_ok: bool,
    pub items: usize,
    pub items_geom_ok: usize,
    pub geom_diffs: Vec<String>,
    pub diffs: Vec<String>,
}

fn norm(s: &str) -> String {
    s.replace('\u{a0}', " ")
}

fn name_of(key: &BoxKey, lang: NameLang) -> Option<String> {
    match key {
        BoxKey::Item(n) => Some(n.clone()),
        BoxKey::Part { owner, part } => match part {
            Part::TableCommand(c) => Some(format!("{owner}{c}")),
            Part::SearchString { bar } => {
                Some(format!("{bar}{owner}{}", std_name("SearchString", lang)))
            }
            Part::ViewStatus => Some(format!("{owner}{}", std_name("ViewStatus", lang))),
            _ => None,
        },
    }
}

fn dom_order(boxes: &[ItemBox]) -> Vec<&ItemBox> {
    let mut out = Vec::with_capacity(boxes.len());
    let mut i = 0;
    while i < boxes.len() {
        let Some(b) = boxes.get(i) else { break };
        if b.kind != BoxKind::Grid {
            out.push(b);
            i += 1;
            continue;
        }
        let end = boxes
            .iter()
            .enumerate()
            .skip(i + 1)
            .find(|(_, x)| x.kind == BoxKind::ViewStatus)
            .map_or(boxes.len(), |(j, _)| j);
        let run = boxes.get(i + 1..end).unwrap_or_default();
        let is_search = |x: &&ItemBox| {
            matches!(
                x.key,
                BoxKey::Part {
                    part: Part::SearchString { .. },
                    ..
                }
            )
        };
        if let Some(bar) = run.first().filter(|x| x.kind == BoxKind::CommandBar) {
            out.push(bar);
            out.extend(run.iter().skip(1).filter(is_search));
            out.extend(run.iter().skip(1).filter(|x| !is_search(x)));
        } else {
            out.extend(run.iter());
        }
        out.push(b);
        i = end;
    }
    out
}

fn listed(b: &ItemBox) -> bool {
    !b.hidden
        && !matches!(
            b.kind,
            BoxKind::Column
                | BoxKind::Group
                | BoxKind::Page
                | BoxKind::Tabs
                | BoxKind::ViewStatus
                | BoxKind::SpinButtons
                | BoxKind::Preloader
        )
}

struct Elem<'a> {
    name: String,
    b: &'a ItemBox,
}

fn dom_elements<'a>(layout: &'a Layout, lang: NameLang) -> Vec<Elem<'a>> {
    dom_order(&layout.boxes)
        .into_iter()
        .filter(|b| listed(b))
        .filter_map(|b| name_of(&b.key, lang).map(|name| Elem { name, b }))
        .collect()
}

type Tuple = (String, String, Option<String>, Vec<String>);

fn ours_tuple(e: &Elem) -> Tuple {
    (
        e.name.clone(),
        record_kind(&e.b.kind).to_string(),
        e.b.title.as_ref().map(|t| norm(&t.text)),
        e.b.buttons
            .iter()
            .map(|x| button_name(*x).to_string())
            .collect(),
    )
}

fn compare_elements(rec: &GenRecord, ours: &[Elem], diffs: &mut Vec<String>) {
    let theirs: Vec<Tuple> = rec
        .elements
        .iter()
        .map(|(n, k, t, _, b)| (n.clone(), k.clone(), t.as_deref().map(norm), b.clone()))
        .collect();
    let mut mine: Vec<Tuple> = ours.iter().map(ours_tuple).collect();
    for ((m, t), e) in mine.iter_mut().zip(&theirs).zip(ours) {
        let wrapped = e.b.title.as_ref().is_some_and(|t| t.lines > 1);
        if let (Some(a), Some(b)) = (&m.2, &t.2)
            && wrapped
            && a != b
            && a.split_whitespace().collect::<String>() == b.split_whitespace().collect::<String>()
        {
            m.2 = Some(b.clone());
        }
    }
    if mine == theirs {
        return;
    }
    let names = |v: &[Tuple]| v.iter().map(|t| t.0.clone()).collect::<Vec<_>>();
    let (mn, tn) = (names(&mine), names(&theirs));
    let only_ours: Vec<&String> = mn.iter().filter(|n| !tn.contains(n)).collect();
    let only_theirs: Vec<&String> = tn.iter().filter(|n| !mn.contains(n)).collect();
    if !only_ours.is_empty() || !only_theirs.is_empty() {
        diffs.push(format!(
            "elements: only ours {only_ours:?}, only theirs {only_theirs:?}"
        ));
    }
    if mn != tn && only_ours.is_empty() && only_theirs.is_empty() {
        diffs.push(format!("elements: order ours {mn:?} vs theirs {tn:?}"));
    }
    for (m, t) in mine
        .iter()
        .zip(&theirs)
        .filter(|(m, t)| m.0 == t.0 && m != t)
    {
        diffs.push(format!("element {}: ours {m:?} vs theirs {t:?}", m.0));
    }
}

fn our_commands(ours: &[Elem]) -> Vec<String> {
    ours.iter()
        .filter(|e| e.b.kind == BoxKind::CommandButton)
        .map(|e| norm(e.b.text.as_deref().unwrap_or(&e.name)))
        .collect()
}

fn is_service(name: &str) -> bool {
    if name.ends_with("_submenu") || name.contains("_all_sub") {
        return true;
    }
    name.rsplit_once('$')
        .is_some_and(|(_, n)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

fn is_ts_column(form: &FormDesc, name: &str) -> bool {
    let Some(path) = form.find(name).and_then(|i| i.props.data_path.as_deref()) else {
        return false;
    };
    let segs: Vec<&str> = path.split('.').collect();
    segs.len() >= 3
        && !segs
            .last()
            .is_some_and(|l| ["НомерСтроки", "LineNumber"].contains(l))
}

fn our_hidden(form: &FormDesc, layout: &Layout) -> BTreeSet<String> {
    let lang = form.name_lang;
    let mut out = BTreeSet::new();
    for b in layout.boxes.iter().filter(|b| b.hidden) {
        let Some(name) = name_of(&b.key, lang) else {
            continue;
        };
        match b.kind {
            BoxKind::CommandBar => {}
            BoxKind::Column => {
                if is_ts_column(form, &name) {
                    out.insert(name);
                }
            }
            _ => {
                out.insert(name);
            }
        }
    }
    if let (FormRole::List { .. }, Some(table), Some(bar)) = (
        stdcmd::form_role(form),
        stdcmd::main_table(form),
        form.auto_command_bar.as_ref(),
    ) {
        let cmds = stdcmd::bar_commands(form, bar);
        let has = |id: &str| cmds.iter().any(|c| c.id == id);
        let on_table = |id: &str| format!("{}{}", table.name, std_name(id, lang));
        if has("SearchControl") {
            for id in ["FindByCurrentValue", "Find", "CancelSearch"] {
                out.insert(on_table(id));
            }
        }
        if has("SetDateInterval") {
            out.insert(on_table("SetDateInterval"));
        }
        if has("ViewModeSubmenu") {
            for id in ["HierarchicalList", "List", "Tree"] {
                out.insert(format!("{}{}", std_name("Form", lang), std_name(id, lang)));
            }
        }
    }
    out
}

fn compare_hidden(rec: &GenRecord, form: &FormDesc, layout: &Layout, diffs: &mut Vec<String>) {
    let ours = our_hidden(form, layout);
    let theirs: BTreeSet<String> = rec
        .hidden
        .iter()
        .filter(|n| !is_service(n))
        .cloned()
        .collect();
    if ours != theirs {
        let a: Vec<_> = ours.difference(&theirs).collect();
        let b: Vec<_> = theirs.difference(&ours).collect();
        diffs.push(format!("hidden: only ours {a:?}, only theirs {b:?}"));
    }
}

fn compare_grids(rec: &GenRecord, ours: &[Elem], diffs: &mut Vec<String>) {
    let mine: BTreeMap<&str, Vec<String>> = ours
        .iter()
        .filter_map(|e| {
            let g = e.b.grid.as_ref()?;
            let head = g
                .columns
                .iter()
                .map(|c| norm(&c.title))
                .filter(|t| !t.is_empty())
                .collect();
            Some((e.name.as_str(), head))
        })
        .collect();
    let theirs: BTreeMap<&str, Vec<String>> = rec
        .grids
        .iter()
        .map(|(k, g)| (k.as_str(), g.head.iter().map(|h| norm(h)).collect()))
        .collect();
    if mine != theirs {
        diffs.push(format!("grids: ours {mine:?} vs theirs {theirs:?}"));
    }
}

fn menu_lines(form: &FormDesc, bar: &Item) -> Vec<String> {
    let mut cmds = stdcmd::bar_commands(form, bar);
    if let Some(p) = cmds.iter().position(|c| c.id == "SearchControl") {
        let c = cmds.remove(p);
        let at = cmds
            .iter()
            .position(|c| c.id == "CustomizeForm")
            .unwrap_or(cmds.len());
        cmds.insert(at, c);
    }
    let is_list = matches!(stdcmd::form_role(form), FormRole::List { .. });
    let marks = cmds.iter().any(|c| c.id == "SetDeletionMark");
    let mut out = Vec::new();
    for c in cmds.iter().filter(|c| c.in_menu) {
        let Some(cap) = ui_style::captions::caption(c.id) else {
            out.push(c.id.to_string());
            continue;
        };
        if !c.on_bar || c.picture_only {
            out.push(norm(&cap.ru));
        }
        let keyed = is_list || !matches!(c.id, "Delete" | "SetDeletionMark");
        let key = match c.id {
            "Delete" if !marks => Some("Del"),
            _ => cap.hotkey.as_deref(),
        };
        if let Some(k) = key.filter(|_| keyed) {
            out.push(k.to_string());
        }
    }
    out
}

fn menu_key(k: &str) -> &str {
    k.strip_prefix("form")
        .and_then(|r| {
            let rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
            (rest.len() < r.len())
                .then(|| rest.strip_prefix('_'))
                .flatten()
        })
        .unwrap_or(k)
}

fn same_line(ours: &str, theirs: &str) -> bool {
    let (o, t) = (norm(ours), norm(theirs));
    (o.starts_with("Найти: ") && t.starts_with("Найти: ")) || o == t
}

fn compare_menu(rec: &GenRecord, form: &FormDesc, diffs: &mut Vec<String>) {
    let Some(bar) = form.auto_command_bar.as_ref() else {
        return;
    };
    let key = format!("allActions{}", bar.name);
    let lines = menu_lines(form, bar);
    for (k, theirs) in &rec.menu {
        if theirs.first().is_some_and(|l| l.starts_with("!error")) {
            continue;
        }
        let k = menu_key(k);
        if k != key {
            diffs.push(format!("menu {k}: no such menu in ours"));
            continue;
        }
        let ok =
            lines.len() == theirs.len() && lines.iter().zip(theirs).all(|(a, b)| same_line(a, b));
        if !ok {
            diffs.push(format!("menu {k}: ours {lines:?} vs theirs {theirs:?}"));
        }
    }
}

pub fn compare(rec: &GenRecord, form: &FormDesc, layout: &Layout) -> GenResult {
    let ours = dom_elements(layout, form.name_lang);
    let mut diffs = Vec::new();
    compare_elements(rec, &ours, &mut diffs);
    let (a, b) = (
        our_commands(&ours),
        rec.commands.iter().map(|c| norm(c)).collect::<Vec<_>>(),
    );
    if a != b {
        diffs.push(format!("commands: ours {a:?} vs theirs {b:?}"));
    }
    compare_hidden(rec, form, layout, &mut diffs);
    compare_grids(rec, &ours, &mut diffs);
    compare_menu(rec, form, &mut diffs);

    let (window_ok, mut geom_diffs) = compare_window(rec, layout, &ours);
    let by_name: HashMap<&str, &Elem> = ours.iter().map(|e| (e.name.as_str(), e)).collect();
    let items = rec.elements.len();
    let mut items_geom_ok = 0;
    for (n, _, _, r, _) in &rec.elements {
        match by_name.get(n.as_str()) {
            Some(e) if near(e.b.rect.to_array(), *r) => items_geom_ok += 1,
            Some(e) => geom_diffs.push(format!("{n}: {:?} vs {r:?}", e.b.rect.to_array())),
            None => geom_diffs.push(format!("{n}: no such element")),
        }
    }
    let mut grids_ok = true;
    for (n, g) in &rec.grids {
        let Some(ge) = by_name.get(n.as_str()).and_then(|e| e.b.grid.as_ref()) else {
            continue;
        };
        let ours_cols: Vec<f32> = ge
            .columns
            .iter()
            .filter(|c| !c.title.is_empty())
            .map(|c| c.w)
            .collect();
        if !g.cols.is_empty()
            && (ours_cols.len() != g.cols.len()
                || ours_cols
                    .iter()
                    .zip(&g.cols)
                    .any(|(a, b)| (a - b).abs() > super::TOLERANCE))
        {
            grids_ok = false;
            geom_diffs.push(format!("grid {n} columns: {ours_cols:?} vs {:?}", g.cols));
        }
    }
    GenResult {
        id: rec.id.clone(),
        composition_ok: diffs.is_empty(),
        window_ok,
        geometry_ok: window_ok && grids_ok && items_geom_ok == items,
        items,
        items_geom_ok,
        geom_diffs,
        diffs,
    }
}

fn compare_window(rec: &GenRecord, layout: &Layout, ours: &[Elem]) -> (bool, Vec<String>) {
    let mut out = Vec::new();
    let w = &layout.window;
    if let Some(f) = rec.floating
        && f != w.floating
    {
        out.push(format!("floating {} vs {f}", w.floating));
    }
    for (name, mine, theirs) in [
        ("window", w.window, rec.window),
        ("workspace", w.workspace, rec.workspace),
        ("content", w.content, rec.content),
    ] {
        if let Some(t) = theirs
            && !near(mine.to_array(), t)
        {
            out.push(format!("{name} {:?} vs {t:?}", mine.to_array()));
        }
    }
    if let Some(t) = rec.commandbar {
        let bar = ours.iter().find(|e| e.b.kind == BoxKind::CommandBar);
        if !bar.is_some_and(|e| near(e.b.rect.to_array(), t)) {
            out.push(format!(
                "commandbar {:?} vs {t:?}",
                bar.map(|e| e.b.rect.to_array())
            ));
        }
    }
    (out.is_empty(), out)
}
