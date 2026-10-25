pub mod generated;

use std::collections::{BTreeMap, HashMap};

use crate::{
    BoxKey, BoxKind, FieldButton, ItemState, Layout, LayoutInput, Part, Rect, Size, content_box,
    layout,
};

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Record {
    pub id: String,
    pub iface: String,
    pub form: String,
    #[serde(default)]
    pub source: String,
    pub viewport: [f32; 2],
    pub opened_at: Option<[f32; 2]>,
    pub content: [f32; 4],
    pub items: BTreeMap<String, RecItem>,
    #[serde(default)]
    pub grids: BTreeMap<String, RecGrid>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RecItem {
    pub kind: String,
    pub r: [f32; 4],
    pub title: Option<RecTitle>,
    pub input: Option<[f32; 4]>,
    #[serde(default)]
    pub buttons: Vec<String>,
    #[serde(default)]
    pub flags: Vec<String>,
    pub text: Option<String>,
    pub value: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RecTitle {
    pub r: [f32; 4],
    pub text: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RecGrid {
    pub r: [f32; 4],
    pub content: [f32; 2],
    pub head: Vec<(String, f32, f32)>,
    #[serde(default)]
    pub row_h: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum Diff {
    Error(String),
    Missing(String),
    Kind {
        key: String,
        ours: String,
        theirs: String,
    },
    Coord {
        key: String,
        what: &'static str,
        ours: [f32; 4],
        theirs: [f32; 4],
    },
    Text {
        key: String,
        what: &'static str,
        ours: String,
        theirs: String,
    },
    Hidden {
        key: String,
        ours: bool,
    },
    Grid {
        key: String,
        what: String,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RecordResult {
    pub id: String,
    pub pass: bool,
    pub items: usize,
    pub items_ok: usize,
    pub diffs: Vec<Diff>,
}

pub const TOLERANCE: f32 = 2.0;

pub fn record_key(key: &BoxKey) -> Option<String> {
    match key {
        BoxKey::Item(n) => Some(n.clone()),
        BoxKey::Part { owner, part } => match part {
            Part::TableCommand(c) => Some(format!("{owner}{c}")),
            Part::SearchString { bar } => Some(format!("{bar}{owner}SearchString")),
            Part::ViewStatus => Some(format!("{owner}ViewStatus")),
            Part::TumblerItem(i) => Some(format!("{owner}_cmd_0_{i}")),
            Part::TumblerColumn(i) => Some(format!("{owner}_col_{i}")),
            Part::SpinButtons => Some(format!("{owner}_SB")),
            Part::More
            | Part::PagesPreloader
            | Part::RadioItem(_)
            | Part::RadioGroup
            | Part::Tab(_) => None,
        },
    }
}

pub fn record_kind(kind: &BoxKind) -> &'static str {
    match kind {
        BoxKind::Field | BoxKind::Column => "field",
        BoxKind::CheckBox => "checkbox",
        BoxKind::Toggle => "frameTumbler",
        BoxKind::ToggleItem => "tumblerItem",
        BoxKind::RadioColumn => "col",
        BoxKind::RadioItem => "radio",
        BoxKind::RadioGroup => "radioGroup",
        BoxKind::Switch => "switch",
        BoxKind::Label | BoxKind::LabelField => "staticText",
        BoxKind::Button => "frameButton",
        BoxKind::CommandButton => "press",
        BoxKind::CommandBar => "frameCommand",
        BoxKind::Grid => "grid",
        BoxKind::SpreadsheetField => "spreadsheet",
        BoxKind::Tabs => "frameTabs",
        BoxKind::ViewStatus => "train",
        BoxKind::SpinButtons => "btn",
        BoxKind::Preloader => "preloader",
        BoxKind::Group => "group",
        BoxKind::Page => "page",
        BoxKind::Picture => "picture",
        BoxKind::Stub(_) => "stub",
    }
}

fn button_name(b: FieldButton) -> &'static str {
    match b {
        FieldButton::Choice => "CB",
        FieldButton::DropList => "DLB",
        FieldButton::Clear => "CLR",
        FieldButton::Open => "OB",
        FieldButton::SpinUp => "SBI",
        FieldButton::SpinDown => "SBD",
    }
}

fn near(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= TOLERANCE)
}

pub fn run_record(rec: &Record, source_xml: &str) -> RecordResult {
    run_record_with(rec, source_xml, &ItemState::default())
}

pub fn run_record_with(rec: &Record, source_xml: &str, state: &ItemState) -> RecordResult {
    let counted = rec.items.values().filter(|i| i.kind != "preloader").count();
    let fail = |msg: String| RecordResult {
        id: rec.id.clone(),
        pass: false,
        items: counted,
        items_ok: 0,
        diffs: vec![Diff::Error(msg)],
    };
    let form = match form_model::parse_form_xml(source_xml) {
        Ok(f) => f,
        Err(e) => return fail(format!("parse: {e:?}")),
    };
    let interface = if rec.iface == "taxi" {
        crate::Interface::Taxi
    } else {
        crate::Interface::V85
    };
    let content = Size {
        w: rec.content[2],
        h: rec.content[3],
    };
    let opened = rec.opened_at.map_or(content, |o| {
        let b = content_box(Size { w: o[0], h: o[1] });
        Size { w: b.w, h: b.h }
    });
    let input = LayoutInput {
        form: &form,
        types: &form_model::NoConfig,
        interface,
        content,
        opened,
        state,
        window: crate::WindowMode::Workspace,
        viewport: Size {
            w: rec.viewport[0],
            h: rec.viewport[1],
        },
    };
    match layout(&input) {
        Ok(l) => compare(rec, &l),
        Err(e) => fail(format!("layout: {e:?}")),
    }
}

pub fn compare(rec: &Record, layout: &Layout) -> RecordResult {
    let mut ours: HashMap<String, &crate::ItemBox> = HashMap::new();
    for b in &layout.boxes {
        if let Some(k) = record_key(&b.key) {
            ours.entry(k).or_insert(b);
        }
    }
    let (mut items, mut items_ok) = (0, 0);
    let mut diffs = Vec::new();
    for (key, it) in &rec.items {
        if it.kind == "preloader" {
            continue;
        }
        items += 1;
        let before = diffs.len();
        match ours.get(key) {
            None => diffs.push(Diff::Missing(key.clone())),
            Some(b) => compare_item(key, it, b, rec.grids.get(key), &mut diffs),
        }
        if diffs.len() == before {
            items_ok += 1;
        }
    }
    RecordResult {
        id: rec.id.clone(),
        pass: diffs.is_empty(),
        items,
        items_ok,
        diffs,
    }
}

fn flag_names(b: &crate::ItemBox) -> Vec<&'static str> {
    let f = &b.flags;
    let mut v = Vec::new();
    for (on, name) in [
        (f.align_right, "taR"),
        (f.read_only, "fieldReadOnly"),
        (f.disabled, "fieldDisabled"),
    ] {
        if on {
            v.push(name);
        }
    }
    v.sort_unstable();
    v
}

fn compare_item(
    key: &str,
    it: &RecItem,
    b: &crate::ItemBox,
    grid: Option<&RecGrid>,
    out: &mut Vec<Diff>,
) {
    let theirs_hidden = it.flags.iter().any(|f| f == "hidden");
    if b.hidden != theirs_hidden {
        out.push(Diff::Hidden {
            key: key.into(),
            ours: b.hidden,
        });
        return;
    }
    if theirs_hidden {
        return;
    }
    let k = record_kind(&b.kind);
    if k != it.kind {
        out.push(Diff::Kind {
            key: key.into(),
            ours: k.into(),
            theirs: it.kind.clone(),
        });
        return;
    }
    let mut coord = |what: &'static str, ours: Rect, theirs: [f32; 4]| {
        if !near(ours.to_array(), theirs) {
            out.push(Diff::Coord {
                key: key.into(),
                what,
                ours: ours.to_array(),
                theirs,
            });
        }
    };
    coord("r", b.rect, it.r);
    match (&b.title, &it.title) {
        (Some(t), Some(rt)) => {
            coord("title.r", t.rect, rt.r);
            if t.text != rt.text {
                out.push(Diff::Text {
                    key: key.into(),
                    what: "title.text",
                    ours: t.text.clone(),
                    theirs: rt.text.clone(),
                });
            }
        }
        (None, Some(rt)) => out.push(Diff::Text {
            key: key.into(),
            what: "title",
            ours: String::new(),
            theirs: rt.text.clone(),
        }),
        (Some(t), None) => out.push(Diff::Text {
            key: key.into(),
            what: "title",
            ours: t.text.clone(),
            theirs: String::new(),
        }),
        (None, None) => {}
    }
    match (&b.input, &it.input) {
        (Some(i), Some(ri)) => {
            if !near(i.to_array(), *ri) {
                out.push(Diff::Coord {
                    key: key.into(),
                    what: "input",
                    ours: i.to_array(),
                    theirs: *ri,
                });
            }
        }
        (None, Some(_)) => out.push(Diff::Text {
            key: key.into(),
            what: "input",
            ours: "none".into(),
            theirs: "some".into(),
        }),
        (Some(_), None) => out.push(Diff::Text {
            key: key.into(),
            what: "input",
            ours: "some".into(),
            theirs: "none".into(),
        }),
        (None, None) => {}
    }
    let ob: Vec<&str> = b.buttons.iter().map(|x| button_name(*x)).collect();
    let tb: Vec<&str> = it.buttons.iter().map(String::as_str).collect();
    if ob != tb {
        out.push(Diff::Text {
            key: key.into(),
            what: "buttons",
            ours: ob.join(","),
            theirs: tb.join(","),
        });
    }
    let of = flag_names(b);
    let mut tf: Vec<&str> = it
        .flags
        .iter()
        .map(String::as_str)
        .filter(|f| !matches!(*f, "focus" | "hidden" | "fieldHidden"))
        .collect();
    tf.sort_unstable();
    if of != tf {
        out.push(Diff::Text {
            key: key.into(),
            what: "flags",
            ours: of.join(","),
            theirs: tf.join(","),
        });
    }
    if matches!(
        b.kind,
        BoxKind::Button | BoxKind::CommandButton | BoxKind::Label
    ) && let Some(t) = &it.text
        && !same_text(b.text.as_deref().unwrap_or(""), t)
    {
        out.push(Diff::Text {
            key: key.into(),
            what: "text",
            ours: b.text.clone().unwrap_or_default(),
            theirs: t.clone(),
        });
    }
    if let Some(g) = grid {
        compare_grid(key, g, b, out);
    }
}

const RECORD_TEXT_CUT: usize = 80;

fn same_text(ours: &str, theirs: &str) -> bool {
    if theirs.chars().count() == RECORD_TEXT_CUT {
        ours.chars().take(RECORD_TEXT_CUT).eq(theirs.chars())
    } else {
        ours == theirs
    }
}

fn compare_grid(key: &str, g: &RecGrid, b: &crate::ItemBox, out: &mut Vec<Diff>) {
    let mut bad = |what: String| {
        out.push(Diff::Grid {
            key: key.into(),
            what,
        })
    };
    let Some(ours) = &b.grid else {
        bad("no grid".into());
        return;
    };
    if !near(b.rect.to_array(), g.r) {
        bad(format!("r {:?} vs {:?}", b.rect.to_array(), g.r));
    }
    if (ours.inner.w - g.content[0]).abs() > TOLERANCE
        || (ours.inner.h - g.content[1]).abs() > TOLERANCE
    {
        bad(format!(
            "content {:?} vs {:?}",
            [ours.inner.w, ours.inner.h],
            g.content
        ));
    }
    let on: Vec<&str> = ours.columns.iter().map(|c| c.name.as_str()).collect();
    let tn: Vec<&str> = g.head.iter().map(|c| c.0.as_str()).collect();
    if on != tn {
        bad(format!("columns {on:?} vs {tn:?}"));
    } else {
        for (c, t) in ours.columns.iter().zip(&g.head) {
            if (c.x - t.1).abs() > TOLERANCE || (c.w - t.2).abs() > TOLERANCE {
                bad(format!(
                    "column {} {:?} vs {:?}",
                    c.name,
                    [c.x, c.w],
                    [t.1, t.2]
                ));
            }
        }
    }
    if let Some(min) = g.row_h.iter().copied().reduce(f32::min)
        && (ours.row_h - min).abs() > TOLERANCE
    {
        bad(format!("row_h {} vs {min}", ours.row_h));
    }
}
