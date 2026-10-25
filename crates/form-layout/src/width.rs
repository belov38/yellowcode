use crate::FieldButton;
use crate::rules::Rules;
use form_model::{DateParts, Props, TypeKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeWidth {
    pub chars: u32,
    pub buttons: Vec<FieldButton>,
    pub stretch: bool,
    pub align_right: bool,
    pub inside: bool,
}

impl TypeWidth {
    pub fn slots(&self) -> usize {
        let spin = self
            .buttons
            .iter()
            .any(|b| matches!(b, FieldButton::SpinUp | FieldButton::SpinDown));
        self.buttons
            .iter()
            .filter(|b| !matches!(b, FieldButton::SpinUp | FieldButton::SpinDown))
            .count()
            + usize::from(spin)
    }
}

fn button_rank(b: FieldButton) -> u8 {
    match b {
        FieldButton::DropList => 0,
        FieldButton::Choice => 1,
        FieldButton::Clear => 2,
        FieldButton::SpinUp => 3,
        FieldButton::SpinDown => 4,
        FieldButton::Open => 5,
    }
}

pub fn apply_button_props(tw: &mut TypeWidth, p: &Props) {
    use FieldButton::*;
    let mut set = |on: Option<bool>, bs: &[FieldButton]| {
        let Some(on) = on else { return };
        for b in bs {
            tw.buttons.retain(|x| x != b);
            if on {
                tw.buttons.push(*b);
            }
        }
    };
    set(p.drop_list_button, &[DropList]);
    set(p.choice_button, &[Choice]);
    set(p.clear_button, &[Clear]);
    set(p.spin_button, &[SpinUp, SpinDown]);
    set(p.open_button, &[Open]);
    tw.buttons.sort_by_key(|b| button_rank(*b));
}

const NUMBER_POINTS: &[(u32, u32, u32)] = &[
    (1, 0, 2),
    (2, 0, 3),
    (3, 0, 3),
    (4, 0, 5),
    (5, 0, 5),
    (6, 0, 6),
    (8, 0, 8),
    (10, 0, 10),
    (12, 0, 11),
    (15, 0, 14),
    (20, 0, 19),
    (25, 0, 23),
    (30, 0, 27),
    (38, 0, 35),
    (2, 1, 3),
    (4, 2, 5),
    (6, 2, 7),
    (10, 2, 10),
    (12, 2, 12),
    (15, 2, 15),
    (20, 2, 19),
    (8, 3, 8),
    (15, 3, 14),
    (20, 4, 19),
    (25, 5, 23),
    (38, 8, 34),
    (30, 10, 26),
];

pub fn number_chars(digits: u32, fraction: u32) -> u32 {
    if digits == 0 {
        return 10;
    }
    if let Some(p) = NUMBER_POINTS
        .iter()
        .find(|p| p.0 == digits && p.1 == fraction)
    {
        return p.2;
    }
    let pick = |f: u32| -> Vec<(u32, u32)> {
        let mut v: Vec<(u32, u32)> = NUMBER_POINTS
            .iter()
            .filter(|p| p.1 == f)
            .map(|p| (p.0, p.2))
            .collect();
        v.sort_unstable();
        v
    };
    let mut pts = pick(fraction);
    if pts.is_empty() {
        pts = pick(0);
    }
    let (first, last) = (pts[0], pts[pts.len() - 1]);
    if digits <= first.0 {
        return first.1.min(digits).max(1);
    }
    if digits >= last.0 {
        return last.1 + (digits - last.0);
    }
    let i = pts.partition_point(|p| p.0 <= digits);
    let (a, b) = (pts[i - 1], pts[i]);
    a.1 + (b.1 - a.1) * (digits - a.0) / (b.0 - a.0)
}

pub fn type_width(kind: &TypeKind) -> TypeWidth {
    use FieldButton::*;
    let mut inside = false;
    let (chars, buttons, stretch, align_right) = match kind {
        TypeKind::String(k) => {
            let chars = if *k == 0 { 15 } else { (*k).min(40) };
            (chars, vec![], *k == 0 || *k > 10, false)
        }
        TypeKind::Number { digits, fraction } => (
            number_chars(*digits, *fraction),
            if *fraction > 0 { vec![Choice] } else { vec![] },
            false,
            true,
        ),
        TypeKind::Boolean => (10, vec![DropList], false, false),
        TypeKind::Date(DateParts::Date) => (9, vec![Choice], false, false),
        TypeKind::Date(DateParts::Time) => (8, vec![], false, false),
        TypeKind::Date(DateParts::DateTime) => (15, vec![Choice], false, false),
        TypeKind::Ref(_) => (15, vec![DropList, Open], true, false),
        TypeKind::Enum(_) => (15, vec![DropList], true, false),
        TypeKind::Composite => (15, vec![Choice], true, false),
        TypeKind::None => {
            inside = true;
            (15, vec![Choice], true, false)
        }
        _ => (15, vec![], true, false),
    };
    TypeWidth {
        chars,
        buttons,
        stretch,
        align_right,
        inside,
    }
}

pub fn natural_width(p: &Props, tw: &TypeWidth, r: &Rules) -> f32 {
    let k = if tw.inside { 0 } else { tw.slots() } as f32;
    let chars = p.width.filter(|w| *w > 0).unwrap_or(tw.chars) as f32;
    r.char_w * chars + r.field_base + r.button_w * k
}

pub fn field_width(p: &Props, tw: &TypeWidth, avail: f32, r: &Rules) -> (f32, bool) {
    let avail = avail.max(0.0);
    let stretched = p.horizontal_stretch.unwrap_or(tw.stretch);
    let k = if tw.inside { 0 } else { tw.slots() } as f32;
    let chars = p.width.filter(|w| *w > 0).unwrap_or(tw.chars) as f32;
    let natural = r.char_w * chars + r.field_base + r.button_w * k;
    if !stretched {
        return (natural, false);
    }
    let w = match (p.auto_max_width, p.max_width) {
        (Some(false), Some(m)) if m > 0 => {
            r.char_w * m as f32 + r.stretch_base + if m <= 10 { 1.0 } else { 0.0 }
        }
        (Some(false), _) => avail,
        _ => natural.max(r.char_w * r.stretch_chars as f32 + r.stretch_base),
    };
    (w.min(avail), true)
}

pub fn label_width(p: &Props, tw: &TypeWidth, avail: f32, r: &Rules) -> (f32, bool) {
    let avail = avail.max(0.0);
    let stretched = p.horizontal_stretch.unwrap_or(tw.stretch);
    let chars = p.width.filter(|w| *w > 0).unwrap_or(tw.chars);
    let chars = if stretched {
        chars.max(r.stretch_chars)
    } else {
        chars
    };
    ((r.char_w * chars as f32).min(avail), stretched)
}
