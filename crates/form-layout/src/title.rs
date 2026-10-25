use form_model::TitleLocation;
use ui_style::{FontRole, Metrics};

use crate::rules::Rules;
use crate::{Rect, TitleBox};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pos {
    Left,
    Top,
    Right,
    None,
}

pub(crate) fn preferred(loc: Option<TitleLocation>, multi_line: bool) -> (Pos, bool) {
    match loc {
        None | Some(TitleLocation::Auto) => (if multi_line { Pos::Top } else { Pos::Left }, false),
        Some(TitleLocation::Left) => (Pos::Left, false),
        Some(TitleLocation::Top) => (Pos::Top, false),
        Some(TitleLocation::Right) => (Pos::Right, false),
        Some(TitleLocation::None) => (Pos::None, false),
        Some(TitleLocation::Bottom) => (Pos::Left, true),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Plan {
    pub pos: Pos,
    pub raw: String,
    pub text: String,
    pub w: f32,
    pub h: f32,
    pub lines: u32,
    pub explicit_h: bool,
}

impl Plan {
    pub(crate) fn has_text(&self) -> bool {
        !self.raw.is_empty()
    }

    pub(crate) fn at(&self, pos: Pos, title_height: Option<u32>, m: &Metrics, r: &Rules) -> Plan {
        plan(&self.raw, pos, title_height, m, r)
    }
}

pub(crate) fn plan(raw: &str, pos: Pos, title_height: Option<u32>, m: &Metrics, r: &Rules) -> Plan {
    let explicit_h = title_height.is_some_and(|h| h > 1);
    if raw.is_empty() || pos == Pos::None {
        return Plan {
            pos: if raw.is_empty() && pos != Pos::Left {
                Pos::None
            } else {
                pos
            },
            raw: raw.to_string(),
            text: String::new(),
            w: 0.0,
            h: 0.0,
            lines: 0,
            explicit_h: false,
        };
    }
    let shown = if pos == Pos::Right {
        raw.to_string()
    } else {
        format!("{raw}:")
    };
    let mut lines = vec![shown.clone()];
    let breakable = shown.contains(' ');
    if pos == Pos::Left && breakable && m.width(&shown, FontRole::Text) > r.title_one_line_max {
        lines = m.wrap(&shown, FontRole::Text, r.title_wrap_w, r.title_max_lines);
    }
    let w = lines
        .iter()
        .map(|l| m.width(l, FontRole::Text))
        .fold(0.0f32, f32::max);
    let want = if pos == Pos::Left {
        title_height.unwrap_or(1).max(1) as usize
    } else {
        1
    };
    let n = lines.len().max(want).min(r.title_max_lines.max(1));
    Plan {
        pos,
        raw: raw.to_string(),
        text: lines.join(if explicit_h { "\n" } else { " " }),
        w,
        h: r.title_h * n as f32,
        lines: n as u32,
        explicit_h,
    }
}

pub(crate) fn fitted(raw: &str, w: f32, m: &Metrics, r: &Rules) -> Plan {
    let shown = format!("{raw}:");
    let lines = m.wrap(&shown, FontRole::Text, w.max(1.0), r.title_max_lines);
    let n = lines.len().max(1);
    Plan {
        pos: Pos::Left,
        raw: raw.to_string(),
        text: lines.join(" "),
        w,
        h: r.title_h * n as f32,
        lines: n as u32,
        explicit_h: false,
    }
}

pub(crate) fn title_box(plan: &Plan, rect: Rect) -> TitleBox {
    TitleBox {
        rect,
        text: plan.text.clone(),
        lines: plan.lines,
    }
}
