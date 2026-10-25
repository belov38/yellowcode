use super::ManagedIndex;
use brace::{Guid, Node};
type Result<T> = std::result::Result<T, &'static str>;
fn at(node: &Node, i: usize) -> Result<&Node> {
    node.at(i).ok_or("index")
}
fn txt(node: &Node) -> Result<&str> {
    node.text().ok_or("shape")
}
fn count(node: &Node) -> Result<usize> {
    txt(node)?
        .trim()
        .parse::<i64>()
        .ok()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or("number")
}
fn pair(node: &Node, i: usize, base: usize) -> Result<&Node> {
    at(
        node,
        i.checked_mul(2)
            .and_then(|i| i.checked_add(base))
            .ok_or("number")?,
    )
}
fn table(file: &str, table: &str, key: &str) -> Option<&'static str> {
    mdschema::static_schema()
        .table(file)?
        .get(table)?
        .get(key)?
        .as_str()
        .filter(|v| !v.is_empty())
}
struct Context<'a>(&'a ManagedIndex);
impl Context<'_> {
    fn name(&self, raw: &str) -> Option<&str> {
        self.0.names.get(&Guid::parse(raw)?).map(|n| n.as_str())
    }
    fn group(&self, node: &Node) -> Result<()> {
        let key = txt(node)?;
        table("ui.json", "command_groups", key)
            .or_else(|| table("rights/cmdi.json", "command_groups", key))
            .or_else(|| self.name(key))
            .ok_or("group")?;
        Ok(())
    }
    fn command(&self, node: &Node) -> Result<()> {
        let list = node.as_list().ok_or("shape")?;
        let idx = txt(at(node, 0)?)?;
        if list.len() < 2 {
            return Ok(());
        }
        let raw = txt(at(node, 1)?)?;
        let Some(name) = self.name(raw) else {
            return Ok(());
        };
        let kind = name.split('.').next().unwrap_or("");
        if name.split('.').count() == 2 && !(idx == "0" && kind == "CommonCommand") {
            let commands = mdschema::static_schema()
                .table("rights/cmdi.json")
                .and_then(|t| t.get("std_commands"));
            let pick = |kind| {
                commands
                    .and_then(|t| t.get(kind))
                    .and_then(|t| t.get(idx))
                    .and_then(|t| t.as_str())
                    .filter(|s| !s.is_empty())
            };
            pick(kind).or_else(|| pick("*")).ok_or("std_command")?;
        } else if idx != "0" {
            return Err("command_index");
        }
        Ok(())
    }
    fn adjbool(&self, node: &Node) -> Result<()> {
        let inner = at(node, 1)?;
        txt(at(at(inner, 1)?, 1)?)?;
        for i in 0..count(at(inner, 2)?)? {
            let raw = txt(pair(inner, i, 3)?)?;
            let val = pair(inner, i, 4)?;
            self.name(raw).ok_or("role")?;
            txt(at(val, 1)?)?;
        }
        Ok(())
    }
    fn subsystem(&self, node: &Node) -> Result<()> {
        self.name(txt(node)?).ok_or("subsystem").map(|_| ())
    }
}

pub(super) fn command_interface(index: &ManagedIndex, node: &Node) -> Result<()> {
    let cx = Context(index);
    let list = node.as_list().ok_or("shape")?;
    if txt(at(node, 0)?)? != "7" {
        return Err("version");
    }
    let mut sections: [&[Node]; 6] = [&[]; 6];
    let mut i = 1usize;
    for (section, width) in sections.iter_mut().zip([2usize, 3, 2, 1, 1, 2]) {
        let Some(flag) = list.get(i) else {
            continue;
        };
        i += 1;
        if txt(flag)? == "0" {
            continue;
        }
        let n = count(list.get(i).ok_or("index")?)?;
        i += 1;
        let end = n
            .checked_mul(width)
            .and_then(|n| n.checked_add(i))
            .ok_or("number")?;
        *section = list.get(i..end).ok_or("tail")?;
        i = end;
    }
    if i != list.len() {
        return Err("tail");
    }
    for [command, visibility] in sections[0].as_chunks::<2>().0 {
        cx.command(command)?;
        cx.adjbool(visibility)?;
    }
    for [command, group, placement] in sections[1].as_chunks::<3>().0 {
        cx.command(command)?;
        cx.group(group)?;
        if !matches!(txt(placement)?, "0" | "1") {
            return Err("placement");
        }
    }
    for [group, command] in sections[2].as_chunks::<2>().0 {
        cx.command(command)?;
        cx.group(group)?;
    }
    for [subsystem, visibility] in sections[5].as_chunks::<2>().0 {
        cx.subsystem(subsystem)?;
        cx.adjbool(visibility)?;
    }
    for it in sections[3] {
        cx.subsystem(it)?;
    }
    for it in sections[4] {
        cx.group(it)?;
    }
    Ok(())
}
pub(super) fn home_page(index: &ManagedIndex, node: &Node) -> Result<()> {
    let list = node.as_list().ok_or("shape")?;
    if txt(at(node, 0)?)? != "1" {
        return Err("version");
    }
    let one = match txt(at(node, 1)?)? {
        "0" => true,
        "1" | "2" => false,
        _ => return Err("template"),
    };
    let cx = Context(index);
    let mut i = 2usize;
    for column in 0..2 {
        let n = count(list.get(i).ok_or("index")?)?;
        i += 1;
        let items = list.get(i..).unwrap_or_default();
        let items = items.get(..n.min(items.len())).ok_or("index")?;
        i = i.saturating_add(n);
        if one && column == 1 {
            if !items.is_empty() {
                return Err("second_column");
            }
            continue;
        }
        for item in items {
            let form = txt(at(at(item, 1)?, 1)?)?;
            cx.name(form).ok_or("form")?;
            txt(at(item, 2)?)?;
            cx.adjbool(at(item, 3)?)?;
        }
    }
    Ok(())
}
fn cai_item(class: &Node, item: &Node, depth: usize) -> Result<()> {
    match txt(class)? {
        "f1d2a809-46ac-46ab-8b37-b2135ac5b323" => {
            if depth >= 64 {
                return Err("depth");
            }
            txt(at(item, 1)?)?;
            let children = at(item, 3)?;
            for i in 0..count(at(children, 1)?)? {
                cai_item(pair(children, i, 2)?, pair(children, i, 3)?, depth + 1)?;
            }
        }
        "8b252cb5-d895-4c40-8ccb-581ce795fb80" => {
            txt(at(item, 1)?)?;
            txt(at(item, 2)?)?;
        }
        _ => return Err("class"),
    }
    Ok(())
}
pub(super) fn client_interface(node: &Node) -> Result<()> {
    let list = node.as_list().ok_or("shape")?;
    if txt(at(node, 0)?)? != "1" {
        return Err("version");
    }
    let mut sides = [None; 4];
    let mut i = 1;
    while let Some(side) = list.get(i).filter(|s| s.is_list()) {
        let slot = match txt(at(side, 1)?)? {
            "1" => 0,
            "3" => 1,
            "4" => 2,
            "2" => 3,
            _ => return Err("side"),
        };
        *sides.get_mut(slot).ok_or("side")? = Some(at(side, 2)?);
        i += 1;
    }
    for body in sides.into_iter().flatten() {
        if txt(at(body, 1)?)? == "0" {
            continue;
        }
        for k in 0..count(at(body, 1)?)? {
            cai_item(pair(body, k, 2)?, pair(body, k, 3)?, 0)?;
        }
    }
    while let Some(tag) = list.get(i).filter(|n| n.text() != Some("0")) {
        let value = at(node, i.checked_add(1).ok_or("number")?)?;
        i = i.checked_add(2).ok_or("number")?;
        if !matches!(txt(tag)?, "1" | "2") {
            return Err("panel_def");
        }
        txt(at(value, 0)?)?;
    }
    Ok(())
}
