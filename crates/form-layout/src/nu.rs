use form_model::{GroupKind, Item, ItemKind, MAX_DEPTH, Spacing};
use ui_style::FontRole;

use super::group::{Meas, VOpts};
use super::{Ctx, Place, Role, WHOLE_ROOM, role};
use crate::title::{self, Pos};
use crate::{BoxKey, BoxKind, ItemBox, Rect, TitleBox};

#[derive(Clone, Copy)]
struct Mode {
    top: bool,
    p: f32,
}

pub(super) enum Cell<'i> {
    Field {
        it: &'i Item,
        ro: Role,
        plan: title::Plan,
        fw: f32,
        stretch: bool,
    },
    Title {
        g: &'i Item,
        t: NuTitle,
    },
    Other {
        it: &'i Item,
        w: f32,
    },
}

#[derive(Clone)]
pub(super) struct NuTitle {
    text: String,
    pub w: f32,
    pub h: f32,
    lines: u32,
}

impl NuTitle {
    pub(super) fn title_box(&self, x: f32, y: f32) -> TitleBox {
        TitleBox {
            rect: Rect::new(x, y, self.w, self.h),
            text: self.text.clone(),
            lines: self.lines,
        }
    }
}

pub(super) enum RowKid<'i> {
    Item(&'i Item),
    Title(&'i Item, NuTitle),
}

#[derive(Clone, Copy, Default)]
struct Pair {
    x: f32,
    tw: f32,
    fw: f32,
}

impl Pair {
    fn field_x(&self, gap: f32) -> f32 {
        self.x
            + self.tw
            + if self.fw > 0.0 && self.tw > 0.0 {
                gap
            } else {
                0.0
            }
    }
}

enum Row<'i> {
    Hidden(&'i Item),
    Plain {
        it: &'i Item,
        ro: Role,
        plan: title::Plan,
        stretched: Option<f32>,
    },
    Group(&'i Item),
    United(&'i Item),
    Nu {
        g: &'i Item,
        title: Option<NuTitle>,
        cells: Vec<Cell<'i>>,
        lead: usize,
    },
    Other(&'i Item),
}

pub(super) struct NuPlan<'i> {
    rows: Vec<Row<'i>>,
    c0: f32,
    c1x: f32,
    e: f32,
    pairs: Vec<Pair>,
    right: f32,
    mode: Mode,
    cell_stretch: bool,
}

impl Mode {
    fn natural() -> Mode {
        Mode {
            top: false,
            p: f32::MAX,
        }
    }
}

impl Ctx<'_> {
    pub(super) fn repr_pads(&self, it: &Item) -> (f32, f32) {
        if it.kind != ItemKind::UsualGroup || self.is_hidden(it) {
            return (0.0, 0.0);
        }
        let d = self.r.repr_sep_dy;
        match it.props.representation.as_deref() {
            Some("NormalSeparation") => (d, d),
            Some("StrongSeparation") => (0.0, d),
            _ => (0.0, 0.0),
        }
    }

    fn squeeze(&self, w: f32, nat: f32, p: f32) -> f32 {
        if p == f32::MAX {
            return w;
        }
        let nat = nat.min(w);
        let min = self.r.nu_shrink_min.min(nat);
        if p <= 1.0 {
            min + p.max(0.0) * (nat - min)
        } else {
            (nat + (p - 1.0) * (nat + self.r.nu_grow_bias)).min(w)
        }
    }

    fn fit_p(
        &mut self,
        items: &[Item],
        x0: f32,
        avail_w: f32,
        top: bool,
        depth: usize,
        limit: f32,
    ) -> Option<f32> {
        let fits = |cx: &mut Self, p: f32| {
            cx.nu_plan(items, x0, avail_w, Mode { top, p }, depth).right <= limit + 0.01
        };
        if !fits(self, 0.0) {
            return None;
        }
        let (mut lo, mut hi) = (0.0f32, 20.0f32);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if fits(self, mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Some(lo)
    }

    pub(super) fn is_nu(&self, it: &Item) -> bool {
        it.kind == ItemKind::UsualGroup
            && it.props.united == Some(false)
            && it.props.group != Some(GroupKind::Vertical)
            && !self.is_hidden(it)
    }

    pub(super) fn nu_title(&self, g: &Item) -> Option<NuTitle> {
        if g.props.show_title == Some(false) {
            return None;
        }
        let t = g
            .props
            .title
            .as_ref()
            .and_then(|t| t.any())
            .filter(|t| !t.is_empty())?;
        let (r, m) = (self.r, self.m);
        let k = r.nu_title_scale;
        let mut lines = vec![t.to_string()];
        if m.width(t, FontRole::Text) * k > r.nu_title_wrap_w {
            lines = m.wrap(t, FontRole::Text, r.nu_title_wrap_w / k, r.title_max_lines);
        }
        let w = lines
            .iter()
            .map(|l| m.width(l, FontRole::Text) * k)
            .fold(0.0f32, f32::max);
        let n = lines.len().max(1);
        Some(NuTitle {
            text: lines.join(" "),
            w,
            h: (r.nu_title_line_h * n as f32).round(),
            lines: n as u32,
        })
    }

    fn cell_width(&self, it: &Item, ro: Role, p: f32) -> (f32, bool) {
        let (w, stretch, tight) = self.field_size(it, ro, f32::MAX);
        let w = if w > WHOLE_ROOM { tight } else { w };
        if stretch {
            (self.squeeze(w, tight, p), true)
        } else {
            (w, false)
        }
    }

    fn nu_cells<'i>(&mut self, g: &'i Item, mode: Mode, oa: f32, depth: usize) -> Vec<Cell<'i>> {
        let mut out = Vec::new();
        if depth > MAX_DEPTH {
            return out;
        }
        for c in &g.children {
            if self.is_hidden(c) {
                continue;
            }
            if self.is_nu(c) {
                if let Some(t) = self.nu_title(c) {
                    out.push(Cell::Title { g: c, t });
                }
                out.extend(self.nu_cells(c, mode, oa, depth + 1));
            } else if let Some(ro) = role(c) {
                let mut plan = self.title_plan(c, ro);
                if mode.top && plan.pos == Pos::Left && plan.has_text() {
                    plan = plan.at(Pos::Top, c.props.title_height, self.m, self.r);
                }
                let (fw, stretch) = self.cell_width(c, ro, mode.p);
                out.push(Cell::Field {
                    it: c,
                    ro,
                    plan,
                    fw,
                    stretch,
                });
            } else {
                let w = self.measure(c, oa, depth + 1).nat;
                out.push(Cell::Other { it: c, w });
            }
        }
        out
    }

    fn nu_plan<'i>(
        &mut self,
        items: &'i [Item],
        x0: f32,
        avail_w: f32,
        mode: Mode,
        depth: usize,
    ) -> NuPlan<'i> {
        let r = self.r;
        let top = mode.top;
        let mut p = NuPlan {
            rows: Vec::new(),
            c0: 0.0,
            c1x: x0,
            e: x0,
            pairs: Vec::new(),
            right: x0,
            mode,
            cell_stretch: false,
        };
        if depth > MAX_DEPTH {
            return p;
        }
        let oa = self.open_avail(avail_w);
        let mut c0 = 0.0f32;
        for it in items {
            if self.is_hidden(it) {
                p.rows.push(Row::Hidden(it));
                continue;
            }
            if let Some(ro) = role(it) {
                let mut plan = self.title_plan(it, ro);
                if top && plan.pos == Pos::Left && plan.has_text() {
                    plan = plan.at(Pos::Top, it.props.title_height, self.m, r);
                }
                if plan.pos == Pos::Left && plan.has_text() {
                    c0 = c0.max(plan.w);
                }
                p.rows.push(Row::Plain {
                    it,
                    ro,
                    plan,
                    stretched: None,
                });
                continue;
            }
            if it.kind != ItemKind::UsualGroup {
                p.rows.push(Row::Other(it));
                continue;
            }
            if self.is_nu(it) {
                let title = self.nu_title(it);
                let cells = self.nu_cells(it, mode, oa, depth + 1);
                let lead = usize::from(
                    title.is_none() && matches!(cells.first(), Some(Cell::Field { .. })),
                );
                if let (1, Some(Cell::Field { plan, .. })) = (lead, cells.first())
                    && plan.pos == Pos::Left
                    && plan.has_text()
                {
                    c0 = c0.max(plan.w);
                }
                p.rows.push(Row::Nu {
                    g: it,
                    title,
                    cells,
                    lead,
                });
            } else if self.united_row(it, oa, depth) {
                let cells = self.nu_cells(it, mode, oa, depth + 1);
                if let Some(Cell::Field { plan, .. }) = cells.first()
                    && plan.pos == Pos::Left
                    && plan.has_text()
                {
                    c0 = c0.max(plan.w);
                }
                p.rows.push(Row::United(it));
            } else {
                p.rows.push(Row::Group(it));
            }
        }
        let gap = r.title_gap;
        let c1x = x0 + c0 + if c0 > 0.0 { gap } else { 0.0 };
        p.c0 = c0;
        p.c1x = c1x;
        let mut e = x0;
        let mut cols: Vec<Pair> = Vec::new();
        let hgap_of =
            |g: &Item| r.h_spacing[g.props.horizontal_spacing.unwrap_or(Spacing::Single) as usize];
        let mut rows = std::mem::take(&mut p.rows);
        for row in &mut rows {
            match row {
                Row::Plain {
                    it,
                    ro,
                    plan,
                    stretched,
                } => {
                    let left = plan.pos == Pos::Left;
                    let room = if left { avail_w - (c1x - x0) } else { avail_w };
                    let (w, stretch, tight) = self.field_size(it, *ro, room.max(0.0));
                    let mut w = w.min(WHOLE_ROOM);
                    if stretch && mode.p != f32::MAX {
                        w = self.squeeze(w, tight, mode.p);
                        *stretched = Some(w);
                    }
                    e = e.max(match plan.pos {
                        Pos::Left => c1x + w,
                        Pos::Top => x0 + w.max(plan.w),
                        Pos::Right => x0 + w + gap + plan.w,
                        Pos::None => x0 + w,
                    });
                }
                Row::Group(g) => {
                    let ind = self.indent(g);
                    let inner = self
                        .nu_plan(
                            &g.children,
                            x0 + ind,
                            (avail_w - ind).max(0.0),
                            Mode::natural(),
                            depth + 1,
                        )
                        .e;
                    let pad = if g.props.representation.as_deref() == Some("NormalSeparation") {
                        r.nu_gap
                    } else {
                        0.0
                    };
                    e = e.max(inner + pad);
                }
                Row::United(g) => {
                    let hgap = hgap_of(g);
                    let cells = self.nu_cells(g, mode, oa, depth + 1);
                    let mut x = x0;
                    for (i, c) in cells.iter().enumerate() {
                        let w = cell_extent(c, gap);
                        if i == 0 {
                            x = match c {
                                Cell::Field { plan, fw, .. } if plan.pos != Pos::Top => c1x + fw,
                                _ => x0 + w,
                            };
                        } else {
                            x += hgap + w;
                        }
                    }
                    e = e.max(x);
                }
                Row::Nu {
                    title, cells, lead, ..
                } => {
                    if let Some(t) = title {
                        e = e.max(x0 + t.w);
                    }
                    if let (1, Some(Cell::Field { fw, plan, .. })) = (*lead, cells.first()) {
                        e = e.max(match plan.pos {
                            Pos::Top => x0 + fw.max(plan.w),
                            _ => c1x + fw,
                        });
                    }
                    p.cell_stretch |= cells
                        .iter()
                        .skip(*lead)
                        .any(|c| matches!(c, Cell::Field { stretch: true, .. }));
                    for (j, c) in cells.iter().skip(*lead).enumerate() {
                        if cols.len() <= j {
                            cols.resize(j + 1, Pair::default());
                        }
                        let (tw, fw) = match c {
                            Cell::Title { t, .. } => (t.w, 0.0),
                            Cell::Field { plan, fw, .. } => match plan.pos {
                                Pos::Left if plan.has_text() => (plan.w, *fw),
                                Pos::Top => (0.0, fw.max(plan.w)),
                                Pos::Right => (0.0, fw + gap + plan.w),
                                _ => (0.0, *fw),
                            },
                            Cell::Other { w, .. } => (0.0, *w),
                        };
                        cols[j].tw = cols[j].tw.max(tw);
                        cols[j].fw = cols[j].fw.max(fw);
                    }
                }
                Row::Hidden(_) | Row::Other(_) => {}
            }
        }
        let mut x = e + r.nu_gap;
        let mut right = e;
        for c in &mut cols {
            c.x = x;
            let end = c.field_x(gap) + c.fw;
            right = right.max(end.max(c.x + c.tw));
            x = end.max(c.x + c.tw) + r.nu_gap;
        }
        p.rows = rows;
        p.e = e;
        p.pairs = cols;
        p.right = right;
        p
    }

    fn united_row(&mut self, g: &Item, oa: f32, depth: usize) -> bool {
        match g.props.group {
            Some(GroupKind::Vertical) => false,
            Some(GroupKind::Horizontal | GroupKind::AlwaysHorizontal) => true,
            _ => self.orient(g, oa, depth + 1, false).0,
        }
    }

    pub(super) fn has_nu(&self, items: &[Item]) -> bool {
        items.iter().any(|c| self.is_nu(c))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn nu_grid(
        &mut self,
        items: &[Item],
        x0: f32,
        avail_w: f32,
        y0: f32,
        gap: Spacing,
        depth: usize,
    ) -> Option<f32> {
        let mark = self.warnings.len();
        let limit = x0 + avail_w;
        let mut plan = self.nu_plan(items, x0, avail_w, Mode::natural(), depth);
        if plan.right > limit + 0.01 {
            let left = if plan.cell_stretch {
                self.fit_p(items, x0, avail_w, false, depth, limit)
            } else {
                None
            };
            let mode = match left {
                Some(p) => Mode { top: false, p },
                None => Mode {
                    top: true,
                    p: self
                        .fit_p(items, x0, avail_w, true, depth, limit)
                        .unwrap_or(0.0),
                },
            };
            plan = self.nu_plan(items, x0, avail_w, mode, depth);
            if plan.right > limit + 0.01 {
                self.warnings.truncate(mark);
                return None;
            }
        }
        let d = depth.min(u16::MAX as usize) as u16;
        let gap_px = self.v_gap(gap);
        let mut y = y0;
        let mut first = true;
        let mut prev_bottom = 0.0f32;
        for row in &plan.rows {
            if let Row::Hidden(it) = row {
                self.hidden_subtree(it, d);
                continue;
            }
            if let Row::Nu {
                title: None, cells, ..
            } = row
                && cells.is_empty()
            {
                continue;
            }
            let pads = match row {
                Row::Group(g) | Row::United(g) => self.repr_pads(g),
                _ => (0.0, 0.0),
            };
            if !first {
                y += gap_px + (prev_bottom - pads.0).max(0.0);
            }
            first = false;
            prev_bottom = pads.1;
            let h = match row {
                Row::Hidden(_) => 0.0,
                Row::Plain {
                    it,
                    ro,
                    plan: tp,
                    stretched,
                } => {
                    let cap = stretched.unwrap_or(f32::MAX);
                    let place = Place {
                        x0,
                        avail_w: avail_w.min(cap),
                        title_x: x0,
                        field_x: plan.c1x,
                        field_avail: (avail_w - (plan.c1x - x0)).max(0.0).min(cap),
                        right: false,
                        top: false,
                        y,
                        depth: d,
                    };
                    self.titled(it, *ro, tp.clone(), place)
                }
                Row::Group(g) => self.group(g, x0, avail_w, y, depth, d),
                Row::United(g) => {
                    self.row_col = Some(plan.c0);
                    self.group(g, x0, avail_w, y, depth, d)
                }
                Row::Other(it) => self.vertical(
                    std::slice::from_ref(*it),
                    x0,
                    avail_w,
                    y,
                    gap,
                    depth,
                    &VOpts::default(),
                ),
                Row::Nu {
                    g,
                    title,
                    cells,
                    lead,
                } => self.nu_row(g, title.as_ref(), cells, *lead, &plan, x0, y, depth, d),
            };
            y += h;
        }
        let mut seen = std::collections::BTreeSet::new();
        let tail: Vec<String> = self.warnings.drain(mark..).collect();
        self.warnings
            .extend(tail.into_iter().filter(|w| seen.insert(w.clone())));
        Some(y - y0)
    }

    #[allow(clippy::too_many_arguments)]
    fn nu_row(
        &mut self,
        g: &Item,
        title: Option<&NuTitle>,
        cells: &[Cell<'_>],
        lead: usize,
        plan: &NuPlan<'_>,
        x0: f32,
        y: f32,
        depth: usize,
        d: u16,
    ) -> f32 {
        let r = self.r;
        let gap = r.title_gap;
        let idx = self.boxes.len();
        let base = if plan.mode.top { r.title_top_dy } else { 0.0 };
        let mut gb = ItemBox::new(
            BoxKey::Item(g.name.clone()),
            BoxKind::Group,
            Rect::new(x0, y, (plan.right - x0).max(0.0), 0.0),
            d,
        );
        let mut h = 0.0f32;
        if let Some(t) = title {
            gb.title = Some(t.title_box(x0, y + base));
            h = h.max(base + t.h);
        }
        self.boxes.push(gb);
        let hidden: Vec<&Item> = g.children.iter().filter(|c| self.is_hidden(c)).collect();
        for c in hidden {
            self.hidden_subtree(c, d + 1);
        }
        for (i, c) in cells.iter().enumerate() {
            let pair = if i >= lead {
                plan.pairs.get(i - lead).copied()
            } else {
                None
            };
            match c {
                Cell::Field {
                    it,
                    ro,
                    plan: tp,
                    fw,
                    ..
                } => {
                    let (tx, fx, x) = match pair {
                        Some(pr) => {
                            let fx = pr.field_x(gap);
                            (pr.x, fx, if tp.pos == Pos::Left { pr.x } else { fx })
                        }
                        None => (x0, plan.c1x, x0),
                    };
                    let place = Place {
                        x0: x,
                        avail_w: *fw,
                        title_x: tx,
                        field_x: fx,
                        field_avail: *fw,
                        right: false,
                        top: true,
                        y,
                        depth: d + 1,
                    };
                    h = h.max(self.titled(it, *ro, tp.clone(), place));
                }
                Cell::Title { g: ng, t } => {
                    let x = pair.map_or(x0, |p| p.x);
                    h = h.max(base + self.row_title(ng, t, x, y + base, d + 1));
                }
                Cell::Other { it, w } => {
                    let x = pair.map_or(x0, |p| p.x);
                    let one = VOpts::row();
                    h = h.max(self.vertical(
                        std::slice::from_ref(*it),
                        x,
                        *w,
                        y,
                        Spacing::Single,
                        depth + 1,
                        &one,
                    ));
                }
            }
        }
        self.boxes[idx].rect.h = h;
        h
    }

    pub(super) fn open_row<'i>(
        &mut self,
        kids: &[&'i Item],
        metas: &[Meas],
        oa: f32,
        depth: usize,
        d: u16,
    ) -> (Vec<RowKid<'i>>, Vec<Meas>) {
        let mut ks = Vec::new();
        let mut ms = Vec::new();
        for (k, m) in kids.iter().zip(metas) {
            if self.is_nu(k) {
                self.open_nu(k, oa, depth, d, &mut ks, &mut ms);
            } else {
                ks.push(RowKid::Item(k));
                ms.push(*m);
            }
        }
        (ks, ms)
    }

    fn open_nu<'i>(
        &mut self,
        g: &'i Item,
        oa: f32,
        depth: usize,
        d: u16,
        ks: &mut Vec<RowKid<'i>>,
        ms: &mut Vec<Meas>,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        if let Some(t) = self.nu_title(g) {
            let w = t.w;
            ks.push(RowKid::Title(g, t));
            ms.push(Meas {
                nat: w,
                tight: w,
                max: w,
                ..Meas::default()
            });
        }
        for c in &g.children {
            if self.is_hidden(c) {
                self.hidden_subtree(c, d + 1);
            } else if self.is_nu(c) {
                self.open_nu(c, oa, depth + 1, d, ks, ms);
            } else {
                let m = self.measure(c, oa, depth + 1);
                ks.push(RowKid::Item(c));
                ms.push(m);
            }
        }
    }

    pub(super) fn row_title(&mut self, g: &Item, t: &NuTitle, x: f32, y: f32, d: u16) -> f32 {
        let mut b = ItemBox::new(
            BoxKey::Item(g.name.clone()),
            BoxKind::Group,
            Rect::new(x, y, t.w, t.h),
            d,
        );
        b.title = Some(t.title_box(x, y));
        self.boxes.push(b);
        t.h
    }
}

fn cell_extent(c: &Cell<'_>, gap: f32) -> f32 {
    match c {
        Cell::Field { plan, fw, .. } => match plan.pos {
            Pos::Left if plan.has_text() => plan.w + gap + fw,
            Pos::Top => fw.max(plan.w),
            Pos::Right => fw + gap + plan.w,
            _ => *fw,
        },
        Cell::Title { t, .. } => t.w,
        Cell::Other { w, .. } => *w,
    }
}
