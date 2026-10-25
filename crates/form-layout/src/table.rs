use form_model::stdcmd::{self, std_name};
use form_model::{Item, ItemKind, Location, TypeKind};

use super::Ctx;
use crate::title::{self, Pos};
use crate::width::{apply_button_props, type_width};
use crate::{BoxKey, BoxKind, FieldButton, GridBox, GridColumn, ItemBox, Part, Rect, Size};
use ui_style::captions::command_text;

struct Col {
    name: String,
    title: String,
    base: f32,
    stretch: bool,
    check: bool,
}

fn widths(cols: &[Col], inner: f32) -> Vec<f32> {
    let fixed: f32 = cols.iter().filter(|c| !c.stretch).map(|c| c.base).sum();
    let bases: f32 = cols.iter().filter(|c| c.stretch).map(|c| c.base).sum();
    let free = inner - fixed;
    let mut w: Vec<f32> = cols.iter().map(|c| c.base).collect();
    if bases > 0.0 {
        if free > bases {
            for (x, c) in w.iter_mut().zip(cols) {
                if c.stretch {
                    *x = c.base * free / bases;
                }
            }
        }
    } else if free > 0.0
        && let Some(i) = cols.iter().rposition(|c| !c.check)
    {
        w[i] += free;
    }
    w
}

impl Ctx<'_> {
    fn columns(&mut self, table: &Item, d: u16, out: &mut Vec<Col>) {
        let list = self.generated_list(table);
        let mut stack: Vec<&Item> = table.children.iter().rev().collect();
        while let Some(c) = stack.pop() {
            if c.kind == ItemKind::ColumnGroup {
                stack.extend(c.children.iter().rev());
                continue;
            }
            let mut b = ItemBox::new(
                BoxKey::Item(c.name.clone()),
                BoxKind::Column,
                Rect::default(),
                d,
            );
            b.hidden = true;
            b.flags.column = true;
            self.boxes.push(b);
            if self.is_hidden(c) {
                continue;
            }
            let name = c
                .props
                .data_path
                .as_deref()
                .and_then(|p| p.rsplit('.').next())
                .unwrap_or(&c.name)
                .to_string();
            let mut title = self.title_text(c);
            if title.is_empty() && c.props.title.is_none() {
                title = c.name.clone();
            }
            let (base, stretch, check) = if list {
                self.list_col_base(c, &title)
            } else {
                self.col_base(c)
            };
            out.push(Col {
                name,
                title,
                base,
                stretch,
                check,
            });
        }
    }

    fn generated_list(&self, table: &Item) -> bool {
        self.input.form.role.is_some()
            && stdcmd::table_kind(self.input.form, table) == stdcmd::TableKind::DynamicList
    }

    fn list_col_base(&self, c: &Item, title: &str) -> (f32, bool, bool) {
        let r = self.r;
        let kind = self.data_kind(c);
        let last = c
            .props
            .data_path
            .as_deref()
            .and_then(|p| p.rsplit('.').next())
            .unwrap_or_default();
        let header = self.m.width(title, ui_style::FontRole::Text) + r.list_header_pad;
        let content = |chars: u32| r.char_w * chars as f32 + r.col_width_pad;
        let tw = type_width(&kind);
        let hier = matches!(
            stdcmd::form_role_with(self.input.form, self.input.types),
            stdcmd::FormRole::List {
                hierarchical: true,
                ..
            }
        );
        let register = matches!(
            stdcmd::form_role_with(self.input.form, self.input.types),
            stdcmd::FormRole::List {
                of: stdcmd::ListOf::InformationRegister,
                ..
            }
        );
        let (base, stretch) = match &kind {
            TypeKind::Boolean => return (header, false, true),
            TypeKind::String(k) if *k == 0 => (r.list_ref_w, true),
            TypeKind::String(k) if *k <= r.short_string => (content(*k), false),
            TypeKind::String(k) => (
                r.list_str_step * (*k).min(r.col_str_max_chars) as f32 + r.list_str_base,
                true,
            ),
            TypeKind::Number { .. } => (content(tw.chars), false),
            TypeKind::Date(form_model::DateParts::DateTime) => {
                (content(r.list_datetime_chars), false)
            }
            TypeKind::Date(_) if register && matches!(last, "Период" | "Period") => {
                (r.list_period_w, false)
            }
            TypeKind::Date(_) => (content(tw.chars), false),
            _ => (r.list_ref_w, true),
        };
        let icon = if matches!(last, "Наименование" | "Description") {
            r.list_icon_w + if hier { r.list_hier_icon_w } else { 0.0 }
        } else {
            0.0
        };
        (base.max(header) + icon, stretch, false)
    }

    fn col_base(&self, c: &Item) -> (f32, bool, bool) {
        let r = self.r;
        if c.kind == ItemKind::CheckBoxField {
            return (r.col_check_w, false, true);
        }
        if self.generated()
            && c.props
                .data_path
                .as_deref()
                .and_then(|p| p.rsplit('.').next())
                .is_some_and(|n| matches!(n, "НомерСтроки" | "LineNumber"))
        {
            return (r.col_line_number, false, false);
        }
        let kind = self.data_kind(c);
        let mut tw = type_width(&kind);
        if self.generated()
            && matches!(
                kind,
                TypeKind::Number {
                    digits: 10,
                    fraction: 3
                }
            )
        {
            tw.chars = 10;
        }
        apply_button_props(&mut tw, &c.props);
        if matches!(kind, TypeKind::Boolean) && c.kind != ItemKind::InputField {
            return (r.col_check_w, false, true);
        }
        let stretch = c.props.horizontal_stretch.unwrap_or(tw.stretch);
        let width = c.props.width.filter(|w| *w > 0);
        let slots = if tw.inside { 0 } else { tw.slots() } as f32;
        let is_string = matches!(kind, TypeKind::String(_));
        let extra = if is_string { r.col_string_extra } else { 0.0 };
        let base = match (width, stretch) {
            (Some(w), true) => r.char_w * w as f32 + r.col_width_pad,
            (Some(w), false) => r.char_w * w as f32 + r.col_width_pad + extra,
            (None, true) => match kind {
                TypeKind::String(0) => r.col_str_unlimited,
                TypeKind::String(k) => {
                    let k = k.min(r.col_str_max_chars) as f32;
                    let lo = r.short_string as f32;
                    let mid = r.col_str_mid_chars as f32;
                    let hi = r.col_str_max_chars as f32;
                    let first = r.char_w * lo + r.col_pad + r.col_string_extra;
                    if !self.generated() {
                        first + (r.col_str_max - first) * (k - lo) / (hi - lo)
                    } else if k <= mid {
                        first + (r.col_str_mid - first) * (k - lo) / (mid - lo)
                    } else {
                        r.col_str_mid + (r.col_str_max - r.col_str_mid) * (k - mid) / (hi - mid)
                    }
                }
                _ => r.col_ref,
            },
            (None, false) => r.char_w * tw.chars as f32 + r.col_pad + r.button_w * slots + extra,
        };
        (base, stretch, false)
    }

    fn table_natural_h(&self, it: &Item) -> f32 {
        let r = self.r;
        match it.props.height_in_table_rows {
            Some(n) if n > 0 => r.table_row_step * n.min(10_000) as f32 + r.table_head_extra,
            _ => r.table_default_h,
        }
    }

    pub(super) fn spreadsheet(&mut self, it: &Item, x0: f32, avail_w: f32, y: f32, d: u16) -> f32 {
        let r = self.r;
        let (pos, unsupported) = title::preferred(it.props.title_location, true);
        if unsupported {
            self.warnings
                .push(format!("{}: title position not supported", it.name));
        }
        let plan = title::plan(&self.title_text(it), pos, it.props.title_height, self.m, r);
        let mut h = match it.props.height {
            Some(n) if n > 0 => r.sheet_row_h * n.min(10_000) as f32,
            _ => r.sheet_default_h,
        };
        if it.props.height.is_none_or(|n| n == 0)
            && d == 0
            && it.props.vertical_stretch != Some(false)
        {
            h += self.stretch(h);
        }
        let has = plan.has_text();
        let (fx, fy, fw, row_h, trect) = match plan.pos {
            Pos::Left if has => {
                let fx = x0 + plan.w + r.title_gap;
                let fw = (avail_w - plan.w - r.title_gap).max(0.0);
                (
                    fx,
                    y,
                    fw,
                    h.max(plan.h),
                    Rect::new(x0, y + r.title_dy, plan.w, plan.h),
                )
            }
            Pos::Top if has => (
                x0,
                y + r.title_top_dy,
                avail_w,
                r.title_top_dy + h,
                Rect::new(x0, y, plan.w.min(avail_w), plan.h),
            ),
            Pos::Right if has => {
                let fw = (avail_w - plan.w - r.title_gap).max(0.0);
                (
                    x0,
                    y,
                    fw,
                    h.max(plan.h),
                    Rect::new(x0 + fw + r.title_gap, y + r.title_dy, plan.w, plan.h),
                )
            }
            _ => (x0, y, avail_w, h, Rect::default()),
        };
        let idx = self.boxes.len();
        self.boxes.push(ItemBox::new(
            BoxKey::Item(it.name.clone()),
            BoxKind::SpreadsheetField,
            Rect::new(fx, fy, fw, h),
            d,
        ));
        self.finish_title(idx, &plan, trect);
        row_h
    }

    pub(super) fn table(&mut self, it: &Item, x0: f32, avail_w: f32, y: f32, d: u16) -> f32 {
        let r = self.r;
        let mut w = if it.props.horizontal_stretch == Some(false) {
            r.table_fixed_w.min(avail_w)
        } else {
            avail_w
        };
        if self.float.is_some_and(|f| f.natural) {
            let mark = self.boxes.len();
            let mut tmp = Vec::new();
            self.columns(it, d.saturating_add(1), &mut tmp);
            self.boxes.truncate(mark);
            w = tmp.iter().map(|c| c.base).sum::<f32>() + 2.0;
            if self.hosted_on_form_bar(it) {
                w = w.max(r.float_list_w);
            }
            w = w.min(r.float_table_max_w).min(avail_w);
            self.nat_need = self.nat_need.max(x0 + w + r.margin_right);
        }
        let bar = it.parts.iter().find(|p| p.kind == ItemKind::AutoCommandBar);
        let bar_name = bar.map_or_else(
            || {
                format!(
                    "{}{}",
                    it.name,
                    std_name("CommandBar", self.input.form.name_lang)
                )
            },
            |b| b.name.clone(),
        );
        let bar_hidden = bar.is_some_and(|b| self.is_hidden(b))
            || it.props.command_bar_location == Some(Location::None)
            || self.hosted_on_form_bar(it);
        let top = if bar_hidden { 0.0 } else { r.table_body_dy };

        let mut cols = Vec::new();
        let grid_idx = self.boxes.len();
        self.boxes.push(ItemBox::new(
            BoxKey::Item(it.name.clone()),
            BoxKind::Grid,
            Rect::default(),
            d,
        ));
        if bar_hidden {
            self.boxes.push(hidden_bar(&bar_name, d));
        } else {
            self.table_bar(it, bar, &bar_name, x0, y, w, d);
        }
        let mut vs = ItemBox::new(
            BoxKey::Part {
                owner: it.name.clone(),
                part: Part::ViewStatus,
            },
            BoxKind::ViewStatus,
            Rect::default(),
            d,
        );
        vs.hidden = true;
        self.boxes.push(vs);
        self.columns(it, d.saturating_add(1), &mut cols);

        let natural = self.table_natural_h(it);
        let mut inner_w = (w - 2.0).max(0.0);
        let sum: f32 = cols.iter().map(|c| c.base).sum();
        if self.float.is_some() && self.hosted_on_form_bar(it) {
            self.float_list = true;
        }
        let stretches = d == 0 && it.props.vertical_stretch != Some(false);
        let h = natural
            + if stretches {
                self.stretch(natural)
            } else {
                0.0
            };
        let inner_h = (h - 2.0).max(0.0);
        let scrolls = sum > inner_w || self.generated_list(it);
        if scrolls {
            self.overflow = true;
        }
        let rows = self
            .input
            .state
            .table_rows
            .get(&it.name)
            .copied()
            .unwrap_or(0);
        let header_h = if it.props.header == Some(false) {
            0.0
        } else {
            r.table_header_h
        };
        if rows as f32 * r.table_row_h > inner_h - header_h {
            inner_w = (inner_w - r.table_scroll_w).max(0.0);
        }
        let ws = widths(&cols, inner_w);
        let mut x = 0.0;
        let columns = cols
            .iter()
            .zip(&ws)
            .map(|(c, w)| {
                let col = GridColumn {
                    name: c.name.clone(),
                    title: c.title.clone(),
                    x,
                    w: *w,
                };
                x += w;
                col
            })
            .collect();
        let g = &mut self.boxes[grid_idx];
        g.rect = Rect::new(x0, y + top, w, h);
        g.hidden = it.props.visible == Some(false);
        g.grid = Some(GridBox {
            inner: Size {
                w: inner_w,
                h: inner_h,
            },
            columns,
            header_h,
            row_h: r.table_row_h,
        });
        top + h
    }

    pub(super) fn hosted_on_form_bar(&self, it: &Item) -> bool {
        let form = self.input.form;
        matches!(stdcmd::form_role(form), stdcmd::FormRole::List { .. })
            && stdcmd::main_table(form).is_some_and(|t| std::ptr::eq(t, it))
            && matches!(
                it.props.command_bar_location,
                None | Some(Location::Auto) | Some(Location::CommandBar)
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn table_bar(
        &mut self,
        it: &Item,
        bar: Option<&Item>,
        bar_name: &str,
        x0: f32,
        y: f32,
        w: f32,
        d: u16,
    ) {
        let r = self.r;
        self.boxes.push(ItemBox::new(
            BoxKey::Item(bar_name.to_string()),
            BoxKind::CommandBar,
            Rect::new(x0, y, w, r.cmd_bar_h),
            d,
        ));
        let mut x = x0;
        let lang = self.input.form.name_lang;
        let button = |cx: &mut Self, cmd: &str, text: Option<&str>, w: f32, x: f32| {
            let mut b = ItemBox::new(
                BoxKey::Part {
                    owner: it.name.clone(),
                    part: Part::TableCommand(cmd.to_string()),
                },
                BoxKind::CommandButton,
                Rect::new(x, y, w, r.button_h),
                d.saturating_add(1),
            );
            b.text = text.map(str::to_string);
            cx.boxes.push(b);
        };
        let cmds: Vec<_> = stdcmd::table_bar_commands(self.input.form, it)
            .into_iter()
            .filter(|c| c.on_bar)
            .collect();
        let mut had_text = false;
        let mut first_pic = true;
        for (i, c) in cmds.iter().enumerate() {
            let key = std_name(c.id, lang);
            if c.picture_only {
                if first_pic && had_text {
                    x += r.table_move_gap;
                }
                first_pic = false;
                button(self, key, None, r.cmd_picture_w, x);
                x += if cmds.get(i + 1).is_some_and(|n| n.picture_only) {
                    r.table_move_step
                } else {
                    r.cmd_picture_w
                };
            } else {
                let text = match command_text(c.id, "ru") {
                    Some(t) => t,
                    None => {
                        self.warn_once(format!(
                            "{}: no caption for standard command {}",
                            it.name, c.id
                        ));
                        key
                    }
                };
                let bw = self.m.width(text, ui_style::FontRole::Text) + r.cmd_pad;
                button(self, key, Some(text), bw, x);
                x += bw;
                had_text = true;
            }
        }
        if let Some(b) = bar {
            let limit = x0 + w - r.table_search_gap - r.table_search_w - r.cmd_gap;
            self.bar_buttons(&b.children, x + r.cmd_gap, y, d.saturating_add(1), limit);
        }
        if it.props.search_string_location != Some(Location::None) {
            self.search_string(it, bar_name, x0 + w, y, d.saturating_add(1));
        }
    }

    pub(super) fn search_string(&mut self, it: &Item, bar_name: &str, right: f32, y: f32, d: u16) {
        let r = self.r;
        let sx = right - r.table_search_gap - r.table_search_w;
        let mut s = ItemBox::new(
            BoxKey::Part {
                owner: it.name.clone(),
                part: Part::SearchString {
                    bar: bar_name.to_string(),
                },
            },
            BoxKind::Field,
            Rect::new(sx, y, r.table_search_w, r.cmd_bar_h),
            d,
        );
        s.input = Some(Rect::new(
            sx + r.input_dx,
            y + r.input_dy,
            (r.table_search_w - r.input_pad - r.button_w).max(0.0),
            (r.cmd_bar_h - r.input_trim).max(0.0),
        ));
        s.buttons = vec![FieldButton::Clear];
        self.boxes.push(s);
    }
}

fn hidden_bar(name: &str, d: u16) -> ItemBox {
    let mut b = ItemBox::new(
        BoxKey::Item(name.to_string()),
        BoxKind::CommandBar,
        Rect::default(),
        d,
    );
    b.hidden = true;
    b
}
