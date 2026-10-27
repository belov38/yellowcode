use std::{env, error::Error, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let source = manifest.join("../../desktop/tauri.conf.json");
    println!("cargo:rerun-if-changed={}", source.display());
    let config: serde_json::Value = serde_json::from_slice(&fs::read(source)?)?;
    let version = config
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or("missing app version")?;
    println!("cargo:rustc-env=YELLOWCODE_VERSION={version}");
    write_viewer_order(&manifest)?;
    write_configurator(&manifest)?;
    write_lowercase_context()?;
    Ok(())
}

fn write_viewer_order(manifest: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let source = manifest.join("src/viewer/tree_order.json");
    println!("cargo:rerun-if-changed={}", source.display());
    let value: serde_json::Value = serde_json::from_slice(&fs::read(source)?)?;
    let list = |value: &serde_json::Value| -> Result<String, Box<dyn Error>> {
        let mut out = String::from("&[");
        for item in value.as_array().ok_or("tree order list expected")? {
            out.push_str(&format!(
                "{:?},",
                item.as_str().ok_or("tree order string expected")?
            ));
        }
        out.push(']');
        Ok(out)
    };
    let mut out = String::new();
    for (name, key) in [("ROOT", "root"), ("COMMON", "common")] {
        out.push_str(&format!(
            "pub(super) const {name}: &[&str] = {};\n",
            list(&value[key])?
        ));
    }
    for (name, key) in [("AFTER", "groups_inside"), ("CHILDREN", "children")] {
        out.push_str(&format!("pub(super) const {name}: &[(&str, &[&str])] = &["));
        for (owner, children) in value[key].as_object().ok_or("tree order map expected")? {
            out.push_str(&format!("({owner:?}, {}),", list(children)?));
        }
        out.push_str("];\n");
    }
    let target =
        PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?).join("viewer_order.rs");
    fs::write(target, out)?;
    Ok(())
}

fn write_configurator(manifest: &std::path::Path) -> Result<(), Box<dyn Error>> {
    use serde_json::Value;
    fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, Box<dyn Error>> {
        value.get(key).map_or(Ok(""), |v| {
            v.as_str().ok_or("configurator string expected".into())
        })
    }
    let read = |name: &str| -> Result<Value, Box<dyn Error>> {
        let path = manifest.join("src/viewer/configurator").join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    };
    let props = read("props.json")?;
    let mut out = String::from("const CATEGORIES: &[(&str, &str)] = &[");
    for row in props["categories"]
        .as_array()
        .ok_or("categories array expected")?
    {
        out.push_str(&format!("({:?},{:?}),", text(row, "en")?, text(row, "ru")?));
    }
    out.push_str("];\nconst PROPS: &[(&str, &[PaletteProp])] = &[");
    for (context, rows) in props["contexts"]
        .as_object()
        .ok_or("contexts map expected")?
    {
        out.push_str(&format!("({context:?}, &["));
        for row in rows.as_array().ok_or("properties array expected")? {
            out.push_str("PaletteProp {");
            for (field, key) in [
                ("name", "name"),
                ("caption_ru", "ru"),
                ("caption_en", "en"),
                ("tooltip_ru", "tip"),
                ("view", "view"),
                ("category", "cat"),
            ] {
                out.push_str(&format!("{field}: {:?},", text(row, key)?));
            }
            let tab = match row.get("tab").filter(|v| !v.is_null()) {
                Some(value) => format!("Some({:?})", value.as_str().ok_or("tab string expected")?),
                None => "None".into(),
            };
            out.push_str(&format!("tab: {tab}}},"));
        }
        out.push_str("]),");
    }
    out.push_str("];\nconst WINDOWS: &[(&str, &[WindowTab])] = &[");
    for (kind, tabs) in read("windows.json")?
        .as_object()
        .ok_or("windows map expected")?
    {
        out.push_str(&format!("({kind:?}, &["));
        for tab in tabs.as_array().ok_or("tabs array expected")? {
            out.push_str(&format!(
                "WindowTab{{title_ru:{:?},title_en:{:?},items:&[",
                text(tab, "ru")?,
                text(tab, "en")?
            ));
            for item in tab["items"].as_array().ok_or("items array expected")? {
                match text(item, "t")? {
                    "prop" => out.push_str(&format!("TabItem::Prop({:?}),", text(item, "name")?)),
                    "list" => out.push_str(&format!("TabItem::List({:?}),", text(item, "kind")?)),
                    "cap" => out.push_str(&format!(
                        "TabItem::Caption{{ru:{:?},en:{:?},control:{:?}}},",
                        text(item, "ru")?,
                        text(item, "en")?,
                        text(item, "control")?
                    )),
                    _ => {}
                }
            }
            out.push_str("]},");
        }
        out.push_str("]),");
    }
    out.push_str("];\nconst RIGHTS: &[(&str, [&str; 4])] = &[");
    for (name, row) in read("rights.json")?
        .as_object()
        .ok_or("rights map expected")?
    {
        out.push_str(&format!(
            "({name:?},[{:?},{:?},{:?},{:?}]),",
            text(row, "ru")?,
            text(row, "en")?,
            text(row, "ru_hint")?,
            text(row, "en_hint")?
        ));
    }
    out.push_str("];\n");
    let target = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?)
        .join("viewer_configurator.rs");
    fs::write(target, out)?;
    Ok(())
}

fn write_lowercase_context() -> Result<(), Box<dyn Error>> {
    use std::fmt::Write;
    let mut cased = Vec::<(u32, u32)>::new();
    let mut ignorable = Vec::<(u32, u32)>::new();
    fn append(rows: &mut Vec<(u32, u32)>, value: u32) {
        if let Some(last) = rows.last_mut()
            && last.1.checked_add(1) == Some(value)
        {
            last.1 = value;
            return;
        }
        rows.push((value, value));
    }
    let mut probe = String::with_capacity(12);
    for scalar in 0..=0x10ffff {
        let Some(c) = char::from_u32(scalar) else {
            continue;
        };
        probe.clear();
        probe.push(c);
        probe.push('Σ');
        let effective_cased = probe.to_lowercase().ends_with('ς');
        if effective_cased {
            append(&mut cased, scalar);
        } else {
            probe.clear();
            probe.push_str("AΣ");
            probe.push(c);
            probe.push('A');
            if probe.to_lowercase().chars().nth(1) == Some('σ') {
                append(&mut ignorable, scalar);
            }
        }
    }
    let mut out = String::new();
    for (name, rows) in [("CASED", cased), ("IGNORABLE", ignorable)] {
        writeln!(&mut out, "const {name}: &[(u32,u32)] = &{rows:?};")?;
    }
    let target = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?)
        .join("lowercase_context.rs");
    fs::write(target, out)?;
    Ok(())
}
