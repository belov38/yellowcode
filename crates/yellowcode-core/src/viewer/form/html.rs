use std::fmt::Write;

use base64::Engine;
use form_layout::Rect;
use form_layout::scene::{Align, Node, Panel, Prim, TextRole};
use form_model::PictureRef;
use ui_style::{FONT_FILES, Interface, PicFormat, Rgb, Style, std_picture};

use super::Preview;

pub enum Resources {
    Inline,
    Urls { prefix: String },
}

pub fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

fn color(name: &str, fallback: Rgb) -> String {
    let Rgb(r, g, b) = Style::get()
        .color(Interface::Taxi, name)
        .unwrap_or(fallback);
    format!("rgb({r},{g},{b})")
}

fn num(v: f32) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let s = format!("{:.2}", (v * 100.0).round() / 100.0);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn base(prefix: &str) -> &str {
    let p = prefix.trim_end_matches('/');
    let ok = prefix.starts_with('/')
        && !prefix.contains("//")
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-'));
    if ok { p } else { "/res" }
}

fn pct(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-') {
            o.push(b as char);
        } else {
            let _ = write!(o, "%{b:02X}");
        }
    }
    o
}

fn font_src(file: &str, bytes: &[u8], res: &Resources) -> String {
    match res {
        Resources::Inline => format!("data:font/ttf;base64,{}", b64(bytes)),
        Resources::Urls { prefix } => format!("{}/font/{file}", base(prefix)),
    }
}

fn css(res: &Resources) -> String {
    let mut s = String::new();
    for (file, bytes) in FONT_FILES.iter() {
        let weight = if file.contains("bold") { 700 } else { 400 };
        let _ = write!(
            s,
            "@font-face{{font-family:\"Liberation Sans\";font-weight:{weight};src:url(\"{}\") format(\"truetype\")}}",
            font_src(file, bytes, res)
        );
    }
    let c = |n: &str, f: (u8, u8, u8)| color(n, Rgb(f.0, f.1, f.2));
    let back = c("ЦветФонаФормы", (255, 255, 255));
    let text = c("ЦветТекстаФормы", (51, 51, 51));
    let border = c("ЦветРамки", (160, 160, 160));
    let field = c("ЦветФонаПоля", (255, 255, 255));
    let btn_back = c("ЦветФонаКнопки", (255, 255, 255));
    let btn_border = c("ЦветРамкиКнопки", (178, 178, 178));
    let btn_text = c("ЦветТекстаКнопки", (77, 77, 77));
    let head_back = c("ЦветФонаШапкиТаблицы", (250, 250, 250));
    let head_text = c("ЦветТекстаШапкиТаблицы", (77, 77, 77));
    let tab_back = {
        let Rgb(r, g, b) = Style::get()
            .color(Interface::Taxi, "ЦветФонаШапкиТаблицы")
            .unwrap_or(Rgb(250, 250, 250));
        let d = |v: u8| (f32::from(v) * 0.93) as u8;
        format!("rgb({},{},{})", d(r), d(g), d(b))
    };
    let strong = c("АльтернативныйЦветФонаПоля", (245, 251, 247));
    let link = c("ЦветФонаВыделенияПоля", (83, 106, 194));
    let _ = write!(
        s,
        "body{{margin:0;background:{back};color:{text};font:13.333px/16px \"Liberation Sans\",Arial,sans-serif}}\
.f{{position:relative;overflow:hidden}}\
.win{{position:absolute;box-sizing:border-box;overflow:hidden;border:1px solid {border};background:{back};box-shadow:0 2px 8px rgba(0,0,0,.25)}}\
.n{{position:absolute;box-sizing:border-box;white-space:pre;overflow:hidden;margin:0}}\
.fld{{border:1px solid {border};border-radius:3px;background:{field}}}\
.ro{{background:{head_back}}}.dis{{opacity:.5}}\
.btn{{border:1px solid {btn_border};border-radius:2px;background:{btn_back}}}\
.cmd{{border:1px solid {btn_border};border-radius:3px;background:{btn_back}}}\
.fbtn{{border:1px solid {border};border-radius:3px;background:{btn_back}}}\
.arrow{{color:{btn_text};text-align:center;font-size:9px}}\
.bar{{background:transparent}}\
.grp{{border-top:1px solid {border}}}.strong{{background:{strong}}}\
.tabs{{border:1px solid {border}}}\
.tab{{border:1px solid {border};background:{tab_back}}}.tab.on{{background:{back};border-bottom-color:{back}}}\
.grid{{border:1px solid {border};background:{field}}}\
.gh{{background:{head_back}}}.gc{{border-right:1px solid {border}}}\
.chk{{border:1px solid {border};border-radius:2px;background:{field}}}\
.rad{{border:1px solid {border};border-radius:50%;background:{field}}}\
.seg{{border:1px solid {btn_border};background:{btn_back}}}.seg.first{{border-radius:3px 0 0 3px}}\
.sw{{border:1px solid {border};border-radius:10px;background:{btn_back}}}\
.ph{{border:1px dashed {border};background:{head_back};color:{border};text-align:center}}\
.t{{color:{text}}}.b{{font-weight:700}}.h{{color:{border}}}.l{{color:{link};text-decoration:underline}}\
.hd{{color:{head_text}}}.bt{{color:{btn_text}}}\
.g{{color:{border};text-align:center}}\
.ta-c{{text-align:center}}.ta-r{{text-align:right}}\
img.n{{object-fit:contain}}"
    );
    s
}

fn style(r: Rect, dy: f32, extra: &str) -> String {
    format!(
        "left:{}px;top:{}px;width:{}px;height:{}px{extra}",
        num(r.x),
        num(r.y + dy),
        num(r.w.max(0.0)),
        num(r.h.max(0.0))
    )
}

fn panel_class(p: Panel) -> String {
    match p {
        Panel::FormCommandBar | Panel::CommandBar => "bar".into(),
        Panel::GroupFrame => "grp".into(),
        Panel::GroupStrong => "strong".into(),
        Panel::TabsFrame => "tabs".into(),
        Panel::Tab { active } => if active { "tab on" } else { "tab" }.into(),
        Panel::GridFrame => "grid".into(),
        Panel::GridHeader => "gh".into(),
        Panel::GridHeaderCell => "gc".into(),
        Panel::Field {
            read_only,
            disabled,
        } => {
            let mut c = String::from("fld");
            if read_only {
                c.push_str(" ro");
            }
            if disabled {
                c.push_str(" dis");
            }
            c
        }
        Panel::Button { default } => if default { "btn b" } else { "btn" }.into(),
        Panel::CommandButton => "cmd".into(),
        Panel::FieldButton => "fbtn".into(),
        Panel::CheckBox => "chk".into(),
        Panel::Radio => "rad".into(),
        Panel::ToggleSegment { first } => if first { "seg first" } else { "seg" }.into(),
        Panel::Switch => "sw".into(),
        Panel::Placeholder => "ph".into(),
    }
}

fn glyph(g: form_layout::FieldButton) -> &'static str {
    use form_layout::FieldButton as B;
    match g {
        B::Choice => "\u{2026}",
        B::DropList => "\u{25BC}",
        B::Clear => "\u{00D7}",
        B::Open => "\u{00BB}",
        B::SpinUp => "\u{25B2}",
        B::SpinDown => "\u{25BC}",
    }
}

fn placeholder(out: &mut String, r: Rect, dy: f32, name: &str) {
    let _ = write!(
        out,
        "<div class=\"n ph\" style=\"{}\">{}</div>",
        style(r, dy, ";font-size:10px;line-height:12px"),
        escape(name)
    );
}

fn picture(
    out: &mut String,
    n: &Node,
    dy: f32,
    reference: &PictureRef,
    res: &Resources,
    preview: &Preview,
) {
    if let PictureRef::Std(name) = reference
        && let Some(p) = std_picture(name)
    {
        let src = match res {
            Resources::Inline => {
                let mime = match p.format {
                    PicFormat::Png => "image/png",
                    PicFormat::Svg => "image/svg+xml",
                };
                format!("data:{mime};base64,{}", b64(p.bytes))
            }
            Resources::Urls { prefix } => {
                format!("{}/pic/{}", base(prefix), pct(&p.info.en))
            }
        };
        let _ = write!(
            out,
            "<img class=\"n\" alt=\"{}\" src=\"{}\" style=\"{}\">",
            escape(name),
            src,
            style(n.rect, dy, "")
        );
        return;
    }
    let name = match reference {
        PictureRef::Std(s) | PictureRef::Config(s) | PictureRef::Embedded(s) => s,
    };
    if let Some(asset) = preview.pictures.get(&super::picture_key(reference)) {
        let src = match (res, reference) {
            (Resources::Urls { prefix }, PictureRef::Config(name)) => {
                format!("{}/cpic/{}", base(prefix), pct(name))
            }
            (Resources::Urls { prefix }, PictureRef::Embedded(name))
                if preview.form_path.is_some() =>
            {
                format!(
                    "{}/fpic/{}?path={}",
                    base(prefix),
                    pct(name),
                    pct(preview.form_path.as_deref().unwrap_or_default())
                )
            }
            _ => format!("data:{};base64,{}", asset.mime, b64(&asset.bytes)),
        };
        let _ = write!(
            out,
            "<img class=\"n\" alt=\"{}\" src=\"{}\" style=\"{}\">",
            escape(name),
            escape(&src),
            style(n.rect, dy, "")
        );
    } else {
        placeholder(out, n.rect, dy, name);
    }
}

pub fn document(p: &Preview, res: &Resources) -> String {
    let nodes = &p.scene.nodes;
    let min_y = nodes.iter().map(|n| n.rect.y).fold(0.0f32, f32::min);
    let dy = -min_y;
    let mut body = String::new();
    for n in nodes {
        match &n.prim {
            Prim::Panel(pn) => {
                let _ = write!(
                    body,
                    "<div class=\"n {}\" style=\"{}\"></div>",
                    panel_class(*pn),
                    style(n.rect, dy, "")
                );
            }
            Prim::Text { text, role, align } => {
                let mut cls = String::from("n t");
                match role {
                    TextRole::Normal => {}
                    TextRole::Bold => cls.push_str(" b"),
                    TextRole::Hyperlink => cls.push_str(" l"),
                    TextRole::Hint => cls.push_str(" h"),
                    TextRole::Header => cls.push_str(" hd"),
                }
                match align {
                    Align::Left => {}
                    Align::Center => cls.push_str(" ta-c"),
                    Align::Right => cls.push_str(" ta-r"),
                }
                let lh = if text.contains('\n') || n.rect.h > 30.0 {
                    16.0
                } else {
                    n.rect.h
                };
                let _ = write!(
                    body,
                    "<div class=\"{cls}\" style=\"{}\">{}</div>",
                    style(n.rect, dy, &format!(";line-height:{}px", num(lh))),
                    escape(text)
                );
            }
            Prim::Picture { reference } => picture(&mut body, n, dy, reference, res, p),
            Prim::MoreArrow => {
                let _ = write!(
                    body,
                    "<div class=\"n arrow\" style=\"{}\">\u{25BC}</div>",
                    style(n.rect, dy, &format!(";line-height:{}px", num(n.rect.h)))
                );
            }
            Prim::Glyph(g) => {
                let _ = write!(
                    body,
                    "<div class=\"n g\" style=\"{}\">{}</div>",
                    style(n.rect, dy, &format!(";line-height:{}px", num(n.rect.h))),
                    glyph(*g)
                );
            }
        }
    }
    let g = &p.layout.window;
    let form = if g.floating {
        let (wr, ws, cr) = (g.window, g.workspace, g.content);
        format!(
            "<div class=\"f\" style=\"width:{}px;height:{}px\">\
<div class=\"win\" style=\"left:{}px;top:{}px;width:{}px;height:{}px\">\
<div class=\"f\" style=\"position:absolute;left:{}px;top:{}px;width:{}px;height:{}px\">{}</div></div></div>",
            num(ws.w),
            num(ws.h),
            num(wr.x - ws.x),
            num(wr.y - ws.y),
            num(wr.w),
            num(wr.h),
            num(cr.x - wr.x - 1.0),
            num(cr.y - wr.y - 1.0 - dy),
            num(p.scene.w),
            num(p.scene.h + dy),
            body
        )
    } else {
        format!(
            "<div class=\"f\" style=\"width:{}px;height:{}px\">{}</div>",
            num(p.scene.w),
            num(p.scene.h + dy),
            body
        )
    };
    format!(
        "<!doctype html><html lang=\"ru\"><head><meta charset=\"utf-8\">\n\
<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src 'self' data:; font-src 'self' data:; style-src 'unsafe-inline'\">\n\
<title>{}</title>\n<style>{}</style></head>\n\
<body>{}</body></html>\n",
        escape(&p.title),
        css(res),
        form
    )
}
