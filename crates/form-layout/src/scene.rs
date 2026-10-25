use form_model::{FormDesc, Item, PictureRef};
use ui_style::{FontRole, Metrics};

use crate::{BoxKey, BoxKind, FieldButton, ItemBox, Layout, Part, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Panel {
    FormCommandBar,
    CommandBar,
    GroupFrame,
    GroupStrong,
    TabsFrame,
    Tab { active: bool },
    GridFrame,
    GridHeader,
    GridHeaderCell,
    Field { read_only: bool, disabled: bool },
    FieldButton,
    Button { default: bool },
    CommandButton,
    CheckBox,
    Radio,
    ToggleSegment { first: bool },
    Switch,
    Placeholder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TextRole {
    Normal,
    Bold,
    Hyperlink,
    Hint,
    Header,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum Prim {
    Panel(Panel),
    Text {
        text: String,
        role: TextRole,
        align: Align,
    },
    Picture {
        reference: PictureRef,
    },
    Glyph(FieldButton),
    MoreArrow,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Node {
    pub rect: Rect,
    pub prim: Prim,
    pub item: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Scene {
    pub w: f32,
    pub h: f32,
    pub nodes: Vec<Node>,
}

const PICTURE: f32 = 16.0;
const BUTTON_PAD: f32 = 6.0;
const RADIO_LABEL_DX: f32 = 21.0;

struct Out<'a> {
    nodes: Vec<Node>,
    form: &'a FormDesc,
}

impl Out<'_> {
    fn push(&mut self, rect: Rect, prim: Prim, item: &Option<String>) {
        self.nodes.push(Node {
            rect,
            prim,
            item: item.clone(),
        });
    }

    fn text(
        &mut self,
        rect: Rect,
        text: &str,
        role: TextRole,
        align: Align,
        item: &Option<String>,
    ) {
        if !text.is_empty() {
            self.push(
                rect,
                Prim::Text {
                    text: text.to_string(),
                    role,
                    align,
                },
                item,
            );
        }
    }

    fn picture_of(&self, it: &Item) -> Option<PictureRef> {
        if let Some(p) = &it.props.picture {
            return Some(p.clone());
        }
        let cmd = it.props.command_name.as_deref()?;
        let name = cmd.strip_prefix("Form.Command.")?;
        self.form
            .commands
            .iter()
            .find(|c| c.name == name)
            .and_then(|c| c.picture.clone())
    }
}

fn item_name(k: &BoxKey) -> &str {
    match k {
        BoxKey::Item(n) => n,
        BoxKey::Part { owner, .. } => owner,
    }
}

fn title_text(b: &ItemBox, m: Option<&Metrics>) -> Option<(Rect, String)> {
    let t = b.title.as_ref()?;
    let text = if t.lines > 1 && !t.text.contains('\n') {
        match m {
            Some(m) => m
                .wrap(&t.text, FontRole::Text, t.rect.w + 1.0, t.lines as usize)
                .join("\n"),
            None => t.text.clone(),
        }
    } else {
        t.text.clone()
    };
    Some((t.rect, text))
}

fn button_content(
    o: &mut Out,
    rect: Rect,
    text: &str,
    pic: Option<PictureRef>,
    role: TextRole,
    item: &Option<String>,
) {
    match pic {
        Some(p) => {
            let py = rect.y + ((rect.h - PICTURE) / 2.0).max(0.0);
            if text.is_empty() {
                let px = rect.x + ((rect.w - PICTURE) / 2.0).max(0.0);
                o.push(
                    Rect::new(px, py, PICTURE, PICTURE),
                    Prim::Picture { reference: p },
                    item,
                );
            } else {
                o.push(
                    Rect::new(rect.x + BUTTON_PAD, py, PICTURE, PICTURE),
                    Prim::Picture { reference: p },
                    item,
                );
                let tx = rect.x + BUTTON_PAD + PICTURE + 4.0;
                o.text(
                    Rect::new(tx, rect.y, (rect.x + rect.w - tx).max(0.0), rect.h),
                    text,
                    role,
                    Align::Left,
                    item,
                );
            }
        }
        None => o.text(rect, text, role, Align::Center, item),
    }
}

pub fn scene(form: &FormDesc, layout: &Layout) -> Scene {
    let m = Metrics::get(crate::Interface::Taxi);
    let mut o = Out {
        nodes: Vec::new(),
        form,
    };
    let mut boxes: Vec<&ItemBox> = layout
        .boxes
        .iter()
        .filter(|b| !b.hidden && !matches!(b.kind, BoxKind::Preloader | BoxKind::Column))
        .collect();
    boxes.sort_by_key(|b| b.depth);
    let tabs_current: std::collections::BTreeMap<&str, Option<&str>> = layout
        .boxes
        .iter()
        .filter(|b| b.kind == BoxKind::Tabs)
        .map(|b| (item_name(&b.key), b.text.as_deref()))
        .collect();
    for b in boxes {
        let name = item_name(&b.key);
        let item = Some(name.to_string());
        let it = form.find(name);
        let r = b.rect;
        let text = b.text.as_deref().unwrap_or("");
        match &b.kind {
            BoxKind::CommandBar => {
                let top = form
                    .auto_command_bar
                    .as_ref()
                    .is_some_and(|c| c.name == name);
                o.push(
                    r,
                    Prim::Panel(if top {
                        Panel::FormCommandBar
                    } else {
                        Panel::CommandBar
                    }),
                    &item,
                );
            }
            BoxKind::CommandButton => {
                o.push(r, Prim::Panel(Panel::CommandButton), &item);
                let pic = match (&b.key, it) {
                    (BoxKey::Item(_), Some(i))
                        if i.props.representation.as_deref() != Some("Text") =>
                    {
                        o.picture_of(i)
                    }
                    (
                        BoxKey::Part {
                            part: Part::TableCommand(cmd),
                            ..
                        },
                        _,
                    ) => table_command_picture(cmd),
                    _ => None,
                };
                if matches!(
                    &b.key,
                    BoxKey::Part {
                        part: Part::More,
                        ..
                    }
                ) {
                    let aw = crate::flow::MORE_ARROW;
                    let tr = Rect::new(r.x, r.y, (r.w - aw).max(0.0), r.h);
                    o.text(tr, text, TextRole::Normal, Align::Center, &item);
                    let ar = Rect::new((r.x + r.w - aw - 2.0).max(r.x), r.y, aw, r.h);
                    o.push(ar, Prim::MoreArrow, &item);
                } else {
                    button_content(&mut o, r, text, pic, TextRole::Normal, &item);
                }
            }
            BoxKind::Group => {
                let repr = it.and_then(|i| i.props.representation.as_deref());
                match repr {
                    Some("StrongSeparation") => o.push(r, Prim::Panel(Panel::GroupStrong), &item),
                    Some("NormalSeparation") => o.push(r, Prim::Panel(Panel::GroupFrame), &item),
                    _ => {}
                }
                if let Some((tr, t)) = title_text(b, m) {
                    o.text(tr, &t, TextRole::Bold, Align::Left, &item);
                }
            }
            BoxKind::Field => {
                let shown: Vec<FieldButton> = b
                    .buttons
                    .iter()
                    .copied()
                    .filter(|g| *g != FieldButton::SpinDown)
                    .collect();
                let slot = 21.0;
                let text_w = (r.w - slot * shown.len() as f32).max(0.0);
                let frame = Rect::new(r.x, r.y, text_w, r.h);
                o.push(
                    frame,
                    Prim::Panel(Panel::Field {
                        read_only: b.flags.read_only,
                        disabled: b.flags.disabled,
                    }),
                    &item,
                );
                if let Some(inp) = b.input {
                    let hint = it
                        .and_then(|i| i.props.input_hint.as_ref())
                        .and_then(|h| h.any());
                    if let Some(h) = hint {
                        o.text(inp, h, TextRole::Hint, Align::Left, &item);
                    }
                }
                let mut x = frame.x + frame.w;
                for g in shown {
                    let bx = Rect::new(x, r.y, slot, r.h);
                    o.push(bx, Prim::Panel(Panel::FieldButton), &item);
                    o.push(bx, Prim::Glyph(g), &item);
                    x += slot;
                }
                if let Some((tr, t)) = title_text(b, m) {
                    o.text(tr, &t, TextRole::Normal, Align::Left, &item);
                }
            }
            BoxKind::LabelField => {
                if let Some((tr, t)) = title_text(b, m) {
                    o.text(tr, &t, TextRole::Normal, Align::Left, &item);
                }
            }
            BoxKind::CheckBox => {
                let sq = Rect::new(r.x, r.y, r.w.min(PICTURE), r.h.min(PICTURE));
                o.push(sq, Prim::Panel(Panel::CheckBox), &item);
                if let Some((tr, t)) = title_text(b, m) {
                    o.text(tr, &t, TextRole::Normal, Align::Left, &item);
                }
            }
            BoxKind::Switch => {
                o.push(r, Prim::Panel(Panel::Switch), &item);
                if let Some((tr, t)) = title_text(b, m) {
                    o.text(tr, &t, TextRole::Normal, Align::Left, &item);
                }
            }
            BoxKind::Toggle => {
                if let Some((tr, t)) = title_text(b, m) {
                    o.text(tr, &t, TextRole::Normal, Align::Left, &item);
                }
            }
            BoxKind::ToggleItem => {
                let first = matches!(
                    &b.key,
                    BoxKey::Part {
                        part: Part::TumblerItem(0),
                        ..
                    }
                );
                o.push(r, Prim::Panel(Panel::ToggleSegment { first }), &item);
                o.text(r, text, TextRole::Normal, Align::Center, &item);
            }
            BoxKind::RadioItem => {
                o.push(r, Prim::Panel(Panel::Radio), &item);
                let w = m.map_or(0.0, |m| m.width(text, FontRole::Text));
                o.text(
                    Rect::new(r.x + RADIO_LABEL_DX, r.y - 1.0, w + 1.0, r.h + 2.0),
                    text,
                    TextRole::Normal,
                    Align::Left,
                    &item,
                );
                if let Some((tr, t)) = title_text(b, m) {
                    o.text(tr, &t, TextRole::Normal, Align::Left, &item);
                }
            }
            BoxKind::Label => {
                let role = if b.flags.hyperlink {
                    TextRole::Hyperlink
                } else {
                    TextRole::Normal
                };
                o.text(r, text, role, Align::Left, &item);
            }
            BoxKind::Button => {
                let pic = it.and_then(|i| o.picture_of(i));
                if b.flags.hyperlink {
                    o.text(r, text, TextRole::Hyperlink, Align::Left, &item);
                } else {
                    o.push(
                        r,
                        Prim::Panel(Panel::Button {
                            default: b.flags.default_button,
                        }),
                        &item,
                    );
                    let role = if b.flags.default_button {
                        TextRole::Bold
                    } else {
                        TextRole::Normal
                    };
                    button_content(&mut o, r, text, pic, role, &item);
                }
            }
            BoxKind::Picture => {
                if let Some(p) = it.and_then(|i| i.props.picture.clone()) {
                    o.push(r, Prim::Picture { reference: p }, &item);
                }
            }
            BoxKind::Grid => {
                o.push(r, Prim::Panel(Panel::GridFrame), &item);
                if let Some(g) = &b.grid {
                    let head = Rect::new(
                        r.x + 1.0,
                        r.y + 1.0,
                        g.inner.w.min((r.w - 2.0).max(0.0)),
                        g.header_h,
                    );
                    if g.header_h > 0.0 {
                        o.push(head, Prim::Panel(Panel::GridHeader), &item);
                    }
                    let limit = head.x + head.w;
                    for c in g.columns.iter().filter(|_| g.header_h > 0.0) {
                        let x = head.x + c.x;
                        if x >= limit {
                            continue;
                        }
                        let cell = Rect::new(x, head.y, c.w.min(limit - x), g.header_h);
                        o.push(cell, Prim::Panel(Panel::GridHeaderCell), &item);
                        let t = &c.title;
                        let inner =
                            Rect::new(cell.x + 6.0, cell.y, (cell.w - 12.0).max(0.0), cell.h);
                        o.text(inner, t, TextRole::Header, Align::Left, &item);
                    }
                }
            }
            BoxKind::SpreadsheetField => {
                o.push(r, Prim::Panel(Panel::GridFrame), &item);
                if let Some((tr, t)) = title_text(b, m) {
                    o.text(tr, &t, TextRole::Normal, Align::Left, &item);
                }
            }
            BoxKind::Tabs => {
                let repr = it.and_then(|i| i.props.pages_representation.as_deref());
                match repr {
                    Some("None") => {}
                    Some("TabsOnBottom") => {
                        let strip = layout
                            .boxes
                            .iter()
                            .filter(|t| {
                                matches!(&t.key, BoxKey::Part { owner, part: Part::Tab(_) } if owner == name)
                            })
                            .map(|t| t.rect.h)
                            .fold(0.0f32, f32::max);
                        let fr = Rect::new(r.x, r.y, r.w, (r.h - strip).max(0.0));
                        o.push(fr, Prim::Panel(Panel::TabsFrame), &item);
                    }
                    _ => o.push(r, Prim::Panel(Panel::TabsFrame), &item),
                }
            }
            BoxKind::Page => {
                let cur = tabs_current.get(name).copied().flatten();
                let active = cur == Some(text);
                o.push(r, Prim::Panel(Panel::Tab { active }), &item);
                o.text(r, text, TextRole::Normal, Align::Center, &item);
            }
            BoxKind::Stub(kind) => {
                o.push(r, Prim::Panel(Panel::Placeholder), &item);
                o.text(r, kind, TextRole::Hint, Align::Center, &item);
            }
            BoxKind::ViewStatus
            | BoxKind::SpinButtons
            | BoxKind::RadioGroup
            | BoxKind::RadioColumn => {}
            BoxKind::Preloader | BoxKind::Column => {}
        }
    }
    let mut w = layout.content.w;
    let mut h = layout.content.h;
    for n in &o.nodes {
        w = w.max(n.rect.x + n.rect.w);
        h = h.max(n.rect.y + n.rect.h);
    }
    Scene {
        w: w.max(0.0),
        h: h.max(0.0),
        nodes: o.nodes,
    }
}

fn table_command_picture(cmd: &str) -> Option<PictureRef> {
    match cmd {
        "ПереместитьВверх" | "MoveUp" => Some(PictureRef::Std("MoveUp".into())),
        "ПереместитьВниз" | "MoveDown" => Some(PictureRef::Std("MoveDown".into())),
        _ => None,
    }
}
