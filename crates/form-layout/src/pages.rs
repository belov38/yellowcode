use form_model::{Item, ItemKind};
use ui_style::FontRole;

use super::Ctx;
use crate::{BoxKey, BoxKind, ItemBox, Part, Rect};

impl Ctx<'_> {
    fn right_edge(&self, from: usize) -> f32 {
        self.boxes[from..]
            .iter()
            .filter(|b| !b.hidden && b.kind != BoxKind::Group)
            .flat_map(|b| {
                let t = b.title.as_ref().map(|t| t.rect.x + t.rect.w);
                [Some(b.rect.x + b.rect.w), t]
            })
            .flatten()
            .fold(f32::MIN, f32::max)
    }

    pub(super) fn pages(
        &mut self,
        it: &Item,
        x0: f32,
        avail: f32,
        y: f32,
        depth: usize,
        d: u16,
    ) -> f32 {
        let r = self.r;
        let (tabs, bottom) = match it.props.pages_representation.as_deref() {
            Some("None") => (false, false),
            Some("TabsOnBottom") => (true, true),
            None | Some("TabsOnTop" | "Auto") => (true, false),
            Some(other) => {
                self.warnings.push(format!(
                    "{}: pages representation {other} laid out as tabs on top",
                    it.name
                ));
                (true, false)
            }
        };
        let (pad_x, near, far, strip) = if tabs {
            (
                r.pages_pad_x,
                r.pages_pad_near,
                r.pages_pad_far,
                r.pages_tab_h,
            )
        } else {
            (0.0, 0.0, 0.0, 0.0)
        };
        let (top_off, bottom_off) = match (tabs, bottom) {
            (false, _) => (0.0, 0.0),
            (true, false) => (strip + near, far),
            (true, true) => (far, near + strip),
        };
        let pages: Vec<&Item> = it
            .children
            .iter()
            .filter(|c| c.kind == ItemKind::Page)
            .collect();
        let shown: Vec<&Item> = pages
            .iter()
            .copied()
            .filter(|p| !self.is_hidden(p))
            .collect();
        let wanted = self.input.state.current_page.get(&it.name);
        let current = shown
            .iter()
            .position(|p| Some(&p.name) == wanted)
            .or(if shown.is_empty() { None } else { Some(0) });

        let idx = self.boxes.len();
        let mut fb = ItemBox::new(
            BoxKey::Item(it.name.clone()),
            BoxKind::Tabs,
            Rect::new(x0, y, avail, 0.0),
            d,
        );
        fb.text = current.and_then(|i| {
            shown[i]
                .props
                .title
                .as_ref()
                .and_then(|t| t.any())
                .map(str::to_string)
        });
        self.boxes.push(fb);

        let cx = x0 + pad_x;
        let cavail = (avail - 2.0 * pad_x).max(0.0);
        let cy = y + top_off;
        let (mut max_h, mut right) = (0.0f32, cx);
        for (i, page) in shown.iter().enumerate() {
            let mark = self.boxes.len();
            let wmark = self.warnings.len();
            let h = self.group_with(page, cx, cavail, cy, depth + 1, d + 1, false);
            if current == Some(i) {
                right = right.max(self.right_edge(mark));
            } else {
                right = right.max(self.right_edge(mark));
                self.boxes.truncate(mark);
                self.warnings.truncate(wmark);
                for c in &page.children {
                    self.hidden_subtree(c, d + 1);
                }
            }
            max_h = max_h.max(h);
        }
        let hidden_pages: Vec<&Item> = pages
            .iter()
            .copied()
            .filter(|p| self.is_hidden(p))
            .collect();
        for page in hidden_pages {
            for c in &page.children {
                self.hidden_subtree(c, d + 1);
            }
        }

        let mut strip_w = 0.0f32;
        let mut tab_boxes = Vec::new();
        for (i, page) in shown.iter().enumerate() {
            let text = page
                .props
                .title
                .as_ref()
                .and_then(|t| t.any())
                .unwrap_or("");
            let w = self.m.width(text, FontRole::Text) + 2.0 * r.tab_pad;
            tab_boxes.push((i, w, text.to_string(), page.name.clone()));
            strip_w += w;
        }
        let content_w = (right - cx).max(0.0);
        let w = if it.props.horizontal_stretch == Some(true) {
            avail
        } else {
            (content_w + 2.0 * pad_x).max(if tabs { strip_w } else { 0.0 })
        };
        let h = top_off + max_h + bottom_off;
        self.boxes[idx].rect = Rect::new(x0, y, w, h);
        let limit = x0 + w - pad_x;
        for b in self.boxes[idx + 1..]
            .iter_mut()
            .filter(|b| !b.hidden && b.kind == BoxKind::Group)
        {
            let over = b.rect.x + b.rect.w - limit;
            if over > 0.0 {
                b.rect.w = (b.rect.w - over).max(0.0);
            }
        }
        if tabs {
            let ty = if bottom { y + h - strip } else { y };
            let mut tx = x0;
            for (i, tw, text, _name) in tab_boxes {
                let mut b = ItemBox::new(
                    BoxKey::Part {
                        owner: it.name.clone(),
                        part: Part::Tab(i as u32),
                    },
                    BoxKind::Page,
                    Rect::new(tx, ty, tw, strip),
                    d,
                );
                b.text = Some(text);
                self.boxes.push(b);
                tx += tw;
            }
        }
        self.boxes.push({
            let mut b = ItemBox::new(
                BoxKey::Part {
                    owner: it.name.clone(),
                    part: Part::PagesPreloader,
                },
                BoxKind::Preloader,
                Rect::default(),
                d,
            );
            b.hidden = true;
            b
        });
        h
    }
}
