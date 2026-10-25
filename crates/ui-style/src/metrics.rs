use crate::Interface;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum FontRole {
    Text,
    TextBold,
}

#[derive(serde::Deserialize)]
struct Font {
    upem: f32,
    notdef: f32,
    ranges: Vec<(u32, Vec<u16>)>,
}

#[derive(serde::Deserialize)]
struct Face {
    px: f32,
    line: f32,
    text: usize,
    bold: usize,
}

#[derive(serde::Deserialize)]
struct Raw {
    fonts: Vec<Font>,
    interfaces: std::collections::BTreeMap<String, Face>,
}

pub struct Metrics {
    text: Font,
    bold: Font,
    px: f32,
    line: f32,
}

static TAXI: OnceLock<Metrics> = OnceLock::new();

impl Font {
    fn adv(&self, c: char) -> f32 {
        let u = c as u32;
        if u == 10 {
            return 0.0;
        }
        for (start, advs) in &self.ranges {
            if u >= *start
                && let Some(a) = advs.get((u - start) as usize)
            {
                return f32::from(*a);
            }
        }
        self.notdef
    }
}

impl Metrics {
    #[allow(
        clippy::expect_used,
        reason = "embedded generated data/metrics.json; parsed by the crate tests"
    )]
    pub fn get(iface: Interface) -> Option<&'static Metrics> {
        match iface {
            Interface::Taxi => Some(TAXI.get_or_init(|| {
                let mut raw: Raw = serde_json::from_str(include_str!("../data/metrics.json"))
                    .expect("data/metrics.json is generated and valid");
                let face = raw.interfaces.remove("taxi").expect("taxi in metrics.json");
                let mut fonts: Vec<Option<Font>> = raw.fonts.into_iter().map(Some).collect();
                Metrics {
                    text: fonts[face.text].take().expect("text font"),
                    bold: fonts[face.bold].take().expect("bold font"),
                    px: face.px,
                    line: face.line,
                }
            })),
            Interface::V85 => None,
        }
    }

    fn font(&self, role: FontRole) -> &Font {
        match role {
            FontRole::Text => &self.text,
            FontRole::TextBold => &self.bold,
        }
    }

    pub fn font_px(&self, _role: FontRole) -> f32 {
        self.px
    }

    pub fn line_height(&self, _role: FontRole) -> f32 {
        self.line
    }

    fn char_w(&self, c: char, role: FontRole) -> f32 {
        let f = self.font(role);
        f.adv(c) * self.px / f.upem
    }

    pub fn width(&self, text: &str, role: FontRole) -> f32 {
        text.chars().map(|c| self.char_w(c, role)).sum()
    }

    pub fn wrap(&self, text: &str, role: FontRole, max_w: f32, max_lines: usize) -> Vec<String> {
        let space = self.char_w(' ', role);
        let mut lines: Vec<String> = Vec::new();
        for para in text.split('\n') {
            let mut cur = String::new();
            let mut cur_w = 0.0f32;
            let mut any = false;
            for word in para.split(' ').filter(|w| !w.is_empty()) {
                any = true;
                let ww = self.width(word, role);
                if !cur.is_empty() && cur_w + space + ww <= max_w {
                    cur.push(' ');
                    cur.push_str(word);
                    cur_w += space + ww;
                    continue;
                }
                if !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                }
                if ww <= max_w {
                    cur.push_str(word);
                    cur_w = ww;
                    continue;
                }
                cur_w = 0.0;
                for c in word.chars() {
                    let cw = self.char_w(c, role);
                    if !cur.is_empty() && cur_w + cw > max_w {
                        lines.push(std::mem::take(&mut cur));
                        cur_w = 0.0;
                    }
                    cur.push(c);
                    cur_w += cw;
                }
            }
            if !cur.is_empty() || !any {
                lines.push(cur);
            }
        }
        if max_lines > 0 && lines.len() > max_lines {
            let rest = lines.split_off(max_lines - 1).join(" ");
            lines.push(rest);
        }
        lines
    }
}
