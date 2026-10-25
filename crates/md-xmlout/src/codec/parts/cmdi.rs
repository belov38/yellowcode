use super::{R, XML_HEAD, bool_text, count, txt};
use crate::paths::object_dir;
use crate::{DUMP_VERSION, DumpSink, Part, PartStatus, Result, XmlWriter};
use brace::{Guid, Node};
use md_model::parts::*;
use md_model::*;
use mdschema::Ui;
use mdschema::rights::RightsTables;

struct Cx<'a> {
    cfg: &'a Configuration,
    ui: &'a Ui,
    tables: &'a RightsTables,
}

impl<'a> Cx<'a> {
    fn new(cfg: &'a Configuration) -> R<Cx<'a>> {
        Ok(Cx {
            cfg,
            ui: mdschema::schema().map_err(|_| "tables")?.ui(),
            tables: mdschema::rights::rights_tables().map_err(|_| "tables")?,
        })
    }

    fn name(&self, u: &str) -> Option<&'a str> {
        self.cfg.full_name(Guid::parse(u)?)
    }

    fn group(&self, g: &Node) -> R<String> {
        let g = txt(g)?;
        let table = self
            .tables
            .cmdi
            .get("command_groups")
            .and_then(|t| t.get(g));
        self.ui
            .command_groups
            .get(g)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| table.and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
            .or_else(|| self.name(g))
            .map(str::to_string)
            .ok_or("group")
    }

    fn std_command(&self, kind: &str, idx: &str) -> R<&'a str> {
        let t = self.tables.cmdi.get("std_commands");
        let pick = |k: &str| {
            t.and_then(|t| t.get(k))
                .and_then(|m| m.get(idx))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        };
        pick(kind).or_else(|| pick("*")).ok_or("std_command")
    }

    fn cmd_name(&self, r: &Node) -> R<String> {
        let l = r.as_list().ok_or("shape")?;
        if l.len() < 2 {
            return Ok(txt(at(r, 0)?)?.to_string());
        }
        let (idx, u) = (txt(at(r, 0)?)?, txt(at(r, 1)?)?);
        let Some(nm) = self.name(u) else {
            return Ok(format!("{idx}:{u}"));
        };
        let kind = nm.split('.').next().unwrap_or("");
        if nm.split('.').count() == 2 && !(idx == "0" && kind == "CommonCommand") {
            return Ok(format!(
                "{nm}.StandardCommand.{}",
                self.std_command(kind, idx)?
            ));
        }
        if idx != "0" {
            return Err("command_index");
        }
        Ok(nm.to_string())
    }

    fn adjbool(&self, w: &mut XmlWriter, tag: &str, v: &Node) -> R<()> {
        let inner = at(v, 1)?;
        let common = txt(at(at(inner, 1)?, 1)?)? == "1";
        let n = count(at(inner, 2)?)?;
        let mut vals = Vec::new();
        for i in 0..n {
            let ru = txt(at(inner, 3 + 2 * i)?)?;
            let rv = at(inner, 4 + 2 * i)?;
            let rn = self.name(ru).ok_or("role")?;
            vals.push((rn, bool_text(txt(at(rv, 1)?)? == "1")));
        }
        w.open(tag, &[]);
        w.leaf("xr:Common", Some(bool_text(common)), &[]);
        for (rn, val) in vals {
            w.leaf("xr:Value", Some(val), &[("name", rn)]);
        }
        w.close(tag);
        Ok(())
    }

    fn subsystem(&self, su: &Node) -> R<&'a str> {
        self.name(txt(su)?).ok_or("subsystem")
    }
}

fn at(n: &Node, i: usize) -> R<&Node> {
    n.at(i).ok_or("index")
}

type Sections<'a> = [Vec<&'a [Node]>; 6];

fn sections(t: &[Node]) -> R<Sections<'_>> {
    let mut out: Sections = Default::default();
    let mut i = 1usize;
    for (sec, wd) in out.iter_mut().zip([2usize, 3, 2, 1, 1, 2]) {
        let Some(flag) = t.get(i) else { continue };
        i += 1;
        if txt(flag)? == "0" {
            continue;
        }
        let n = count(t.get(i).ok_or("index")?)?;
        i += 1;
        let end = n
            .checked_mul(wd)
            .and_then(|m| i.checked_add(m))
            .ok_or("number")?;
        *sec = t.get(i..end).ok_or("tail")?.chunks(wd).collect();
        i = end;
    }
    if i != t.len() {
        return Err("tail");
    }
    Ok(out)
}

const CI_NS: &str = "xmlns=\"http://v8.1c.ru/8.3/xcf/extrnprops\" xmlns:xr=\"http://v8.1c.ru/8.3/xcf/readable\" \
    xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"";

pub fn convert_ci(cfg: &Configuration, t: &Node) -> R<String> {
    let cx = Cx::new(cfg)?;
    let l = t.as_list().ok_or("shape")?;
    if txt(at(t, 0)?)? != "7" {
        return Err("version");
    }
    let [vis, plc, order, sorder, gorder, svis] = sections(l)?;
    let mut w = XmlWriter::new(1);
    if !vis.is_empty() {
        w.open("CommandsVisibility", &[]);
        for it in vis {
            let [r, ab] = it else { return Err("shape") };
            w.open("Command", &[("name", &cx.cmd_name(r)?)]);
            cx.adjbool(&mut w, "Visibility", ab)?;
            w.close("Command");
        }
        w.close("CommandsVisibility");
    }
    if !plc.is_empty() {
        w.open("CommandsPlacement", &[]);
        for it in plc {
            let [r, g, p] = it else { return Err("shape") };
            w.open("Command", &[("name", &cx.cmd_name(r)?)]);
            w.leaf("CommandGroup", Some(&cx.group(g)?), &[]);
            let placement = match txt(p)? {
                "0" => "Auto",
                "1" => "Manual",
                _ => return Err("placement"),
            };
            w.leaf("Placement", Some(placement), &[]);
            w.close("Command");
        }
        w.close("CommandsPlacement");
    }
    if !order.is_empty() {
        w.open("CommandsOrder", &[]);
        for it in order {
            let [g, r] = it else { return Err("shape") };
            w.open("Command", &[("name", &cx.cmd_name(r)?)]);
            w.leaf("CommandGroup", Some(&cx.group(g)?), &[]);
            w.close("Command");
        }
        w.close("CommandsOrder");
    }
    if !svis.is_empty() {
        w.open("SubsystemsVisibility", &[]);
        for it in svis {
            let [su, ab] = it else { return Err("shape") };
            w.open("Subsystem", &[("name", cx.subsystem(su)?)]);
            cx.adjbool(&mut w, "Visibility", ab)?;
            w.close("Subsystem");
        }
        w.close("SubsystemsVisibility");
    }
    if !sorder.is_empty() {
        w.open("SubsystemsOrder", &[]);
        for it in sorder {
            let [su] = it else { return Err("shape") };
            w.leaf("Subsystem", Some(cx.subsystem(su)?), &[]);
        }
        w.close("SubsystemsOrder");
    }
    if !gorder.is_empty() {
        w.open("GroupsOrder", &[]);
        for it in gorder {
            let [g] = it else { return Err("shape") };
            w.leaf("Group", Some(&cx.group(g)?), &[]);
        }
        w.close("GroupsOrder");
    }
    let body = w.text();
    let sep = if body.is_empty() { "" } else { "\r\n" };
    Ok(format!(
        "{XML_HEAD}\r\n<CommandInterface {CI_NS} version=\"{DUMP_VERSION}\">{sep}{body}\r\n</CommandInterface>"
    ))
}

pub fn convert_homepage(cfg: &Configuration, t: &Node) -> R<String> {
    let cx = Cx::new(cfg)?;
    let l = t.as_list().ok_or("shape")?;
    if txt(at(t, 0)?)? != "1" {
        return Err("version");
    }
    let tmpl = match txt(at(t, 1)?)? {
        "0" => "OneColumn",
        "1" => "TwoColumnsEqualWidth",
        "2" => "TwoColumnsVariableWidth",
        _ => return Err("template"),
    };
    let mut w = XmlWriter::new(1);
    w.leaf("WorkingAreaTemplate", Some(tmpl), &[]);
    let mut i = 2usize;
    let cols = if tmpl == "OneColumn" {
        [Some("Column"), None]
    } else {
        [Some("LeftColumn"), Some("RightColumn")]
    };
    for cn in cols {
        let n = count(l.get(i).ok_or("index")?)?;
        i += 1;
        let items: Vec<&Node> = l.get(i..).unwrap_or_default().iter().take(n).collect();
        i = i.saturating_add(n);
        let Some(cn) = cn else {
            if !items.is_empty() {
                return Err("second_column");
            }
            continue;
        };
        if items.is_empty() {
            w.leaf(cn, None, &[]);
            continue;
        }
        w.open(cn, &[]);
        for it in items {
            let fu = txt(at(at(it, 1)?, 1)?)?;
            let fname = cx.name(fu).ok_or("form")?;
            w.open("Item", &[]);
            w.leaf("Form", Some(fname), &[]);
            w.leaf("Height", Some(txt(at(it, 2)?)?), &[]);
            cx.adjbool(&mut w, "Visibility", at(it, 3)?)?;
            w.close("Item");
        }
        w.close(cn);
    }
    let display = match l.get(i).and_then(Node::text) {
        Some("0") => Some("Top"),
        Some("1") => Some("Bottom"),
        _ => None,
    };
    if let Some(d) = display {
        w.leaf("MACommandInterfaceDisplays", Some(d), &[]);
    }
    Ok(format!(
        "{XML_HEAD}\r\n<HomePageWorkArea {CI_NS} version=\"{DUMP_VERSION}\">\r\n{}\r\n</HomePageWorkArea>",
        w.text()
    ))
}

const CAI_GROUP: &str = "f1d2a809-46ac-46ab-8b37-b2135ac5b323";
const CAI_PANEL: &str = "8b252cb5-d895-4c40-8ccb-581ce795fb80";
const CAI_SIDE_ORDER: [&str; 4] = ["top", "left", "right", "bottom"];
const CAI_DEPTH: usize = 64;

fn side_name(s: &str) -> R<&'static str> {
    match s {
        "1" => Ok("top"),
        "2" => Ok("bottom"),
        "3" => Ok("left"),
        "4" => Ok("right"),
        _ => Err("side"),
    }
}

fn cai_item(w: &mut XmlWriter, cls: &Node, it: &Node, depth: usize) -> R<()> {
    match txt(cls)? {
        CAI_GROUP => {
            if depth >= CAI_DEPTH {
                return Err("depth");
            }
            w.open("group", &[("id", txt(at(it, 1)?)?)]);
            let lst = at(it, 3)?;
            for k in 0..count(at(lst, 1)?)? {
                w.open("group", &[]);
                cai_item(w, at(lst, 2 + 2 * k)?, at(lst, 3 + 2 * k)?, depth + 1)?;
                w.close("group");
            }
            w.close("group");
        }
        CAI_PANEL => {
            w.open("panel", &[("id", txt(at(it, 1)?)?)]);
            w.leaf("uuid", Some(txt(at(it, 2)?)?), &[]);
            w.close("panel");
        }
        _ => return Err("class"),
    }
    Ok(())
}

pub fn convert_cai(t: &Node) -> R<String> {
    let l = t.as_list().ok_or("shape")?;
    if txt(at(t, 0)?)? != "1" {
        return Err("version");
    }
    let mut sides: Vec<(&str, &Node)> = Vec::new();
    let mut i = 1usize;
    while let Some(s) = l.get(i).filter(|s| s.is_list()) {
        sides.push((side_name(txt(at(s, 1)?)?)?, at(s, 2)?));
        i += 1;
    }
    let mut w = XmlWriter::new(1);
    for sn in CAI_SIDE_ORDER {
        let Some((_, body)) = sides.iter().rev().find(|(n, _)| *n == sn) else {
            continue;
        };
        if txt(at(body, 1)?)? == "0" {
            continue;
        }
        for k in 0..count(at(body, 1)?)? {
            w.open(sn, &[]);
            cai_item(&mut w, at(body, 2 + 2 * k)?, at(body, 3 + 2 * k)?, 0)?;
            w.close(sn);
        }
    }
    while let Some(tag) = l.get(i).filter(|n| n.text() != Some("0")) {
        let d = at(t, i + 1)?;
        i += 2;
        if !matches!(txt(tag)?, "1" | "2") {
            return Err("panel_def");
        }
        w.leaf("panelDef", None, &[("id", txt(at(d, 0)?)?)]);
    }
    Ok(format!(
        "{XML_HEAD}\r\n<ClientApplicationInterface xmlns=\"http://v8.1c.ru/8.2/managed-application/core\" \
         xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
         xsi:type=\"InterfaceLayouter\">\r\n{}\r\n</ClientApplicationInterface>",
        w.text()
    ))
}

pub fn command_interface_xml(cfg: &Configuration, ci: &CommandInterface) -> Option<String> {
    convert_ci(cfg, &ci.source).ok()
}

pub fn home_page_xml(cfg: &Configuration, p: &RawPart) -> Option<String> {
    convert_homepage(cfg, &p.node).ok()
}

pub fn client_interface_xml(_cfg: &Configuration, p: &RawPart) -> Option<String> {
    convert_cai(&p.node).ok()
}

pub fn dump(cfg: &Configuration, sink: &mut DumpSink) -> Result<PartStatus> {
    for s in cfg.of_kind(MdKind::Subsystem) {
        let Some(xml) = s
            .parts
            .command_interface
            .as_ref()
            .and_then(|ci| command_interface_xml(cfg, ci))
        else {
            continue;
        };
        let rel = format!("{}/Ext/CommandInterface.xml", object_dir(cfg, s));
        sink.put(Part::CommandInterface, &rel, xml.as_bytes())?;
    }
    let p = &cfg.parts;
    let files = [
        (
            "Ext/CommandInterface.xml",
            p.command_interface
                .as_ref()
                .and_then(|c| command_interface_xml(cfg, c)),
        ),
        (
            "Ext/MainSectionCommandInterface.xml",
            p.main_section
                .as_ref()
                .and_then(|c| command_interface_xml(cfg, c)),
        ),
        (
            "Ext/HomePageWorkArea.xml",
            p.home_page.as_ref().and_then(|c| home_page_xml(cfg, c)),
        ),
        (
            "Ext/ClientApplicationInterface.xml",
            p.client_interface
                .as_ref()
                .and_then(|c| client_interface_xml(cfg, c)),
        ),
    ];
    for (rel, xml) in files {
        if let Some(xml) = xml {
            sink.put(Part::CommandInterface, rel, xml.as_bytes())?;
        }
    }
    Ok(PartStatus::Written)
}
