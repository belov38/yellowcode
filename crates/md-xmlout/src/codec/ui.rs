use brace::Node;
use mdschema::Schema;

pub const PALETTE: &[(&str, &str)] = &[
    ("0", "FirstBrand"),
    ("1", "SecondBrand"),
    ("2", "Red"),
    ("3", "Orange"),
    ("4", "Yellow"),
    ("5", "Green"),
    ("6", "LightBlue"),
    ("7", "Blue"),
    ("8", "Violet"),
    ("9", "Purple"),
    ("10", "Pink"),
    ("15", "Gray"),
];

type Attrs = Vec<(String, String)>;
type Children = Vec<(String, String, String)>;

const T_BORDER_ENUM: &str = "48312c09-257f-4b29-b280-284dd89efc1e";

fn int(n: &Node) -> Option<i64> {
    n.text()?.trim().parse().ok()
}

fn flag(n: &Node) -> Option<&'static str> {
    Some(if n.text()? == "1" { "true" } else { "false" })
}

fn bold(n: &Node) -> Option<&'static str> {
    Some(if int(n)? >= 700 { "true" } else { "false" })
}

pub fn height(n: &Node) -> Option<String> {
    let v = int(n)?;
    Some(if v.rem_euclid(10) == 0 {
        (v.div_euclid(10)).to_string()
    } else {
        format!("{}", v as f64 / 10.0)
    })
}

fn push(a: &mut Attrs, k: &str, v: impl Into<String>) {
    a.push((k.to_string(), v.into()));
}

pub fn font_attrs(v: &Node, s: &Schema) -> Option<Attrs> {
    let l = v.as_list()?;
    if l.first()?.text()? != "8" {
        return None;
    }
    let kind = l.get(1)?.text()?;
    let mask = int(l.get(2)?)?;
    let mut a: Attrs = Vec::new();
    if kind == "0" {
        let f = l.get(3..)?;
        push(
            &mut a,
            "faceName",
            f.get(13).and_then(Node::text).unwrap_or(""),
        );
        push(&mut a, "height", height(f.first()?)?);
        push(&mut a, "bold", bold(f.get(4)?)?);
        push(&mut a, "italic", flag(f.get(5)?)?);
        push(&mut a, "underline", flag(f.get(6)?)?);
        push(&mut a, "strikeout", flag(f.get(7)?)?);
        push(&mut a, "kind", "Absolute");
        push(&mut a, "scale", f.get(15)?.text()?);
        return Some(a);
    }
    if !matches!(kind, "1" | "2" | "3") {
        return None;
    }
    let mut i = 3;
    if kind != "3" {
        let rid = l.get(3)?.as_list()?.first()?.text()?;
        let t = s.ui_tables();
        let table = if kind == "2" {
            &t.style_fonts
        } else {
            &t.windows_fonts
        };
        let nm = table.get(rid)?;
        let prefix = if kind == "2" { "style:" } else { "sys:" };
        push(&mut a, "ref", format!("{prefix}{nm}"));
        i = 4;
    }
    let mut face = None;
    if mask & 2 != 0 {
        push(&mut a, "height", height(l.get(i)?)?);
        i += 1;
    }
    if mask & 4 != 0 {
        push(&mut a, "bold", bold(l.get(i)?)?);
        i += 1;
    }
    for bit in [8, 16, 32] {
        if mask & bit != 0 {
            let k = match bit {
                8 => "italic",
                16 => "underline",
                _ => "strikeout",
            };
            push(&mut a, k, flag(l.get(i)?)?);
            i += 1;
        }
    }
    if mask & 1 != 0 {
        face = Some(l.get(i)?.text()?.to_string());
        i += 1;
    }
    if let Some(f) = face {
        let at = usize::from(kind != "3");
        a.insert(at, ("faceName".to_string(), f));
    }
    push(
        &mut a,
        "kind",
        match kind {
            "1" => "WindowsFont",
            "2" => "StyleItem",
            _ => "AutoFont",
        },
    );
    if mask & 512 != 0 {
        push(&mut a, "scale", l.get(i + 1)?.text()?);
    }
    if mask & 256 != 0 {
        push(&mut a, "lineHeight", l.get(i + 2)?.text()?);
    }
    Some(a)
}

pub fn border(v: &Node, s: &Schema) -> Option<(Attrs, Children)> {
    let l = v.as_list()?;
    if l.first()?.text()? != "3" {
        return None;
    }
    let t = s.ui_tables();
    match l.get(1)?.text()? {
        "1" => {
            let nm = t.style_borders.get(l.get(2)?.as_list()?.first()?.text()?)?;
            Some((vec![("ref".to_string(), format!("style:{nm}"))], vec![]))
        }
        "0" => {
            if l.get(6)?.text()? != T_BORDER_ENUM {
                return None;
            }
            let st = t.border_styles.get(l.get(3)?.text()?)?;
            let width = l.get(4)?.text()?;
            Some((
                vec![("width".to_string(), width.to_string())],
                vec![(
                    "v8ui:style".into(),
                    "v8ui:ControlBorderType".into(),
                    st.clone(),
                )],
            ))
        }
        _ => None,
    }
}

pub fn color_text(v: &Node, s: &Schema) -> Option<String> {
    color_with(v, |kind, val| match kind {
        "1" => Some(format!("win:{}", s.ui_tables().win_colors.get(val)?)),
        "2" => Some(format!("web:{}", s.ui().web_colors.get(val)?)),
        "3" => Some(format!("style:{}", s.ui_tables().style_colors.get(val)?)),
        _ => None,
    })
}

pub fn color_with(v: &Node, table: impl Fn(&str, &str) -> Option<String>) -> Option<String> {
    let l = v.as_list()?;
    let kind = l.get(1)?.text()?;
    let val = match l.get(2)? {
        Node::List(x) => x.first().and_then(Node::text).unwrap_or(""),
        _ => "",
    };
    match kind {
        "4" => {
            if l.get(3).and_then(Node::text) == Some("5") {
                let (_, nm) = PALETTE.iter().find(|(k, _)| *k == val)?;
                return Some(format!("pal:{nm}"));
            }
            Some("auto".to_string())
        }
        "0" => {
            let n: i64 = val.trim().parse().ok()?;
            Some(format!(
                "#{:02X}{:02X}{:02X}",
                n & 255,
                (n >> 8) & 255,
                (n >> 16) & 255
            ))
        }
        _ => table(kind, val),
    }
}
