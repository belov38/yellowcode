use form_model::stdcmd;
use form_model::{HAlign, Item, ItemKind, Location, MAX_DEPTH, Spacing, TypeKind};
use ui_style::Metrics;

use crate::rules::Rules;
use crate::title::{self, Pos};
use crate::width::{self, field_width, label_width, type_width};
use crate::{
    BoxFlags, BoxKey, BoxKind, FieldButton, ItemBox, Layout, LayoutError, LayoutInput, Part, Rect,
    Size, WindowGeom, WindowMode,
};

#[path = "controls.rs"]
mod controls;
pub(crate) use controls::MORE_ARROW;
#[path = "group.rs"]
mod group;
#[path = "nu.rs"]
mod nu;
#[path = "pages.rs"]
mod pages;
#[path = "table.rs"]
mod table;

use group::VOpts;

struct Ctx<'a> {
    input: &'a LayoutInput<'a>,
    r: &'static Rules,
    m: &'static Metrics,
    boxes: Vec<ItemBox>,
    warnings: Vec<String>,
    stretch_sum: f32,
    grow: Option<(f32, f32)>,
    stretch_seen: usize,
    stretch_n: usize,
    stretch_given: f32,
    overflow: bool,
    float: Option<FloatPass>,
    nat_need: f32,
    float_list: bool,
    row_col: Option<f32>,
}

#[derive(Clone, Copy)]
struct FloatPass {
    fit_w: f32,
    natural: bool,
    stretch: bool,
}

struct Pass {
    layout: Layout,
    used: f32,
    need: f32,
    list: bool,
}

pub(crate) fn layout(input: &LayoutInput, r: &'static Rules) -> Result<Layout, LayoutError> {
    let m = Metrics::get(input.interface).ok_or(LayoutError::Unsupported(
        "no text metrics for this interface",
    ))?;
    if input.window == WindowMode::Floating {
        return floating(input, r, m);
    }
    let mut l = pass(input, r, m, None).layout;
    let vp = input.viewport;
    let ws = Rect::new(
        0.0,
        r.workspace_top,
        vp.w,
        (vp.h - r.workspace_top).max(0.0),
    );
    l.window = WindowGeom {
        floating: false,
        window: ws,
        workspace: ws,
        content: Rect::new(0.0, r.content_top, input.content.w, input.content.h),
    };
    Ok(l)
}

fn floating(
    input: &LayoutInput,
    r: &'static Rules,
    m: &'static Metrics,
) -> Result<Layout, LayoutError> {
    let vp = input.viewport;
    let ws = Rect::new(
        0.0,
        r.workspace_top,
        vp.w,
        (vp.h - r.workspace_top).max(0.0),
    );
    let fit_w = r.float_w_k * vp.w + r.float_w_b;
    let sized = |w: f32| LayoutInput {
        content: Size { w, h: 0.0 },
        opened: Size { w, h: 0.0 },
        ..*input
    };
    let nat = pass(
        &sized(NATURAL_ROOM),
        r,
        m,
        Some(FloatPass {
            fit_w,
            natural: true,
            stretch: false,
        }),
    );
    let wc = r.float_content_w(nat.need, vp.w);
    let fl = FloatPass {
        fit_w,
        natural: false,
        stretch: true,
    };
    let p0 = pass(
        &sized(wc),
        r,
        m,
        Some(FloatPass {
            stretch: false,
            ..fl
        }),
    );
    let target = if p0.list {
        r.float_list_content_h
    } else {
        p0.used + r.float_pad
    };
    let (win_h, content_h) = r.float_heights(target, ws.h);
    let p = pass(
        &LayoutInput {
            content: Size {
                w: wc,
                h: content_h,
            },
            opened: Size {
                w: wc,
                h: content_h,
            },
            ..*input
        },
        r,
        m,
        Some(fl),
    );
    let (win_w, frame) = (wc + 2.0 * r.float_frame, r.float_frame);
    let (x, y) = r.float_pos(vp.w, ws.h, win_w, win_h);
    let mut l = p.layout;
    l.content = Size {
        w: wc,
        h: content_h,
    };
    l.window = WindowGeom {
        floating: true,
        window: Rect::new(x, y, win_w, win_h),
        workspace: ws,
        content: Rect::new(x + frame, y + r.float_head_h, wc, content_h),
    };
    Ok(l)
}

const WHOLE_ROOM: f32 = 1.0e6;

const NATURAL_ROOM: f32 = 8000.0;

fn pass(
    input: &LayoutInput,
    r: &'static Rules,
    m: &'static Metrics,
    float: Option<FloatPass>,
) -> Pass {
    let mut cx = Ctx {
        input,
        r,
        m,
        boxes: Vec::new(),
        warnings: Vec::new(),
        stretch_sum: 0.0,
        grow: None,
        stretch_seen: 0,
        stretch_n: 0,
        stretch_given: 0.0,
        overflow: false,
        float,
        nat_need: 0.0,
        float_list: false,
        row_col: None,
    };
    let mut used = cx.root(r);
    let scroll = if cx.overflow { r.table_scroll_h } else { 0.0 };
    let free = if float.is_some() {
        (input.content.h - used).max(0.0) - scroll
    } else {
        input.content.h - used - scroll
    };
    let any_free = if float.is_some() {
        free != 0.0
    } else {
        free > 0.0
    };
    if cx.stretch_sum > 0.0 && any_free && float.is_none_or(|f| f.stretch) {
        cx.boxes.clear();
        cx.warnings.clear();
        cx.stretch_n = cx.stretch_seen;
        cx.grow = Some((free, cx.stretch_sum));
        used = cx.root(r);
    }
    Pass {
        layout: Layout {
            content: input.content,
            window: WindowGeom {
                floating: false,
                window: Rect::default(),
                workspace: Rect::default(),
                content: Rect::default(),
            },
            boxes: cx.boxes,
            warnings: cx.warnings,
        },
        used,
        need: cx.nat_need,
        list: cx.float_list,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Input,
    Check,
    Label,
    Radio,
}

fn role(it: &Item) -> Option<Role> {
    match it.kind {
        ItemKind::InputField => Some(Role::Input),
        ItemKind::CheckBoxField => Some(Role::Check),
        ItemKind::LabelField => Some(Role::Label),
        ItemKind::RadioButtonField => Some(Role::Radio),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct Place {
    x0: f32,
    avail_w: f32,
    title_x: f32,
    field_x: f32,
    field_avail: f32,
    right: bool,
    top: bool,
    y: f32,
    depth: u16,
}

impl Ctx<'_> {
    fn root(&mut self, r: &'static Rules) -> f32 {
        self.stretch_sum = 0.0;
        self.stretch_seen = 0;
        self.stretch_given = 0.0;
        self.overflow = false;
        self.nat_need = 0.0;
        self.float_list = false;
        let input = self.input;
        let x0 = r.margin_left;
        let work_w = (input.content.w - r.margin_left - r.margin_right).max(0.0);
        if let Some(bar) = &input.form.auto_command_bar {
            let rect = Rect::new(x0, r.cmd_bar_y, work_w, r.cmd_bar_h);
            let mut b = ItemBox::new(BoxKey::Item(bar.name.clone()), BoxKind::CommandBar, rect, 0);
            b.hidden = bar.props.visible == Some(false);
            if b.hidden {
                b.rect = Rect::default();
            }
            let hidden = b.hidden;
            self.boxes.push(b);
            if !hidden {
                let limit = x0 + work_w - self.more_width() - r.cmd_gap;
                let mut cmds = stdcmd::bar_commands_with(input.form, bar, input.types);
                let main = stdcmd::main_table(input.form).filter(|t| self.hosted_on_form_bar(t));
                let is_list = matches!(
                    stdcmd::form_role_with(input.form, input.types),
                    stdcmd::FormRole::List { .. }
                );
                if is_list && main.is_none() {
                    cmds.retain(|c| !c.on_bar);
                }
                if let Some(t) = main {
                    if matches!(t.props.search_control_location, Some(Location::None)) {
                        cmds.retain(|c| c.id != "SearchControl");
                    }
                    let control = cmds
                        .iter()
                        .position(|c| c.id == "SearchControl" && c.on_bar)
                        .filter(|_| self.generated());
                    let ctl_w = if control.is_some() {
                        r.search_control_w + r.cmd_gap
                    } else {
                        0.0
                    };
                    if !matches!(t.props.search_string_location, Some(Location::None)) {
                        self.search_string(t, &bar.name, x0 + work_w - ctl_w, r.cmd_bar_y, 1);
                    }
                    if let Some(i) = control {
                        let c = cmds.remove(i);
                        let x = x0 + work_w - r.table_search_gap - r.search_control_w;
                        let b = ItemBox::new(
                            BoxKey::Item(c.item.clone()),
                            BoxKind::CommandButton,
                            Rect::new(x, r.cmd_bar_y, r.search_control_w, r.button_h),
                            1,
                        );
                        self.boxes.push(b);
                    }
                }
                let end = self.std_buttons(&cmds, x0, r.cmd_bar_y, 1, limit);
                self.bar_buttons(&bar.children, end, r.cmd_bar_y, 1, limit);
                self.more_button(&bar.name, x0, work_w, r.cmd_bar_y);
            }
        }
        self.vertical(
            &input.form.items,
            x0,
            work_w,
            0.0,
            Spacing::Single,
            0,
            &VOpts::default(),
        )
    }

    fn stretch(&mut self, h: f32) -> f32 {
        self.stretch_sum += h;
        self.stretch_seen += 1;
        let Some((free, sum)) = self.grow else {
            return 0.0;
        };
        let extra = if self.stretch_seen >= self.stretch_n {
            free - self.stretch_given
        } else {
            (free * h / sum).floor()
        };
        self.stretch_given += extra;
        extra
    }

    pub(super) fn generated(&self) -> bool {
        self.input.form.role.is_some()
    }

    fn resized(&self) -> bool {
        (self.input.content.w - self.input.opened.w).abs() > 0.5
    }

    fn is_hidden(&self, it: &Item) -> bool {
        it.props.visible == Some(false) || self.input.state.hidden.contains(&it.name)
    }

    fn v_gap(&self, s: Spacing) -> f32 {
        self.r.v_spacing[s as usize]
    }

    fn title_text(&self, it: &Item) -> String {
        if let Some(t) = &it.props.title {
            return t.any().unwrap_or("").to_string();
        }
        let Some(path) = it.props.data_path.as_deref() else {
            return String::new();
        };
        if let Some(t) = self
            .input
            .form
            .resolve(path, self.input.types)
            .and_then(|r| r.title)
            .and_then(|t| t.any().map(str::to_string))
        {
            return t;
        }
        path.rsplit('.').next().unwrap_or("").to_string()
    }

    fn rows(&self, it: &Item) -> Option<u32> {
        let p = &it.props;
        match (p.multi_line == Some(true), p.height) {
            (true, None) => Some(self.r.multi_default_rows),
            (_, Some(n)) if n > 1 => Some(n.min(1000)),
            (true, Some(n)) => Some(n.max(1)),
            _ => None,
        }
    }

    fn data_kind(&self, it: &Item) -> TypeKind {
        it.props
            .data_path
            .as_deref()
            .and_then(|p| self.input.form.resolve(p, self.input.types))
            .map_or(TypeKind::String(0), |res| res.ty.kind())
    }

    fn title_plan(&mut self, it: &Item, role: Role) -> title::Plan {
        let multi = match role {
            Role::Input => self.rows(it).is_some_and(|n| n > 1),
            Role::Radio => self.radio_rows(it) > 1,
            _ => false,
        };
        let (pos, unsupported) = title::preferred(it.props.title_location, multi);
        if unsupported {
            self.warnings
                .push(format!("{}: title position not supported", it.name));
        }
        title::plan(
            &self.title_text(it),
            pos,
            it.props.title_height,
            self.m,
            self.r,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn vertical(
        &mut self,
        items: &[Item],
        x0: f32,
        avail_w: f32,
        y0: f32,
        gap: Spacing,
        depth: usize,
        opts: &VOpts,
    ) -> f32 {
        if depth > MAX_DEPTH {
            self.warnings
                .push("nesting too deep, subtree skipped".into());
            return 0.0;
        }
        let r = self.r;
        if !opts.row
            && self.has_nu(items)
            && let Some(h) = self.nu_grid(items, x0, avail_w, y0, gap, depth)
        {
            return h;
        }
        let aligns: Vec<HAlign> = items.iter().map(|it| opts.align_of(it)).collect();
        let mut plans: Vec<Option<title::Plan>> = Vec::with_capacity(items.len());
        let mut col = 0.0f32;
        for (it, al) in items.iter().zip(&aligns) {
            let mut plan = match role(it) {
                Some(ro) if !self.is_hidden(it) => Some(self.title_plan(it, ro)),
                _ => None,
            };
            let narrow = r.narrow_cell_fit && opts.row && !self.resized();
            if let (true, Some(ro), Some(p)) = (narrow, role(it), plan.as_mut())
                && p.pos == Pos::Left
                && p.has_text()
                && !p.explicit_h
            {
                let (fw, _, nf) = self.field_size(it, ro, f32::MAX);
                let d = p.w + r.title_gap + nf - avail_w;
                if fw < 1.0e6 && nf.is_finite() && d > 0.0 && p.w + nf > 0.0 {
                    let w = (p.w - d * p.w / (p.w + nf)).max(0.0);
                    *p = title::fitted(&p.raw, w, self.m, r);
                }
            }
            if let Some(p) = plan
                .as_ref()
                .filter(|p| p.pos == Pos::Left && *al == HAlign::Left)
            {
                col = col.max(p.w);
            }
            plans.push(plan);
        }
        col = col.max(opts.title_col);
        if self.float.is_some() && !opts.row {
            self.float_titles_on_top(items, &aligns, &mut plans, x0, &mut col);
        }
        let field_x = if col > 0.0 {
            x0 + col + r.title_gap
        } else {
            x0
        };
        let field_avail = (avail_w - (field_x - x0)).max(0.0);
        let d = depth.min(u16::MAX as usize) as u16;
        let gap_px = self.v_gap(gap);
        let region = if aligns
            .iter()
            .any(|a| matches!(a, HAlign::Center | HAlign::Right))
        {
            self.align_region(items, avail_w, depth)
        } else {
            0.0
        };
        let btn_col = items
            .iter()
            .filter(|c| !self.is_hidden(c) && self.plain_button(c))
            .map(|c| self.button_nat(c))
            .fold(0.0f32, f32::max);
        let mut y = y0;
        let mut first = true;
        let mut prev_bottom = 0.0f32;
        for ((it, plan), al) in items.iter().zip(plans).zip(aligns) {
            if self.is_hidden(it) {
                self.hidden_subtree(it, d);
                continue;
            }
            if !first {
                y += gap_px + (prev_bottom - self.repr_pads(it).0).max(0.0);
            }
            first = false;
            if matches!(al, HAlign::Center | HAlign::Right) {
                let bw = self.measure(it, avail_w, depth).nat.min(avail_w);
                let free = (region - bw).max(0.0);
                let dx = if al == HAlign::Center {
                    free / 2.0
                } else {
                    free
                };
                let one = VOpts::single();
                y += self.vertical(
                    std::slice::from_ref(it),
                    x0 + dx.round(),
                    bw,
                    y,
                    gap,
                    depth,
                    &one,
                );
                continue;
            }
            let mut place = Place {
                x0,
                avail_w,
                title_x: x0,
                field_x,
                field_avail,
                right: false,
                top: opts.row,
                y,
                depth: d,
            };
            if let Some(p) = plan.as_ref().filter(|p| p.pos == Pos::Left && p.w > 0.0) {
                match opts.children_align {
                    group::ChildrenAlign::None => {
                        place.field_x = x0 + p.w + r.title_gap;
                        place.field_avail = (avail_w - p.w - r.title_gap).max(0.0);
                    }
                    group::ChildrenAlign::TitlesRight => place.title_x = x0 + col - p.w,
                    group::ChildrenAlign::ItemsRight => place.right = true,
                    group::ChildrenAlign::Auto => {}
                }
            }
            let h = match (role(it), plan) {
                (Some(ro), Some(plan)) => self.titled(it, ro, plan, place),
                _ => match &it.kind {
                    ItemKind::UsualGroup => self.group(it, x0, avail_w, y, depth, d),
                    ItemKind::LabelDecoration => self.decoration(it, x0, avail_w, y, d),
                    ItemKind::PictureDecoration | ItemKind::PictureField => {
                        self.picture(it, x0, y, d)
                    }
                    ItemKind::Button => self.button(it, x0, avail_w, y, d, btn_col),
                    ItemKind::CommandBar => self.command_bar(it, x0, avail_w, y, d),
                    ItemKind::Pages => self.pages(it, x0, avail_w, y, depth, d),
                    ItemKind::Table => self.table(it, x0, avail_w, y, d),
                    ItemKind::SpreadSheetDocumentField => self.spreadsheet(it, x0, avail_w, y, d),
                    other => self.stub(it, other.xml().to_string(), x0, avail_w, y, d),
                },
            };
            y += h;
            prev_bottom = self.repr_pads(it).1;
        }
        y - y0
    }

    fn float_titles_on_top(
        &mut self,
        items: &[Item],
        aligns: &[HAlign],
        plans: &mut [Option<title::Plan>],
        x0: f32,
        col: &mut f32,
    ) {
        let (r, fit) = (self.r, self.float.map_or(f32::MAX, |f| f.fit_w));
        loop {
            let field_x = x0 + *col + r.title_gap;
            let mut worst: Option<(usize, f32)> = None;
            for (i, ((it, plan), al)) in items.iter().zip(plans.iter()).zip(aligns).enumerate() {
                let (Some(ro), Some(p)) = (role(it), plan.as_ref()) else {
                    continue;
                };
                if p.pos != Pos::Left
                    || !p.has_text()
                    || *al != HAlign::Left
                    || !matches!(ro, Role::Input | Role::Label)
                {
                    continue;
                }
                let (fw, stretched, _) = self.field_size(it, ro, f32::MAX);
                let own_w = Some(fw).filter(|w| *w < WHOLE_ROOM);
                let misfit =
                    !stretched && own_w.is_some_and(|w| field_x + w + r.margin_right > fit);
                if misfit && worst.is_none_or(|(_, w)| p.w > w) {
                    worst = Some((i, p.w));
                }
            }
            let Some((i, _)) = worst else { return };
            if let (Some(it), Some(p)) = (items.get(i), plans.get_mut(i).and_then(Option::as_mut)) {
                *p = p.at(Pos::Top, it.props.title_height, self.m, r);
            }
            *col = items
                .iter()
                .zip(plans.iter())
                .zip(aligns)
                .filter_map(|((_, p), al)| {
                    p.as_ref()
                        .filter(|p| p.pos == Pos::Left && *al == HAlign::Left)
                })
                .fold(0.0f32, |c, p| c.max(p.w));
        }
    }

    fn titled(&mut self, it: &Item, ro: Role, mut plan: title::Plan, mut p: Place) -> f32 {
        let r = self.r;
        let row_dy = if p.top && ro == Role::Check {
            r.check_row_dy
        } else {
            0.0
        };
        p.y += row_dy;
        let props = &it.props;
        let kind = self.data_kind(it);
        let mut tw = type_width(&kind);
        let rows = if ro == Role::Input {
            self.rows(it)
        } else {
            None
        };
        match ro {
            Role::Input => width::apply_button_props(&mut tw, props),
            _ => tw.buttons.clear(),
        }
        let room0 = match plan.pos {
            Pos::Left => p.field_avail,
            _ => p.avail_w,
        };
        let mut look = self.look_in(it, ro, room0);
        if ro == Role::Radio
            && plan.pos == Pos::Left
            && plan.has_text()
            && it.props.title_location.is_none()
            && matches!(&look, controls::Look::Radios(g) if g.rows > 1)
        {
            plan = plan.at(Pos::Top, props.title_height, self.m, r);
            look = self.look_in(it, ro, p.avail_w);
        }
        let look_size = look.size(r);
        let mut h = match ro {
            Role::Input => rows.map_or(r.field_h, |n| r.multi_row_h * n as f32 - r.multi_trim),
            Role::Label => r.label_h,
            Role::Check | Role::Radio => look_size.map_or(r.check_size, |s| s.1),
        };
        if p.depth == 0 && rows.is_some_and(|n| n > 1) && props.vertical_stretch != Some(false) {
            h += self.stretch(h);
        }
        let natural = self.float.is_some_and(|f| f.natural);
        let size = |room: f32| match ro {
            Role::Input if natural => (width::natural_width(props, &tw, r), false),
            Role::Input => field_width(props, &tw, room, r),
            Role::Label => label_width(props, &tw, room, r),
            Role::Check | Role::Radio => (look_size.map_or(r.check_size, |s| s.0), false),
        };
        let share_rule = self.float.is_none();
        if share_rule
            && plan.pos == Pos::Left
            && plan.has_text()
            && matches!(ro, Role::Input | Role::Label)
        {
            let (wanted, stretched) = size(f32::MAX);
            if !stretched && (p.field_x - p.x0) + wanted > r.title_top_share * self.input.content.w
            {
                plan = plan.at(Pos::Top, props.title_height, self.m, r);
            }
        }
        let centred = ((h - r.title_h) / 2.0).floor().max(0.0);
        let title_dy = match (ro, &look) {
            (Role::Input, _) => r.title_dy,
            (Role::Label, _) => r.label_title_dy,
            (Role::Check, controls::Look::Plain) => r.check_title_dy,
            (Role::Radio, controls::Look::Radios(_)) => 0.0,
            _ => centred,
        };
        let box_row = if ro == Role::Check && look_size.is_none() {
            r.check_row_h
        } else {
            h
        };
        let (fx, room) = match plan.pos {
            Pos::Left => (p.field_x, p.field_avail),
            _ => (p.x0, p.avail_w),
        };
        let (w, _) = size(room);
        let fx = if p.right && plan.pos == Pos::Left {
            p.x0 + p.avail_w - w
        } else {
            fx
        };
        if natural {
            let right = match plan.pos {
                Pos::Top => (fx + w).max(p.x0 + plan.w),
                _ => fx + w,
            };
            self.nat_need = self.nat_need.max(right + r.float_field_pad);
        }
        let (fy, row_h, title_rect) = match plan.pos {
            Pos::Left if plan.h > h => {
                let dy = if plan.explicit_h || p.top {
                    0.0
                } else {
                    ((plan.h - h) / 2.0).round()
                };
                (p.y + dy, plan.h, Rect::new(p.title_x, p.y, plan.w, plan.h))
            }
            Pos::Left => (
                p.y,
                box_row,
                Rect::new(p.title_x, p.y + title_dy, plan.w, plan.h),
            ),
            Pos::Top => {
                let tw_px = if matches!(ro, Role::Check | Role::Radio) {
                    plan.w
                } else {
                    w
                };
                (
                    p.y + r.title_top_dy,
                    r.title_top_dy + box_row,
                    Rect::new(p.x0, p.y, tw_px, plan.h),
                )
            }
            Pos::Right => {
                let x = if ro == Role::Check {
                    fx + w + r.check_title_gap
                } else {
                    fx + w + r.title_gap
                };
                (p.y, box_row, Rect::new(x, p.y + title_dy, plan.w, plan.h))
            }
            Pos::None => (p.y, box_row, Rect::default()),
        };
        let bkind = match ro {
            Role::Input => BoxKind::Field,
            Role::Label => BoxKind::LabelField,
            Role::Check => match look {
                controls::Look::Switch => BoxKind::Switch,
                controls::Look::Tumbler(_) => BoxKind::Toggle,
                _ => BoxKind::CheckBox,
            },
            Role::Radio => match look {
                controls::Look::Tumbler(_) => BoxKind::Toggle,
                _ => BoxKind::RadioItem,
            },
        };
        let mut b = ItemBox::new(
            BoxKey::Item(it.name.clone()),
            bkind,
            Rect::new(fx, fy, w, h),
            p.depth,
        );
        let mut spin: Option<ItemBox> = None;
        if ro == Role::Input {
            let slots = tw.slots() as f32;
            let input_w = (w - r.input_pad - r.button_w * slots).max(0.0);
            let input_x = fx + r.input_dx;
            b.input = Some(Rect::new(
                input_x,
                fy + r.input_dy,
                input_w,
                (h - r.input_trim).max(0.0),
            ));
            b.buttons = tw.buttons.clone();
            if let Some(i) = tw.buttons.iter().position(|x| *x == FieldButton::SpinUp) {
                spin = Some(ItemBox::new(
                    BoxKey::Part {
                        owner: it.name.clone(),
                        part: Part::SpinButtons,
                    },
                    BoxKind::SpinButtons,
                    Rect::new(
                        input_x + input_w + r.button_w * i as f32 + r.spin_dx,
                        fy + r.spin_dy,
                        r.spin_w,
                        r.spin_h,
                    ),
                    p.depth,
                ));
            }
        }
        let idx = self.boxes.len();
        self.boxes.push(b);
        self.finish_title(idx, &plan, title_rect);
        self.finish_flags(idx, it, &tw);
        self.look_parts(idx, it, &look);
        self.boxes.extend(spin);
        row_h + row_dy
    }

    fn finish_title(&mut self, idx: usize, plan: &title::Plan, rect: Rect) {
        if plan.has_text() && plan.pos != Pos::None {
            self.boxes[idx].title = Some(title::title_box(plan, rect));
        }
    }

    fn finish_flags(&mut self, idx: usize, it: &Item, tw: &width::TypeWidth) {
        self.boxes[idx].flags = BoxFlags {
            read_only: it.props.read_only == Some(true),
            disabled: it.props.enabled == Some(false),
            align_right: tw.align_right
                && !matches!(
                    it.kind,
                    ItemKind::CheckBoxField | ItemKind::RadioButtonField
                ),
            password: it.props.password_mode == Some(true),
            multi_line: self.rows(it).is_some(),
            ..BoxFlags::default()
        };
    }

    fn stub(&mut self, it: &Item, what: String, x0: f32, avail_w: f32, y: f32, d: u16) -> f32 {
        self.warnings
            .push(format!("{}: item kind {what} not supported yet", it.name));
        let h = self.r.field_h;
        self.boxes.push(ItemBox::new(
            BoxKey::Item(it.name.clone()),
            BoxKind::Stub(what),
            Rect::new(x0, y, avail_w, h),
            d,
        ));
        h
    }

    fn hidden_subtree(&mut self, root: &Item, d: u16) {
        let mut stack = vec![(root, d)];
        while let Some((it, d)) = stack.pop() {
            let kind = match &it.kind {
                ItemKind::InputField => BoxKind::Field,
                ItemKind::CheckBoxField => BoxKind::CheckBox,
                ItemKind::LabelField => BoxKind::LabelField,
                ItemKind::UsualGroup | ItemKind::Page => BoxKind::Group,
                ItemKind::RadioButtonField => BoxKind::RadioItem,
                ItemKind::LabelDecoration => BoxKind::Label,
                ItemKind::PictureDecoration | ItemKind::PictureField => BoxKind::Picture,
                ItemKind::Button => BoxKind::Button,
                ItemKind::CommandBar => BoxKind::CommandBar,
                ItemKind::Pages => BoxKind::Tabs,
                ItemKind::Table => BoxKind::Grid,
                ItemKind::SpreadSheetDocumentField => BoxKind::SpreadsheetField,
                other => BoxKind::Stub(other.xml().to_string()),
            };
            let mut b = ItemBox::new(BoxKey::Item(it.name.clone()), kind, Rect::default(), d);
            b.hidden = true;
            self.boxes.push(b);
            if usize::from(d) < MAX_DEPTH {
                for c in it.children.iter().rev() {
                    stack.push((c, d + 1));
                }
            }
        }
    }
}
