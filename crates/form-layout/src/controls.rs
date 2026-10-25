use form_model::{Item, ItemKind};
use ui_style::FontRole;

use super::{Ctx, Role};
use crate::{BoxFlags, BoxKey, BoxKind, ItemBox, Part, Rect};

fn standard_command(cmd: &str) -> Option<&str> {
    let (head, id) = cmd.rsplit_once('.')?;
    (head.ends_with(".StandardCommand") || head == "StandardCommand")
        .then_some(id)
        .filter(|i| !i.is_empty())
}

const MAX_LINES: usize = 10_000;
const TOGGLE_OFF: &str = "Нет";
const TOGGLE_ON: &str = "Да";
fn more_text() -> &'static str {
    ui_style::captions::command_text("More", "ru").unwrap_or("More")
}
pub(crate) const MORE_ARROW: f32 = 12.0;

pub(super) struct Segments {
    texts: Vec<String>,
    widths: Vec<f32>,
}

pub(super) struct Radios {
    texts: Vec<String>,
    cols: usize,
    col_x: Vec<f32>,
    pub rows: usize,
    pub w: f32,
    pub h: f32,
}

pub(super) enum Look {
    Plain,
    Switch,
    Tumbler(Segments),
    Radios(Radios),
}

impl Look {
    pub(super) fn size(&self, r: &crate::rules::Rules) -> Option<(f32, f32)> {
        match self {
            Look::Plain => None,
            Look::Switch => Some((r.switch_w, r.switch_h)),
            Look::Tumbler(s) => Some((s.widths.iter().sum(), r.toggle_h)),
            Look::Radios(g) => Some((g.w, g.h)),
        }
    }
}

pub(super) struct Deco {
    pub text: String,
    pub w: f32,
    pub h: f32,
    pub stretch: bool,
}

impl Ctx<'_> {
    fn text_w(&self, s: &str) -> f32 {
        self.m.width(s, FontRole::Text)
    }

    pub(super) fn deco(&self, it: &Item, avail: f32) -> Deco {
        let r = self.r;
        let p = &it.props;
        let text = p
            .title
            .as_ref()
            .and_then(|t| t.any())
            .unwrap_or("")
            .to_string();
        let avail = avail.max(0.0);
        let whole = p.auto_max_width == Some(false);
        let limit = if whole {
            p.max_width
                .filter(|m| *m > 0)
                .map_or(avail, |m| (r.char_w * m.min(10_000) as f32).min(avail))
        } else {
            r.decor_max_w.min(avail)
        };
        let stretch = p.horizontal_stretch == Some(true);
        let own = p.width.filter(|w| *w > 0);
        let wrap_w = own.map_or(limit, |c| r.char_w * c.min(10_000) as f32);
        let lines = self.m.wrap(&text, FontRole::Text, wrap_w, MAX_LINES);
        let text_w = lines.iter().map(|l| self.text_w(l)).fold(0.0f32, f32::max);
        let link = p.hyperlink == Some(true);
        let link_w = if link { 2.0 * r.link_frame } else { 0.0 };
        let w = if own.is_some() {
            wrap_w
        } else if whole {
            (self.text_w(text.trim_end()) + link_w).min(limit)
        } else if stretch {
            limit
        } else {
            (text_w + link_w).min(limit)
        };
        let h = match p.height.filter(|h| *h > 1) {
            Some(n) => r.decor_line_h * n.min(10_000) as f32 + r.decor_rows_extra,
            None => {
                r.decor_line_h * lines.len().max(1) as f32
                    + if link { 2.0 * r.link_frame } else { 0.0 }
            }
        };
        Deco {
            text,
            w,
            h,
            stretch,
        }
    }

    pub(super) fn decoration(&mut self, it: &Item, x0: f32, avail: f32, y: f32, d: u16) -> f32 {
        let dc = self.deco(it, avail);
        let mut b = ItemBox::new(
            BoxKey::Item(it.name.clone()),
            BoxKind::Label,
            Rect::new(x0, y, dc.w, dc.h),
            d,
        );
        b.flags = BoxFlags {
            hyperlink: it.props.hyperlink == Some(true),
            ..BoxFlags::default()
        };
        b.text = Some(dc.text);
        self.boxes.push(b);
        dc.h
    }

    pub(super) fn picture_size(&self, it: &Item) -> (f32, f32) {
        let r = self.r;
        let w = it
            .props
            .width
            .filter(|w| *w > 0)
            .map_or(r.picture_size, |c| r.char_w * c.min(10_000) as f32);
        let h = it
            .props
            .height
            .filter(|h| *h > 0)
            .map_or(r.picture_size, |n| r.picture_row_h * n.min(10_000) as f32);
        (w, h)
    }

    pub(super) fn picture(&mut self, it: &Item, x0: f32, y: f32, d: u16) -> f32 {
        let (w, h) = self.picture_size(it);
        self.boxes.push(ItemBox::new(
            BoxKey::Item(it.name.clone()),
            BoxKind::Picture,
            Rect::new(x0, y, w, h),
            d,
        ));
        h
    }

    pub(super) fn button_text(&self, it: &Item) -> String {
        if let Some(t) = it.props.title.as_ref().and_then(|t| t.any()) {
            return t.to_string();
        }
        let Some(cmd) = it.props.command_name.as_deref() else {
            return String::new();
        };
        let name = cmd.rsplit('.').next().unwrap_or(cmd);
        if let Some(id) = standard_command(cmd) {
            return ui_style::captions::command_text(id, "ru")
                .unwrap_or(name)
                .to_string();
        }
        self.input
            .form
            .commands
            .iter()
            .find(|c| c.name == name && cmd.starts_with("Form.Command."))
            .and_then(|c| c.title.as_ref().and_then(|t| t.any()))
            .unwrap_or(name)
            .to_string()
    }

    pub(super) fn warn_once(&mut self, msg: String) {
        if !self.warnings.contains(&msg) {
            self.warnings.push(msg);
        }
    }

    fn warn_unknown_caption(&mut self, it: &Item) {
        if it.props.title.as_ref().and_then(|t| t.any()).is_some() {
            return;
        }
        let Some(id) = it.props.command_name.as_deref().and_then(standard_command) else {
            return;
        };
        if ui_style::captions::caption(id).is_none() {
            self.warn_once(format!("{}: no caption for standard command {id}", it.name));
        }
    }

    fn command_picture(&self, it: &Item) -> bool {
        if it.props.picture.is_some() {
            return true;
        }
        let Some(cmd) = it.props.command_name.as_deref() else {
            return false;
        };
        self.input
            .form
            .commands
            .iter()
            .any(|c| cmd == format!("Form.Command.{}", c.name) && c.picture.is_some())
    }

    pub(super) fn plain_button(&self, it: &Item) -> bool {
        it.kind == ItemKind::Button
            && it.props.button_type.as_deref() != Some("Hyperlink")
            && it.props.width.is_none_or(|w| w == 0)
            && it.props.horizontal_stretch != Some(true)
    }

    pub(super) fn button_nat(&self, it: &Item) -> f32 {
        let text = self.button_text(it);
        let tw = self.text_w(&text);
        if it.props.button_type.as_deref() == Some("Hyperlink") {
            tw
        } else {
            tw + self.r.button_pad
        }
    }

    pub(super) fn button(
        &mut self,
        it: &Item,
        x0: f32,
        avail: f32,
        y: f32,
        d: u16,
        col: f32,
    ) -> f32 {
        let r = self.r;
        let link = it.props.button_type.as_deref() == Some("Hyperlink");
        self.warn_unknown_caption(it);
        let text = self.button_text(it);
        let nat = self.button_nat(it);
        let w = if let Some(c) = it.props.width.filter(|w| *w > 0) {
            r.char_w * c.min(10_000) as f32
        } else if it.props.horizontal_stretch == Some(true) && !link {
            avail.max(0.0)
        } else if link {
            nat
        } else {
            col.max(nat)
        };
        let h = if link { r.link_h } else { r.button_h };
        let mut b = ItemBox::new(
            BoxKey::Item(it.name.clone()),
            BoxKind::Button,
            Rect::new(x0, y, w, h),
            d,
        );
        b.flags = BoxFlags {
            default_button: it.props.default_button == Some(true),
            hyperlink: link,
            disabled: it.props.enabled == Some(false),
            ..BoxFlags::default()
        };
        b.text = Some(text);
        self.boxes.push(b);
        h
    }

    fn press(&self, it: &Item) -> (f32, String) {
        let r = self.r;
        let text = self.button_text(it);
        let pic = self.command_picture(it);
        let rep = it.props.representation.as_deref();
        let show_text = !(text.is_empty() || pic && rep == Some("Picture"));
        let show_pic = pic && rep != Some("Text");
        let w = match (show_text, show_pic) {
            (false, true) | (false, false) => r.cmd_picture_w,
            (true, false) => self.text_w(&text) + r.cmd_pad,
            (true, true) => self.text_w(&text) + r.cmd_pad + r.cmd_picture_extra,
        };
        (w, text)
    }

    pub(super) fn bar_buttons(
        &mut self,
        items: &[Item],
        mut x: f32,
        y: f32,
        d: u16,
        limit: f32,
    ) -> f32 {
        for c in items {
            if self.is_hidden(c) {
                self.hidden_subtree(c, d);
                continue;
            }
            match c.kind {
                ItemKind::Button | ItemKind::Popup => {
                    if c.kind == ItemKind::Popup {
                        self.warnings
                            .push(format!("{}: popup drawn as a button", c.name));
                    }
                    self.warn_unknown_caption(c);
                    let (w, text) = self.press(c);
                    if x + w > limit + 0.5 {
                        self.hidden_subtree(c, d);
                        continue;
                    }
                    let mut b = ItemBox::new(
                        BoxKey::Item(c.name.clone()),
                        BoxKind::CommandButton,
                        Rect::new(x, y, w, self.r.button_h),
                        d,
                    );
                    b.flags.disabled = c.props.enabled == Some(false);
                    b.text = Some(text.replace(' ', "\u{a0}"));
                    self.boxes.push(b);
                    x += w + self.r.cmd_gap;
                }
                ItemKind::ButtonGroup if d < u16::MAX => {
                    x = self.bar_buttons(&c.children, x, y, d + 1, limit);
                }
                _ => {}
            }
        }
        x
    }

    pub(super) fn std_buttons(
        &mut self,
        cmds: &[form_model::stdcmd::StdCommand],
        mut x: f32,
        y: f32,
        d: u16,
        limit: f32,
    ) -> f32 {
        let start = x;
        for c in cmds.iter().filter(|c| c.on_bar) {
            if c.new_group && x > start && self.generated() {
                x += self.r.table_move_gap - self.r.cmd_gap;
            }
            let text = match ui_style::captions::command_text(c.id, "ru") {
                Some(t) => t,
                None => {
                    self.warn_once(format!(
                        "{}: no caption for standard command {}",
                        c.item, c.id
                    ));
                    c.item.as_str()
                }
            };
            let w = if c.picture_only {
                self.r.cmd_picture_w
            } else {
                let role = if c.default && self.generated() {
                    FontRole::TextBold
                } else {
                    FontRole::Text
                };
                self.m.width(text, role) + self.r.cmd_pad
            };
            let mut b = ItemBox::new(
                BoxKey::Item(c.item.clone()),
                BoxKind::CommandButton,
                Rect::new(x, y, w, self.r.button_h),
                d,
            );
            if x + w > limit + 0.5 {
                b.rect = Rect::default();
                b.hidden = true;
                self.boxes.push(b);
                continue;
            }
            if !c.picture_only {
                b.text = Some(text.replace(' ', "\u{a0}"));
                b.flags.default_button = c.default;
            }
            self.boxes.push(b);
            x += w + self.r.cmd_gap;
        }
        x
    }

    pub(super) fn bar_natural(&self, items: &[Item]) -> f32 {
        let mut w = 0.0f32;
        for c in items {
            if self.is_hidden(c) {
                continue;
            }
            match c.kind {
                ItemKind::Button | ItemKind::Popup => w += self.press(c).0 + self.r.cmd_gap,
                ItemKind::ButtonGroup => w += self.bar_natural(&c.children),
                _ => {}
            }
        }
        w
    }

    pub(super) fn command_bar(&mut self, it: &Item, x0: f32, avail: f32, y: f32, d: u16) -> f32 {
        let h = self.r.cmd_bar_h;
        self.boxes.push(ItemBox::new(
            BoxKey::Item(it.name.clone()),
            BoxKind::CommandBar,
            Rect::new(x0, y, avail, h),
            d,
        ));
        self.bar_buttons(&it.children, x0, y, d.saturating_add(1), x0 + avail);
        h
    }

    pub(super) fn more_width(&self) -> f32 {
        self.text_w(more_text()) + self.r.cmd_pad + MORE_ARROW
    }

    pub(super) fn more_button(&mut self, bar: &str, x0: f32, w: f32, y: f32) {
        if ui_style::captions::caption("More").is_none() {
            self.warn_once(format!("{bar}: no caption for standard command More"));
        }
        let bw = self.more_width();
        let mut b = ItemBox::new(
            BoxKey::Part {
                owner: bar.to_string(),
                part: Part::More,
            },
            BoxKind::CommandButton,
            Rect::new(x0 + (w - bw).max(0.0), y, bw, self.r.button_h),
            1,
        );
        b.text = Some(more_text().to_string());
        self.boxes.push(b);
    }

    fn segments(&self, texts: Vec<String>) -> Segments {
        let r = self.r;
        let n = texts.len();
        let mut widths: Vec<f32> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| {
                self.text_w(t) + r.toggle_pad + if i + 1 == n { r.toggle_end_pad } else { 0.0 }
            })
            .collect();
        let sum: f32 = widths.iter().sum();
        if let Some(last) = widths.last_mut().filter(|_| sum < r.toggle_min_w) {
            *last += r.toggle_min_w - sum;
        }
        Segments { texts, widths }
    }

    fn radio_item_count(&self, it: &Item) -> usize {
        it.props.choice_list.len()
            + self
                .input
                .state
                .choices
                .get(&it.name)
                .map_or(0, |e| e.len())
    }

    fn radio_items(&self, it: &Item) -> Vec<String> {
        let mut v: Vec<String> = it
            .props
            .choice_list
            .iter()
            .map(|l| l.any().unwrap_or("").to_string())
            .collect();
        if let Some(extra) = self.input.state.choices.get(&it.name) {
            v.extend(extra.iter().cloned());
        }
        if v.is_empty() {
            v.push(String::new());
        }
        v
    }

    fn radio_label_widths(&self, texts: &[String]) -> Vec<f32> {
        texts
            .iter()
            .map(|t| self.r.radio_label_dx + self.text_w(t))
            .collect()
    }

    fn radios_with(&self, texts: Vec<String>, widths: &[f32], columns: Option<u32>) -> Radios {
        let r = self.r;
        let n = texts.len().max(1);
        let cols = columns
            .filter(|c| *c > 0)
            .map_or(n, |c| (c as usize).min(n));
        let rows = n.div_ceil(cols);
        let mut col_w = vec![0.0f32; cols];
        for (i, w) in widths.iter().enumerate() {
            col_w[i % cols] = col_w[i % cols].max(*w);
        }
        let mut col_x = Vec::with_capacity(cols);
        let mut x = 0.0f32;
        for w in &col_w {
            col_x.push(x);
            x += w + r.radio_col_gap;
        }
        let w = (x - r.radio_col_gap).max(0.0);
        let h = r.radio_row_h * rows as f32 + r.radio_row_gap * (rows - 1) as f32;
        Radios {
            texts,
            cols,
            col_x,
            rows,
            w,
            h,
        }
    }

    fn radio_width_in(&self, widths: &[f32], cols: usize) -> f32 {
        let mut col_w = vec![0.0f32; cols];
        for (i, w) in widths.iter().enumerate() {
            col_w[i % cols] = col_w[i % cols].max(*w);
        }
        let gaps = self.r.radio_col_gap * cols.saturating_sub(1) as f32;
        (col_w.iter().sum::<f32>() + gaps).max(0.0)
    }

    pub(super) fn radio_wraps(&self, it: &Item) -> bool {
        it.kind == ItemKind::RadioButtonField
            && it.props.radio_button_type.as_deref() != Some("Tumbler")
            && it.props.columns_count.is_none_or(|c| c == 0)
            && self.radio_item_count(it) > 1
    }

    pub(super) fn look(&self, it: &Item, ro: Role) -> Look {
        self.look_in(it, ro, f32::MAX)
    }

    pub(super) fn look_in(&self, it: &Item, ro: Role, room: f32) -> Look {
        match ro {
            Role::Check => match it.props.check_box_type.as_deref() {
                Some("Tumbler") => {
                    Look::Tumbler(self.segments(vec![TOGGLE_OFF.into(), TOGGLE_ON.into()]))
                }
                Some("Switcher") => Look::Switch,
                _ => Look::Plain,
            },
            Role::Radio => {
                let texts = self.radio_items(it);
                if it.props.radio_button_type.as_deref() == Some("Tumbler") {
                    Look::Tumbler(self.segments(texts))
                } else {
                    let widths = self.radio_label_widths(&texts);
                    let columns = if self.radio_wraps(it) && room.is_finite() {
                        let n = texts.len();
                        let mut first_row = Vec::with_capacity(n + 1);
                        first_row.push(0.0f32);
                        for w in &widths {
                            first_row.push(first_row.last().copied().unwrap_or(0.0) + w);
                        }
                        let gap = self.r.radio_col_gap;
                        Some(
                            (1..=n)
                                .rev()
                                .find(|&c| {
                                    first_row.get(c).copied().unwrap_or(0.0)
                                        + gap * c.saturating_sub(1) as f32
                                        <= room
                                        && self.radio_width_in(&widths, c) <= room
                                })
                                .unwrap_or(1) as u32,
                        )
                    } else {
                        it.props.columns_count
                    };
                    Look::Radios(self.radios_with(texts, &widths, columns))
                }
            }
            _ => Look::Plain,
        }
    }

    pub(super) fn radio_rows(&self, it: &Item) -> usize {
        match self.look(it, Role::Radio) {
            Look::Radios(g) => g.rows,
            _ => 1,
        }
    }

    pub(super) fn look_parts(&mut self, idx: usize, it: &Item, look: &Look) {
        let r = self.r;
        let d = self.boxes[idx].depth;
        let area = self.boxes[idx].rect;
        match look {
            Look::Plain | Look::Switch => {}
            Look::Tumbler(s) => {
                let mut x = area.x;
                for (i, (t, w)) in s.texts.iter().zip(&s.widths).enumerate() {
                    let rect = Rect::new(x, area.y, *w, area.h);
                    let part = |p| BoxKey::Part {
                        owner: it.name.clone(),
                        part: p,
                    };
                    self.boxes.push(ItemBox::new(
                        part(Part::TumblerColumn(i as u32)),
                        BoxKind::RadioColumn,
                        rect,
                        d,
                    ));
                    let mut b = ItemBox::new(
                        part(Part::TumblerItem(i as u32)),
                        BoxKind::ToggleItem,
                        rect,
                        d,
                    );
                    b.text = Some(t.clone());
                    self.boxes.push(b);
                    x += w;
                }
            }
            Look::Radios(g) => {
                let top = area.y;
                let circle = |i: usize| {
                    let (c, row) = (i % g.cols, i / g.cols);
                    Rect::new(
                        area.x + g.col_x[c] + r.radio_dxy,
                        top + row as f32 * (r.radio_row_h + r.radio_row_gap) + r.radio_dxy,
                        r.radio_circle,
                        r.radio_circle,
                    )
                };
                self.boxes[idx].kind = BoxKind::RadioItem;
                self.boxes[idx].rect = circle(0);
                self.boxes[idx].text = Some(g.texts[0].clone());
                for (i, t) in g.texts.iter().enumerate().skip(1) {
                    let mut b = ItemBox::new(
                        BoxKey::Part {
                            owner: it.name.clone(),
                            part: Part::RadioItem(i as u32),
                        },
                        BoxKind::RadioItem,
                        circle(i),
                        d,
                    );
                    b.text = Some(t.clone());
                    self.boxes.push(b);
                }
                self.boxes.push(ItemBox::new(
                    BoxKey::Part {
                        owner: it.name.clone(),
                        part: Part::RadioGroup,
                    },
                    BoxKind::RadioGroup,
                    Rect::new(area.x, top, g.w, g.h),
                    d,
                ));
            }
        }
    }
}
