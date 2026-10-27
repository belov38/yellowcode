use super::chart::{FORM_DECLARED, write_chart};
use super::values::{GraphError, R, at, bad, is, item, list, py_space, slice};
use crate::templates::context::Lookup;
use crate::templates::mxlxml::Writer;
use crate::templates::{moxel, moxel_view::MoxelView, tree::TreeError};
use crate::xml_out::XmlWriter;
use brace::Node;

pub(crate) const CHART_TYPE: &str = "3543ef08-3316-4f7e-9447-0cd0a1cbf1d5";
pub(crate) const SHEET_TYPE: &str = "e603103e-a318-4edc-a014-b1c6cf94d49f";
const CHART_NS: &str = "http://v8.1c.ru/8.2/data/chart";
const MXL_NS: &str = "http://v8.1c.ru/8.2/data/spreadsheet";

pub(crate) fn kind(value: &Node) -> Option<&'static str> {
    let v = value.as_list()?;
    if v.len() < 3 || !is(v.first()?, "#") {
        return None;
    }
    match v.get(1)?.text()? {
        CHART_TYPE => Some("Chart"),
        SHEET_TYPE => Some("SpreadsheetDocument"),
        _ => None,
    }
}

pub(crate) fn settings_lines(cfg: &dyn Lookup, value: &Node, depth: usize) -> R<Vec<String>> {
    match kind(value) {
        Some("Chart") => {
            let v = list(value)?;
            let p = format!("d{}p1", depth + 1);
            let xmlns = format!("xmlns:{p}");
            let xsi = format!("{p}:Chart");
            let mut w = XmlWriter::new(depth);
            w.open("Settings", &[(&xmlns, CHART_NS), ("xsi:type", &xsi)]);
            write_chart(&mut w, slice(v, 2, 4), &p, cfg, FORM_DECLARED)?;
            w.close("Settings");
            Ok(w.text().split("\r\n").map(str::to_string).collect())
        }
        Some("SpreadsheetDocument") => sheet_lines(cfg, value, depth),
        _ => bad(format!(
            "settings value {}",
            brace::to_compact(value.as_list().and_then(|v| v.get(1)).unwrap_or(value))
        )),
    }
}

fn index_of(body: &[&str], line: &str) -> R<i64> {
    let i = body
        .iter()
        .position(|l| *l == line)
        .ok_or_else(|| GraphError(format!("{line} not found")))?;
    i64::try_from(i).map_err(|_| GraphError("line index".into()))
}

fn sheet_lines(cfg: &dyn Lookup, value: &Node, depth: usize) -> R<Vec<String>> {
    let doc = MoxelView::read(list(item(value, 2)?)?, moxel::HEAD)
        .map_err(|error| GraphError(moxel::MoxelError::from(TreeError::from(error)).0))?;
    let text = Writer::from_view(cfg, doc)?.run()?;
    let all: Vec<&str> = text.split("\r\n").collect();
    let mut body: Vec<&str> = slice(&all, 2, -1).to_vec();
    if is(item(doc.lang, 0)?, "#") {
        let i = index_of(&body, "\t<languageSettings>")?;
        let j = index_of(&body, "\t</languageSettings>")?;
        let mut kept = slice(&body, 0, i).to_vec();
        kept.extend_from_slice(slice(&body, j.saturating_add(1), i64::MAX));
        body = kept;
    }
    if is(at(doc.u13, 0)?, "0") {
        body.retain(|x| *x != "\t<templateMode>true</templateMode>");
    }
    if doc.rows.is_empty() && doc.height == 0 {
        body.retain(|x| *x != "\t<vgRows>0</vgRows>");
    }
    let ind = "\t".repeat(depth);
    let mut out = vec![format!(
        "{ind}<Settings xmlns:mxl=\"{MXL_NS}\" xsi:type=\"mxl:SpreadsheetDocument\">"
    )];
    for ln in body {
        out.push(format!("{ind}{}", named_items(&mxl_tags(ln))));
    }
    out.push(format!("{ind}</Settings>"));
    Ok(out)
}

fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn mxl_tags(ln: &str) -> String {
    let cs: Vec<char> = ln.chars().collect();
    let mut out = String::with_capacity(ln.len() + 16);
    let mut i = 0;
    while let Some(&c) = cs.get(i) {
        if c == '<' {
            let mut j = i + 1;
            let slash = cs.get(j) == Some(&'/');
            if slash {
                j += 1;
            }
            if cs.get(j).is_some_and(char::is_ascii_alphabetic) {
                let mut k = j + 1;
                while cs.get(k).is_some_and(|&x| word(x)) {
                    k += 1;
                }
                if cs
                    .get(k)
                    .is_some_and(|&x| py_space(x) || x == '/' || x == '>')
                {
                    out.push('<');
                    if slash {
                        out.push('/');
                    }
                    out.push_str("mxl:");
                    out.extend(cs.get(j..k).unwrap_or_default());
                    i = k;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn named_items(ln: &str) -> String {
    const PAT: &str = "xsi:type=\"NamedItem";
    let mut out = String::with_capacity(ln.len() + 8);
    let mut rest = ln;
    while let Some(i) = rest.find(PAT) {
        let (before, from) = rest.split_at(i);
        out.push_str(before);
        let after = from.get(PAT.len()..).unwrap_or_default();
        let n: usize = after
            .chars()
            .take_while(|&c| word(c))
            .map(char::len_utf8)
            .sum();
        if n > 0 && after.get(n..).is_some_and(|t| t.starts_with('"')) {
            out.push_str("xsi:type=\"mxl:NamedItem");
            out.push_str(after.get(..=n).unwrap_or_default());
            rest = after.get(n + 1..).unwrap_or_default();
        } else {
            let c = from.chars().next().map_or(1, char::len_utf8);
            out.push_str(from.get(..c).unwrap_or_default());
            rest = from.get(c..).unwrap_or_default();
        }
    }
    out.push_str(rest);
    out
}
