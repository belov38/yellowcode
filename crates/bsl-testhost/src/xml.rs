use crate::scan::ScanError;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::path::Path;

pub(crate) enum Ev<'a> {
    Start(&'a [(String, String)]),
    Text(&'a str),
}

fn local(e: &BytesStart<'_>) -> String {
    String::from_utf8_lossy(e.local_name().as_ref()).into_owned()
}

fn attrs(e: &BytesStart<'_>) -> Vec<(String, String)> {
    e.attributes()
        .flatten()
        .map(|a| {
            let k = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
            let v = a
                .unescape_value()
                .map(|v| v.into_owned())
                .unwrap_or_default();
            (k, v)
        })
        .collect()
}

pub(crate) fn scan_file(
    path: &Path,
    mut f: impl FnMut(&[String], Ev<'_>),
) -> Result<(), ScanError> {
    let bytes = std::fs::read(path).map_err(|e| ScanError::new(path, e.to_string()))?;
    let mut rd = Reader::from_reader(bytes.as_slice());
    rd.config_mut().trim_text(true);
    let mut stack: Vec<String> = Vec::new();
    let mut buf = Vec::new();
    loop {
        let ev = rd
            .read_event_into(&mut buf)
            .map_err(|e| ScanError::new(path, format!("XML: {e}")))?;
        match ev {
            Event::Start(e) => {
                stack.push(local(&e));
                f(&stack, Ev::Start(&attrs(&e)));
            }
            Event::Empty(e) => {
                stack.push(local(&e));
                f(&stack, Ev::Start(&attrs(&e)));
                stack.pop();
            }
            Event::End(_) => {
                stack.pop();
            }
            Event::Text(t) => {
                let s = t
                    .unescape()
                    .map_err(|e| ScanError::new(path, format!("XML: {e}")))?;
                f(&stack, Ev::Text(&s));
            }
            Event::CData(t) => {
                let s = String::from_utf8_lossy(&t).into_owned();
                f(&stack, Ev::Text(&s));
            }
            Event::Eof => {
                if !stack.is_empty() {
                    return Err(ScanError::new(path, "XML: unexpected end of file"));
                }
                return Ok(());
            }
            _ => {}
        }
        buf.clear();
    }
}

fn is(stack: &[String], want: &[&str]) -> bool {
    stack.len() == want.len() && stack.iter().zip(want).all(|(a, b)| a == b)
}

pub(crate) fn config_children(path: &Path) -> Result<Vec<(String, String)>, ScanError> {
    let mut out = Vec::new();
    scan_file(path, |st, ev| {
        if let Ev::Text(t) = ev
            && st.len() == 4
            && is(
                &st[..3],
                &["MetaDataObject", "Configuration", "ChildObjects"],
            )
        {
            out.push((st[3].clone(), t.to_string()));
        }
    })?;
    Ok(out)
}

pub(crate) fn properties(path: &Path) -> Result<Vec<(String, String)>, ScanError> {
    let mut out = Vec::new();
    scan_file(path, |st, ev| {
        if let Ev::Text(t) = ev
            && st.len() == 4
            && st.first().map(String::as_str) == Some("MetaDataObject")
            && st.get(2).map(String::as_str) == Some("Properties")
        {
            out.push((st[3].clone(), t.to_string()));
        }
    })?;
    Ok(out)
}

#[derive(Default)]
pub(crate) struct ObjectNames {
    pub fields: Vec<String>,
    pub enum_values: Vec<String>,
}

pub(crate) fn object_names(path: &Path) -> Result<ObjectNames, ScanError> {
    const FIELDS: [&str; 6] = [
        "Attribute",
        "TabularSection",
        "Dimension",
        "Resource",
        "AddressingAttribute",
        "AccountingFlag",
    ];
    let mut out = ObjectNames::default();
    scan_file(path, |st, ev| {
        if let Ev::Text(t) = ev
            && st.len() == 6
            && st[0] == "MetaDataObject"
            && st[2] == "ChildObjects"
            && st[4] == "Properties"
            && st[5] == "Name"
        {
            if FIELDS.contains(&st[3].as_str()) {
                out.fields.push(t.to_string());
            } else if st[3] == "EnumValue" {
                out.enum_values.push(t.to_string());
            }
        }
    })?;
    Ok(out)
}

pub(crate) fn predefined_names(path: &Path) -> Result<Vec<String>, ScanError> {
    let mut out = Vec::new();
    scan_file(path, |st, ev| {
        if let Ev::Text(t) = ev
            && st.first().map(String::as_str) == Some("PredefinedData")
            && st.len() >= 3
            && st[st.len() - 2] == "Item"
            && st[st.len() - 1] == "Name"
        {
            out.push(t.to_string());
        }
    })?;
    Ok(out)
}

#[derive(Default)]
pub(crate) struct FormNames {
    pub attributes: Vec<String>,
    pub main_type: Option<String>,
}

pub(crate) fn form_names(path: &Path) -> Result<FormNames, ScanError> {
    fn done(first: &mut Option<String>, main: &mut bool, out: &mut FormNames) {
        if std::mem::take(main) && out.main_type.is_none() {
            out.main_type = first.take();
        }
        *first = None;
    }
    let mut out = FormNames::default();
    let mut first_type: Option<String> = None;
    let mut main = false;
    scan_file(path, |st, ev| match ev {
        Ev::Start(a) if is(st, &["Form", "Attributes", "Attribute"]) => {
            done(&mut first_type, &mut main, &mut out);
            if let Some((_, v)) = a.iter().find(|(k, _)| k == "name") {
                out.attributes.push(v.clone());
            }
        }
        Ev::Text(t) if is(st, &["Form", "Attributes", "Attribute", "Type", "Type"]) => {
            if first_type.is_none() {
                first_type = Some(t.to_string());
            }
        }
        Ev::Text(t) if is(st, &["Form", "Attributes", "Attribute", "MainAttribute"]) => {
            main = t.trim() == "true";
        }
        _ => {}
    })?;
    done(&mut first_type, &mut main, &mut out);
    Ok(out)
}
