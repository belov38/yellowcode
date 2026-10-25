use form_model::{GroupKind, HAlign, Item, ItemKind, MAX_DEPTH, Spacing};
use ui_style::FontRole;

use super::nu::RowKid;
use super::{Ctx, Role, role};
use crate::title::Pos;
use crate::width::{apply_button_props, field_width, label_width, natural_width, type_width};
use crate::{BoxKey, BoxKind, ItemBox, Rect, TitleBox};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ChildrenAlign {
    Auto,
    None,
    TitlesRight,
    ItemsRight,
}

impl ChildrenAlign {
    fn parse(s: Option<&str>) -> ChildrenAlign {
        match s {
            Some("None") => ChildrenAlign::None,
            Some("ItemsLeftTitlesRight") => ChildrenAlign::TitlesRight,
            Some("ItemsRightTitlesLeft") => ChildrenAlign::ItemsRight,
            _ => ChildrenAlign::Auto,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct VOpts {
    pub children_align: ChildrenAlign,
    pub halign: HAlign,
    pub honor_own: bool,
    pub row: bool,
    pub title_col: f32,
}

impl Default for VOpts {
    fn default() -> VOpts {
        VOpts {
            children_align: ChildrenAlign::Auto,
            halign: HAlign::Left,
            honor_own: true,
            row: false,
            title_col: 0.0,
        }
    }
}

impl VOpts {
    pub(super) fn single() -> VOpts {
        VOpts {
            honor_own: false,
            ..VOpts::default()
        }
    }

    pub(super) fn row() -> VOpts {
        VOpts {
            honor_own: false,
            row: true,
            ..VOpts::default()
        }
    }

    pub(super) fn align_of(&self, it: &Item) -> HAlign {
        let own = if self.honor_own {
            it.props.group_horizontal_align
        } else {
            None
        };
        match own {
            Some(a @ (HAlign::Left | HAlign::Center | HAlign::Right)) => a,
            _ => match self.halign {
                HAlign::Auto => HAlign::Left,
                a => a,
            },
        }
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub(super) struct Meas {
    pub nat: f32,
    pub tight: f32,
    pub max: f32,
    pub left_col: f32,
    pub fixed: f32,
    pub stretch: bool,
    pub greedy: bool,
    pub bar: bool,
    pub wrap: bool,
}

impl Ctx<'_> {
    fn open_delta(&self) -> f32 {
        let (o, c) = (self.input.opened.w, self.input.content.w);
        if o.is_finite() && o > 0.0 { o - c } else { 0.0 }
    }

    pub(super) fn open_avail(&self, avail: f32) -> f32 {
        (avail + self.open_delta()).max(0.0)
    }

    pub(super) fn field_size(&self, it: &Item, ro: Role, room: f32) -> (f32, bool, f32) {
        let kind = self.data_kind(it);
        let mut tw = type_width(&kind);
        match ro {
            Role::Input => apply_button_props(&mut tw, &it.props),
            _ => tw.buttons.clear(),
        }
        let r = self.r;
        match ro {
            Role::Input => {
                let (w, st) = field_width(&it.props, &tw, room, r);
                (w, st, natural_width(&it.props, &tw, r).min(w))
            }
            Role::Label => {
                let (w, st) = label_width(&it.props, &tw, room, r);
                let chars = it.props.width.filter(|w| *w > 0).unwrap_or(tw.chars);
                (w, st, (r.char_w * chars as f32).min(w))
            }
            Role::Check | Role::Radio => {
                let w = self
                    .look_in(it, ro, room)
                    .size(r)
                    .map_or(r.check_size, |s| s.0);
                (w, false, w)
            }
        }
    }

    pub(super) fn measure(&mut self, it: &Item, oa: f32, depth: usize) -> Meas {
        if depth > MAX_DEPTH || self.is_hidden(it) {
            return Meas::default();
        }
        let r = self.r;
        if let Some(ro) = role(it) {
            let mark = self.warnings.len();
            let plan = self.title_plan(it, ro);
            self.warnings.truncate(mark);
            let (fw, stretch, nat_fw) = self.field_size(it, ro, f32::MAX);
            let greedy = fw > 1.0e6;
            let fw = if greedy {
                r.char_w * r.stretch_chars as f32 + r.field_base
            } else {
                fw
            };
            let has = plan.has_text();
            let wrap = ro == Role::Radio && self.radio_wraps(it);
            let (fw_min, fw) = if wrap {
                (self.field_size(it, ro, 0.0).0, fw)
            } else {
                (nat_fw, fw)
            };
            let (nat, fixed, left_col) = match plan.pos {
                Pos::Left if has => (
                    plan.w + r.title_gap + fw,
                    plan.w + r.title_gap,
                    plan.w + r.title_gap,
                ),
                Pos::Right if has => (fw + r.title_gap + plan.w, 0.0, 0.0),
                Pos::Top if has => (plan.w.max(fw), 0.0, 0.0),
                _ => (fw, 0.0, 0.0),
            };
            return Meas {
                nat,
                tight: nat - fw + fw_min,
                max: nat + if stretch { r.max_extra } else { 0.0 },
                left_col,
                fixed,
                stretch,
                greedy,
                bar: false,
                wrap,
            };
        }
        if it.kind == ItemKind::UsualGroup {
            return self.measure_group(it, oa, depth);
        }
        let own = match it.kind {
            ItemKind::LabelDecoration => {
                let dc = self.deco(it, oa);
                Some((dc.w, dc.stretch))
            }
            ItemKind::Button => Some((
                it.props
                    .width
                    .filter(|w| *w > 0)
                    .map_or_else(|| self.button_nat(it), |c| r.char_w * c.min(10_000) as f32),
                it.props.horizontal_stretch == Some(true),
            )),
            ItemKind::PictureDecoration | ItemKind::PictureField => {
                Some((self.picture_size(it).0, false))
            }
            ItemKind::CommandBar => {
                let w = self.bar_natural(&it.children).min(oa);
                let stretch = it.props.horizontal_stretch == Some(true);
                return Meas {
                    nat: w,
                    tight: w,
                    max: w,
                    stretch,
                    greedy: stretch,
                    bar: true,
                    ..Meas::default()
                };
            }
            _ => None,
        };
        if let Some((w, stretch)) = own {
            return Meas {
                nat: w,
                tight: w,
                max: w,
                stretch,
                ..Meas::default()
            };
        }
        Meas {
            nat: oa,
            tight: oa,
            max: oa,
            stretch: true,
            ..Meas::default()
        }
    }

    fn measure_group(&mut self, it: &Item, oa: f32, depth: usize) -> Meas {
        let indent = self.indent(it);
        let (horiz, kids, metas) = self.orient(it, oa, depth, false);
        let hgap =
            self.r.h_spacing[it.props.horizontal_spacing.unwrap_or(Spacing::Single) as usize];
        let any_stretch = metas.iter().any(|m| m.stretch);
        let any_greedy = metas.iter().any(|m| m.greedy);
        let (mut nat, mut max, mut tight) = (0.0f32, 0.0f32, 0.0f32);
        let fixed;
        if horiz {
            let gaps = hgap * kids.len().saturating_sub(1) as f32;
            nat = metas.iter().map(|m| m.nat).sum::<f32>() + gaps;
            max = metas.iter().map(|m| m.max).sum::<f32>() + gaps;
            tight = metas.iter().map(|m| m.tight).sum::<f32>() + gaps;
            fixed = metas.iter().map(|m| m.fixed).sum::<f32>();
        } else {
            let own =
                ChildrenAlign::parse(it.props.children_align.as_deref()) == ChildrenAlign::None;
            let opts = VOpts {
                halign: it.props.horizontal_align.unwrap_or(HAlign::Left),
                ..VOpts::default()
            };
            let shared: Vec<bool> = kids
                .iter()
                .map(|k| opts.align_of(k) == HAlign::Left)
                .collect();
            let col = if own {
                0.0
            } else {
                metas
                    .iter()
                    .zip(&shared)
                    .filter(|(_, s)| **s)
                    .map(|(m, _)| m.left_col)
                    .fold(0.0, f32::max)
            };
            for (m, sh) in metas.iter().zip(&shared) {
                let c = if own || !sh || m.left_col <= 0.0 {
                    m.left_col
                } else {
                    col
                };
                nat = nat.max(c + m.nat - m.left_col);
                max = max.max(c + m.max - m.left_col);
                tight = tight.max(c + m.tight - m.left_col);
            }
            fixed = col;
        }
        Meas {
            nat: nat + indent,
            tight: tight + indent,
            max: max + indent,
            left_col: 0.0,
            fixed,
            stretch: any_stretch,
            greedy: any_greedy,
            bar: false,
            wrap: metas.iter().any(|m| m.wrap),
        }
    }

    pub(super) fn indent(&self, it: &Item) -> f32 {
        if it.props.representation.as_deref() == Some("StrongSeparation") {
            self.r.repr_strong_dx
        } else {
            0.0
        }
    }

    pub(super) fn orient<'i>(
        &mut self,
        it: &'i Item,
        oa: f32,
        depth: usize,
        page: bool,
    ) -> (bool, Vec<&'i Item>, Vec<Meas>) {
        let indent = self.indent(it);
        let kids: Vec<&Item> = it.children.iter().filter(|c| !self.is_hidden(c)).collect();
        let metas: Vec<Meas> = kids
            .iter()
            .map(|c| self.measure(c, (oa - indent).max(0.0), depth + 1))
            .collect();
        let horiz = match it.props.group {
            Some(GroupKind::Vertical) => false,
            None if page => false,
            Some(GroupKind::Horizontal | GroupKind::AlwaysHorizontal) => true,
            Some(GroupKind::HorizontalIfPossible) | None => {
                let hgap = self.r.h_spacing
                    [it.props.horizontal_spacing.unwrap_or(Spacing::Single) as usize];
                let total = metas.iter().map(|m| m.max).sum::<f32>()
                    + hgap * kids.len().saturating_sub(1) as f32;
                !kids.is_empty() && total <= (oa - indent).max(0.0)
            }
        };
        (horiz, kids, metas)
    }

    pub(super) fn align_region(&mut self, items: &[Item], avail_w: f32, depth: usize) -> f32 {
        let oa = self.open_avail(avail_w);
        let mut w = 0.0f32;
        for it in items {
            w = w.max(self.measure(it, oa, depth).nat);
        }
        w.min(avail_w)
    }

    pub(super) fn group(
        &mut self,
        it: &Item,
        x0: f32,
        avail_w: f32,
        y: f32,
        depth: usize,
        d: u16,
    ) -> f32 {
        self.group_with(it, x0, avail_w, y, depth, d, true)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn group_with(
        &mut self,
        it: &Item,
        x0: f32,
        avail_w: f32,
        y: f32,
        depth: usize,
        d: u16,
        titles: bool,
    ) -> f32 {
        let r = self.r;
        let first_col = self.row_col.take();
        let oa = self.open_avail(avail_w);
        let (horiz, kids, metas) = self.orient(it, oa, depth + 1, !titles);
        let props = &it.props;
        let title = props.title.as_ref().and_then(|t| t.any()).unwrap_or("");
        let show = titles && props.show_title != Some(false) && !title.is_empty();
        let repr = props.representation.as_deref();
        let nu_title = (props.united == Some(false) && !horiz && titles)
            .then(|| self.nu_title(it))
            .flatten();
        let mut off = if show { r.group_title_h } else { 0.0 };
        if let Some(t) = &nu_title {
            off = t.h + self.v_gap(props.vertical_spacing.unwrap_or(Spacing::Single));
        }
        if repr == Some("NormalSeparation") {
            off += r.repr_sep_dy;
        } else if show && repr == Some("None") {
            off += r.repr_none_dy;
        }
        let cx = x0 + self.indent(it);
        let cavail = (avail_w - (cx - x0)).max(0.0);
        let idx = self.boxes.len();
        let mut gb = ItemBox::new(
            BoxKey::Item(it.name.clone()),
            BoxKind::Group,
            Rect::new(x0, y, avail_w, 0.0),
            d,
        );
        if let (true, Some(t)) = (show, &nu_title) {
            gb.title = Some(t.title_box(x0, y));
        } else if show {
            let w = self.m.width(title, FontRole::TextBold);
            gb.title = Some(TitleBox {
                rect: Rect::new(x0, y, w, r.title_h),
                text: title.to_string(),
                lines: 1,
            });
        }
        self.boxes.push(gb);
        let inner = if horiz {
            let hgap = r.h_spacing[props.horizontal_spacing.unwrap_or(Spacing::Single) as usize];
            let metas: Vec<Meas> = if props.united == Some(false) {
                metas
                    .iter()
                    .map(|m| Meas {
                        nat: m.tight,
                        stretch: false,
                        greedy: false,
                        ..*m
                    })
                    .collect()
            } else {
                metas
            };
            let (kids, mut metas) = self.open_row(&kids, &metas, oa, depth + 1, d);
            let mut title_col = 0.0;
            if let (Some(c0), Some(m), Some(RowKid::Item(k))) =
                (first_col, metas.first_mut(), kids.first())
                && c0 > 0.0
                && role(k).is_some()
            {
                let delta = (c0 + r.title_gap - m.left_col).max(0.0);
                m.nat += delta;
                m.tight += delta;
                m.max += delta;
                m.fixed += delta;
                title_col = c0;
            }
            self.horizontal(
                &kids,
                &metas,
                cx,
                cavail,
                y + off,
                hgap,
                depth + 1,
                title_col,
                d + 1,
            )
        } else {
            let gap = props.vertical_spacing.unwrap_or(Spacing::Single);
            let opts = VOpts {
                children_align: ChildrenAlign::parse(props.children_align.as_deref()),
                halign: props.horizontal_align.unwrap_or(HAlign::Left),
                ..VOpts::default()
            };
            self.vertical(&it.children, cx, cavail, y + off, gap, depth + 1, &opts)
        };
        if horiz {
            let hidden: Vec<&Item> = it.children.iter().filter(|c| self.is_hidden(c)).collect();
            for c in hidden {
                self.hidden_subtree(c, d + 1);
            }
        }
        let h = if kids.is_empty() && inner == 0.0 {
            0.0
        } else {
            off + inner
        };
        self.boxes[idx].rect.h = h;
        h
    }

    #[allow(clippy::too_many_arguments)]
    fn horizontal(
        &mut self,
        kids: &[RowKid<'_>],
        metas: &[Meas],
        x0: f32,
        avail_w: f32,
        y0: f32,
        hgap: f32,
        depth: usize,
        title_col: f32,
        d: u16,
    ) -> f32 {
        let n = kids.len();
        let gaps = hgap * n.saturating_sub(1) as f32;
        let mut alloc: Vec<f32> = metas.iter().map(|m| m.nat).collect();
        let total = alloc.iter().sum::<f32>() + gaps;
        let greedy = metas.iter().filter(|m| m.greedy).count();
        if greedy > 0 {
            let need: f32 = metas
                .iter()
                .zip(&alloc)
                .map(|(m, a)| if m.greedy { m.tight.max(m.fixed) } else { *a })
                .sum::<f32>()
                + gaps;
            if need > avail_w {
                let pick = |m: &Meas| m.bar && !m.greedy;
                shrink(&mut alloc, metas, need - avail_w, pick, |_| 0.0);
            }
            let others: f32 = metas
                .iter()
                .zip(&alloc)
                .map(|(m, a)| if m.greedy { m.fixed } else { *a })
                .sum();
            let mut left = (avail_w - others - gaps).max(0.0);
            let mut rest = greedy;
            for (a, m) in alloc.iter_mut().zip(metas).filter(|(_, m)| m.greedy) {
                let share = (left / rest as f32).round();
                *a = m.fixed + share;
                left -= share;
                rest -= 1;
            }
        } else if total > avail_w {
            let deficit = shrink(&mut alloc, metas, total - avail_w, |m| m.wrap, |m| m.tight);
            let mut left = shrink(&mut alloc, metas, deficit, |m| m.stretch, |m| m.tight);
            if left > 0.01 {
                left = shrink(&mut alloc, metas, left, |m| m.bar, |_| 0.0);
            }
            if left > 0.01 {
                shrink(&mut alloc, metas, left, |m| m.stretch, |m| m.fixed);
            }
        }
        let mut x = x0;
        let mut h = 0.0f32;
        for (i, (kid, a)) in kids.iter().zip(&alloc).enumerate() {
            let it = match kid {
                RowKid::Item(it) => it,
                RowKid::Title(g, t) => {
                    h = h.max(self.row_title(g, t, x, y0, d));
                    x += a + hgap;
                    continue;
                }
            };
            let one = VOpts {
                title_col: if i == 0 { title_col } else { 0.0 },
                ..VOpts::row()
            };
            let ch = self.vertical(
                std::slice::from_ref(*it),
                x,
                *a,
                y0,
                Spacing::Single,
                depth,
                &one,
            );
            h = h.max(ch);
            x += a + hgap;
        }
        h
    }
}

fn shrink(
    alloc: &mut [f32],
    metas: &[Meas],
    mut deficit: f32,
    pick: impl Fn(&Meas) -> bool,
    lower: impl Fn(&Meas) -> f32,
) -> f32 {
    for _ in 0..metas.len() + 1 {
        if deficit <= 0.01 {
            break;
        }
        let weight: f32 = alloc
            .iter()
            .zip(metas)
            .filter(|(a, m)| pick(m) && **a > lower(m))
            .map(|(_, m)| m.nat)
            .sum();
        if weight <= 0.0 {
            break;
        }
        let want = deficit;
        for (a, m) in alloc.iter_mut().zip(metas) {
            if pick(m) && *a > lower(m) {
                let take = (want * m.nat / weight).min(*a - lower(m));
                *a -= take;
                deficit -= take;
            }
        }
    }
    deficit.max(0.0)
}
