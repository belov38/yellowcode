use crate::{DUMP_VERSION, ManagedXmlWriter, codec::parts::count};
use brace::{Guid, Node};
use memory_budget::{BudgetError, BudgetedText, Category, Charged, MemoryBudget};
enum Failure {
    Bad,
    Memory(BudgetError),
}
impl From<BudgetError> for Failure {
    fn from(e: BudgetError) -> Self {
        Self::Memory(e)
    }
}
impl From<&'static str> for Failure {
    fn from(_: &'static str) -> Self {
        Self::Bad
    }
}
type Result<T> = std::result::Result<T, Failure>;
fn at(node: &Node, index: usize) -> Result<&Node> {
    node.at(index).ok_or(Failure::Bad)
}
fn txt(node: &Node) -> Result<&str> {
    node.text().ok_or(Failure::Bad)
}
fn need<T>(v: Option<T>) -> Result<T> {
    v.ok_or(Failure::Bad)
}
fn pair(node: &Node, index: usize, base: usize) -> Result<&Node> {
    at(
        node,
        index
            .checked_mul(2)
            .and_then(|i| i.checked_add(base))
            .ok_or(Failure::Bad)?,
    )
}
fn boolean(v: bool) -> &'static str {
    if v { "true" } else { "false" }
}
fn table(file: &str, name: &str, key: &str) -> Option<&'static str> {
    mdschema::static_schema()
        .table(file)?
        .get(name)?
        .get(key)?
        .as_str()
        .filter(|s| !s.is_empty())
}
fn copied(budget: &MemoryBudget, parts: &[&str]) -> Result<BudgetedText> {
    let mut out = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
    for p in parts {
        out.try_push_str(p)?;
    }
    Ok(out)
}
struct Context<'a, 'b> {
    budget: &'b MemoryBudget,
    resolve: &'b dyn Fn(Guid) -> Option<&'a str>,
}
impl<'a> Context<'a, '_> {
    fn name(&self, raw: &str) -> Option<&'a str> {
        (self.resolve)(Guid::parse(raw)?)
    }
    fn group(&self, node: &Node) -> Result<BudgetedText> {
        let key = txt(node)?;
        let name = need(
            table("ui.json", "command_groups", key)
                .or_else(|| table("rights/cmdi.json", "command_groups", key))
                .or_else(|| self.name(key)),
        )?;
        copied(self.budget, &[name])
    }
    fn command(&self, node: &Node) -> Result<BudgetedText> {
        let list = need(node.as_list())?;
        let idx = txt(at(node, 0)?)?;
        if list.len() < 2 {
            return copied(self.budget, &[idx]);
        }
        let raw = txt(at(node, 1)?)?;
        let Some(name) = self.name(raw) else {
            return copied(self.budget, &[idx, ":", raw]);
        };
        let kind = name.split('.').next().unwrap_or("");
        if name.split('.').count() == 2 && !(idx == "0" && kind == "CommonCommand") {
            let commands = need(
                mdschema::static_schema()
                    .table("rights/cmdi.json")
                    .and_then(|v| v.get("std_commands")),
            )?;
            let pick = |key| {
                commands
                    .get(key)
                    .and_then(|v| v.get(idx))
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.is_empty())
            };
            copied(
                self.budget,
                &[
                    name,
                    ".StandardCommand.",
                    need(pick(kind).or_else(|| pick("*")))?,
                ],
            )
        } else {
            if idx != "0" {
                return Err(Failure::Bad);
            }
            copied(self.budget, &[name])
        }
    }
    fn subsystem(&self, node: &Node) -> Result<&'a str> {
        need(self.name(txt(node)?))
    }
    fn adjbool(&self, w: &mut ManagedXmlWriter, tag: &str, node: &Node) -> Result<()> {
        let inner = at(node, 1)?;
        let common = txt(at(at(inner, 1)?, 1)?)? == "1";
        let n = count(at(inner, 2)?)?;
        for i in 0..n {
            need(self.name(txt(pair(inner, i, 3)?)?))?;
            txt(at(pair(inner, i, 4)?, 1)?)?;
        }
        w.open(tag, &[])?;
        w.leaf("xr:Common", Some(boolean(common)), &[])?;
        for i in 0..n {
            let role = need(self.name(txt(pair(inner, i, 3)?)?))?;
            let val = txt(at(pair(inner, i, 4)?, 1)?)? == "1";
            w.leaf("xr:Value", Some(boolean(val)), &[("name", role)])?;
        }
        w.close(tag)?;
        Ok(())
    }
}
fn document(budget: &MemoryBudget, limit: u64, root: &'static str) -> Result<ManagedXmlWriter> {
    Ok(ManagedXmlWriter::part_document(
        budget,
        limit,
        root,
        &[
            ("xmlns", "http://v8.1c.ru/8.3/xcf/extrnprops"),
            ("xmlns:xr", "http://v8.1c.ru/8.3/xcf/readable"),
            ("xmlns:xs", "http://www.w3.org/2001/XMLSchema"),
            ("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance"),
            ("version", DUMP_VERSION),
        ],
    )?)
}
fn sections(list: &[Node]) -> Result<[&[Node]; 6]> {
    let mut out = [&[][..]; 6];
    let mut i = 1usize;
    for (section, width) in out.iter_mut().zip([2usize, 3, 2, 1, 1, 2]) {
        let Some(flag) = list.get(i) else {
            continue;
        };
        i += 1;
        if txt(flag)? == "0" {
            continue;
        }
        let n = count(need(list.get(i))?)?;
        i += 1;
        let end = n
            .checked_mul(width)
            .and_then(|n| n.checked_add(i))
            .ok_or(Failure::Bad)?;
        *section = need(list.get(i..end))?;
        i = end;
    }
    if i != list.len() {
        return Err(Failure::Bad);
    }
    Ok(out)
}
fn command_interface(cx: &Context<'_, '_>, t: &Node, limit: u64) -> Result<Charged<String>> {
    let l = need(t.as_list())?;
    if txt(at(t, 0)?)? != "7" {
        return Err(Failure::Bad);
    }
    let [vis, plc, order, sorder, gorder, svis] = sections(l)?;
    let mut w = document(cx.budget, limit, "CommandInterface")?;
    if !vis.is_empty() {
        w.open("CommandsVisibility", &[])?;
        for [r, ab] in vis.as_chunks::<2>().0 {
            w.open("Command", &[("name", cx.command(r)?.as_str())])?;
            cx.adjbool(&mut w, "Visibility", ab)?;
            w.close("Command")?;
        }
        w.close("CommandsVisibility")?;
    }
    if !plc.is_empty() {
        w.open("CommandsPlacement", &[])?;
        for [r, g, p] in plc.as_chunks::<3>().0 {
            w.open("Command", &[("name", cx.command(r)?.as_str())])?;
            w.leaf("CommandGroup", Some(cx.group(g)?.as_str()), &[])?;
            let placement = match txt(p)? {
                "0" => "Auto",
                "1" => "Manual",
                _ => return Err(Failure::Bad),
            };
            w.leaf("Placement", Some(placement), &[])?;
            w.close("Command")?;
        }
        w.close("CommandsPlacement")?;
    }
    if !order.is_empty() {
        w.open("CommandsOrder", &[])?;
        for [g, r] in order.as_chunks::<2>().0 {
            w.open("Command", &[("name", cx.command(r)?.as_str())])?;
            w.leaf("CommandGroup", Some(cx.group(g)?.as_str()), &[])?;
            w.close("Command")?;
        }
        w.close("CommandsOrder")?;
    }
    if !svis.is_empty() {
        w.open("SubsystemsVisibility", &[])?;
        for [su, ab] in svis.as_chunks::<2>().0 {
            w.open("Subsystem", &[("name", cx.subsystem(su)?)])?;
            cx.adjbool(&mut w, "Visibility", ab)?;
            w.close("Subsystem")?;
        }
        w.close("SubsystemsVisibility")?;
    }
    if !sorder.is_empty() {
        w.open("SubsystemsOrder", &[])?;
        for [su] in sorder.as_chunks::<1>().0 {
            w.leaf("Subsystem", Some(cx.subsystem(su)?), &[])?;
        }
        w.close("SubsystemsOrder")?;
    }
    if !gorder.is_empty() {
        w.open("GroupsOrder", &[])?;
        for [g] in gorder.as_chunks::<1>().0 {
            w.leaf("Group", Some(cx.group(g)?.as_str()), &[])?;
        }
        w.close("GroupsOrder")?;
    }
    Ok(w.finish()?)
}
fn home_page(cx: &Context<'_, '_>, t: &Node, limit: u64) -> Result<Charged<String>> {
    let l = need(t.as_list())?;
    if txt(at(t, 0)?)? != "1" {
        return Err(Failure::Bad);
    }
    let tmpl = match txt(at(t, 1)?)? {
        "0" => "OneColumn",
        "1" => "TwoColumnsEqualWidth",
        "2" => "TwoColumnsVariableWidth",
        _ => return Err(Failure::Bad),
    };
    let mut w = document(cx.budget, limit, "HomePageWorkArea")?;
    w.leaf("WorkingAreaTemplate", Some(tmpl), &[])?;
    let mut i = 2usize;
    let cols = if tmpl == "OneColumn" {
        [Some("Column"), None]
    } else {
        [Some("LeftColumn"), Some("RightColumn")]
    };
    for cn in cols {
        let n = count(l.get(i).ok_or(Failure::Bad)?)?;
        i += 1;
        let tail = l.get(i..).unwrap_or_default();
        let items = need(tail.get(..n.min(tail.len())))?;
        i = i.saturating_add(n);
        let Some(cn) = cn else {
            if !items.is_empty() {
                return Err(Failure::Bad);
            }
            continue;
        };
        if items.is_empty() {
            w.leaf(cn, None, &[])?;
            continue;
        }
        w.open(cn, &[])?;
        for it in items {
            let fu = txt(at(at(it, 1)?, 1)?)?;
            let fname = cx.name(fu).ok_or(Failure::Bad)?;
            w.open("Item", &[])?;
            w.leaf("Form", Some(fname), &[])?;
            w.leaf("Height", Some(txt(at(it, 2)?)?), &[])?;
            cx.adjbool(&mut w, "Visibility", at(it, 3)?)?;
            w.close("Item")?;
        }
        w.close(cn)?;
    }
    let display = match l.get(i).and_then(Node::text) {
        Some("0") => Some("Top"),
        Some("1") => Some("Bottom"),
        _ => None,
    };
    if let Some(d) = display {
        w.leaf("MACommandInterfaceDisplays", Some(d), &[])?;
    }
    Ok(w.finish()?)
}

const GROUP: &str = "f1d2a809-46ac-46ab-8b37-b2135ac5b323";
const PANEL: &str = "8b252cb5-d895-4c40-8ccb-581ce795fb80";
fn client_item(w: &mut ManagedXmlWriter, class: &Node, item: &Node, depth: usize) -> Result<()> {
    match txt(class)? {
        GROUP => {
            if depth >= 64 {
                return Err(Failure::Bad);
            }
            w.open("group", &[("id", txt(at(item, 1)?)?)])?;
            let list = at(item, 3)?;
            for i in 0..count(at(list, 1)?)? {
                w.open("group", &[])?;
                client_item(w, pair(list, i, 2)?, pair(list, i, 3)?, depth + 1)?;
                w.close("group")?;
            }
            w.close("group")?;
        }
        PANEL => {
            w.open("panel", &[("id", txt(at(item, 1)?)?)])?;
            w.leaf("uuid", Some(txt(at(item, 2)?)?), &[])?;
            w.close("panel")?;
        }
        _ => return Err(Failure::Bad),
    }
    Ok(())
}
fn client_interface(budget: &MemoryBudget, t: &Node, limit: u64) -> Result<Charged<String>> {
    let list = need(t.as_list())?;
    if txt(at(t, 0)?)? != "1" {
        return Err(Failure::Bad);
    }
    let mut sides = [None; 4];
    let mut i = 1usize;
    while let Some(side) = list.get(i).filter(|n| n.is_list()) {
        let slot = match txt(at(side, 1)?)? {
            "1" => 0,
            "3" => 1,
            "4" => 2,
            "2" => 3,
            _ => return Err(Failure::Bad),
        };
        *sides.get_mut(slot).ok_or(Failure::Bad)? = Some(at(side, 2)?);
        i += 1;
    }
    let mut w = ManagedXmlWriter::part_document(
        budget,
        limit,
        "ClientApplicationInterface",
        &[
            ("xmlns", "http://v8.1c.ru/8.2/managed-application/core"),
            ("xmlns:xs", "http://www.w3.org/2001/XMLSchema"),
            ("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance"),
            ("xsi:type", "InterfaceLayouter"),
        ],
    )?;
    let mut has_body = false;
    for (name, side) in ["top", "left", "right", "bottom"].into_iter().zip(sides) {
        let Some(body) = side else {
            continue;
        };
        if txt(at(body, 1)?)? == "0" {
            continue;
        }
        for k in 0..count(at(body, 1)?)? {
            has_body = true;
            w.open(name, &[])?;
            client_item(&mut w, pair(body, k, 2)?, pair(body, k, 3)?, 0)?;
            w.close(name)?;
        }
    }
    while let Some(tag) = list.get(i).filter(|n| n.text() != Some("0")) {
        let definition = at(t, i.checked_add(1).ok_or(Failure::Bad)?)?;
        i = i.checked_add(2).ok_or(Failure::Bad)?;
        if !matches!(txt(tag)?, "1" | "2") {
            return Err(Failure::Bad);
        }
        has_body = true;
        w.leaf("panelDef", None, &[("id", txt(at(definition, 0)?)?)])?;
    }
    if !has_body {
        w.empty_body_line()?;
    }
    Ok(w.finish()?)
}
fn complete(
    result: Result<Charged<String>>,
) -> std::result::Result<Option<Charged<String>>, BudgetError> {
    match result {
        Ok(body) => Ok(Some(body)),
        Err(Failure::Bad) => Ok(None),
        Err(Failure::Memory(e)) => Err(e),
    }
}
pub fn managed_command_interface_xml<'a>(
    budget: &MemoryBudget,
    source: &Node,
    resolve: impl Fn(Guid) -> Option<&'a str>,
    limit: u64,
) -> std::result::Result<Option<Charged<String>>, BudgetError> {
    complete(command_interface(
        &Context {
            budget,
            resolve: &resolve,
        },
        source,
        limit,
    ))
}
pub fn managed_home_page_xml<'a>(
    budget: &MemoryBudget,
    source: &Node,
    resolve: impl Fn(Guid) -> Option<&'a str>,
    limit: u64,
) -> std::result::Result<Option<Charged<String>>, BudgetError> {
    complete(home_page(
        &Context {
            budget,
            resolve: &resolve,
        },
        source,
        limit,
    ))
}
pub fn managed_client_interface_xml(
    budget: &MemoryBudget,
    source: &Node,
    limit: u64,
) -> std::result::Result<Option<Charged<String>>, BudgetError> {
    complete(client_interface(budget, source, limit))
}
