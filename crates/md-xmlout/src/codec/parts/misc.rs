use super::{R, XML_HEAD, count, each, txt};
use crate::codec::ui;
use crate::paths::object_dir;
use crate::{DUMP_VERSION, DumpSink, Part, PartStatus, Result, XmlWriter};
use brace::Node;
use md_model::parts::*;
use md_model::*;

pub fn at(n: &[Node], i: usize) -> R<&Node> {
    n.get(i).ok_or("index")
}

pub fn atext(n: &[Node], i: usize) -> R<&str> {
    txt(at(n, i)?)
}

pub fn chars(s: &str, a: usize, b: usize) -> String {
    s.chars().skip(a).take(b.saturating_sub(a)).collect()
}

pub fn date(s: &str) -> String {
    format!("{}-{}-{}", chars(s, 0, 4), chars(s, 4, 6), chars(s, 6, 8))
}

pub fn time(s: &str) -> String {
    format!(
        "{}:{}:{}",
        chars(s, 8, 10),
        chars(s, 10, 12),
        chars(s, 12, 14)
    )
}

pub const SCHED_ATTRS: [&str; 12] = [
    "BeginDate",
    "EndDate",
    "BeginTime",
    "EndTime",
    "CompletionTime",
    "CompletionInterval",
    "RepeatPeriodInDay",
    "RepeatPause",
    "WeekDayInMonth",
    "DayInMonth",
    "WeeksPeriod",
    "DaysRepeatPeriod",
];

pub struct Parsed {
    pub attrs: [String; 12],
    pub week_days: Vec<String>,
    pub months: Vec<String>,
    pub detailed: Vec<Parsed>,
    pub det_src: Vec<Node>,
}

pub fn parse_schedule(t: &[Node], in_detail: bool) -> R<Parsed> {
    let add = |a: usize, b: usize| a.checked_add(b).ok_or("number");
    let (bd, ed) = (date(atext(t, 0)?), date(atext(t, 1)?));
    let (bt, et, ct) = (time(atext(t, 2)?), time(atext(t, 3)?), time(atext(t, 4)?));
    let (ci, rp, rs) = (atext(t, 5)?, atext(t, 6)?, atext(t, 7)?);
    let n = count(at(t, 8)?)?;
    let list = |from: usize, n: usize| -> R<Vec<String>> {
        let end = add(from, n)?;
        t.get(from..end.min(t.len()))
            .unwrap_or_default()
            .iter()
            .map(|x| txt(x).map(str::to_string))
            .collect()
    };
    let week_days = list(9, n)?;
    let mut i = add(9, n)?;
    let (wdm, dim) = (atext(t, i)?, atext(t, add(i, 1)?)?);
    i = add(i, 2)?;
    let m = count(at(t, i)?)?;
    let months = list(add(i, 1)?, m)?;
    i = add(add(i, 1)?, m)?;
    let tail = |j: usize| t.get(j).map_or(Ok("0"), txt);
    let (wp, drp) = (tail(i)?, tail(add(i, 1)?)?);
    i = add(i, 2)?;
    let mut detailed = Vec::new();
    let mut det_src = Vec::new();
    if let Some(k) = t.get(i) {
        let k = count(k)?;
        let items = t.iter().skip(add(i, 1)?).take(k);
        if in_detail && items.clone().next().is_some() {
            return Err("nested");
        }
        for d in items {
            detailed.push(parse_schedule(d.as_list().ok_or("shape")?, true)?);
            det_src.push(d.clone());
        }
    }
    let s = str::to_string;
    let attrs = [
        bd,
        ed,
        bt,
        et,
        ct,
        s(ci),
        s(rp),
        s(rs),
        s(wdm),
        s(dim),
        s(wp),
        s(drp),
    ];
    Ok(Parsed {
        attrs,
        week_days,
        months,
        detailed,
        det_src,
    })
}

fn write_schedule(w: &mut XmlWriter, tag: &str, p: &Parsed) {
    let attrs: Vec<(&str, &str)> = SCHED_ATTRS
        .iter()
        .copied()
        .zip(p.attrs.iter().map(String::as_str))
        .collect();
    w.open(tag, &attrs);
    w.leaf("ent:WeekDays", Some(&p.week_days.join(" ")), &[]);
    w.leaf("ent:Months", Some(&p.months.join(" ")), &[]);
}

pub fn parse_root(t: &Node) -> R<Parsed> {
    parse_schedule(t.as_list().ok_or("shape")?, false)
}

pub fn schedule_text(t: &Node) -> R<String> {
    let p = parse_root(t)?;
    let mut w = XmlWriter::new(1);
    write_schedule(&mut w, "Schedule", &p);
    for d in &p.detailed {
        write_schedule(&mut w, "ent:DetailedDailySchedules", d);
        w.close("ent:DetailedDailySchedules");
    }
    w.close("Schedule");
    Ok(format!(
        "{XML_HEAD}\r\n<JobSchedule xmlns=\"http://v8.1c.ru/8.3/xcf/extrnprops\" \
         xmlns:ent=\"http://v8.1c.ru/8.1/data/enterprise\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" version=\"{DUMP_VERSION}\">\r\n{}\r\n</JobSchedule>",
        w.text()
    ))
}

pub fn schedule_xml(s: &Schedule) -> String {
    s.source
        .node()
        .and_then(|node| schedule_text(node).ok())
        .unwrap_or_default()
}

pub fn content_xml(_cfg: &Configuration, items: &[ContentItem]) -> String {
    let mut w = XmlWriter::new(1);
    for it in items {
        w.open("Item", &[]);
        w.leaf("Metadata", Some(&it.object.name), &[]);
        w.leaf(
            "AutoRecord",
            Some(if it.auto_record { "Allow" } else { "Deny" }),
            &[],
        );
        w.close("Item");
    }
    let body = w.text();
    let sep = if body.is_empty() { "" } else { "\r\n" };
    format!(
        "{XML_HEAD}\r\n<ExchangePlanContent xmlns=\"http://v8.1c.ru/8.3/xcf/extrnprops\" \
         xmlns:xr=\"http://v8.1c.ru/8.3/xcf/readable\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" version=\"{DUMP_VERSION}\">{sep}{body}\r\n</ExchangePlanContent>"
    )
}

pub fn convert_style(t: &Node) -> R<String> {
    let s = mdschema::schema().map_err(|_| "tables")?;
    let ut = s.ui_tables();
    let l = t.as_list().ok_or("shape")?;
    let n = count(at(l, 1)?)?;
    let order: std::collections::HashMap<&str, usize> = ut
        .style_order
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    let mut items = Vec::new();
    for it in l.iter().skip(2).take(n) {
        let it = it.as_list().ok_or("shape")?;
        let sid = atext(at(it, 0)?.as_list().ok_or("shape")?, 0)?;
        let typ = atext(it, 1)?;
        let val = at(it, 2)?;
        let (table, group) = match typ {
            "0" => (&ut.style_colors, 0),
            "1" => (&ut.style_fonts, 2),
            "2" => (&ut.style_borders, 1),
            _ => return Err("item"),
        };
        let nm = table.get(sid).ok_or("item")?;
        let key = order
            .get(nm.as_str())
            .copied()
            .unwrap_or(1000 + group * 100);
        items.push((key, nm, typ, val));
    }
    items.sort_by_key(|x| x.0);
    let mut w = XmlWriter::new(1);
    for (_, nm, typ, val) in items {
        w.open("Item", &[("name", nm)]);
        match typ {
            "0" => {
                let c = ui::color_text(val, s).ok_or("color")?;
                w.leaf("Color", Some(&c), &[]);
            }
            "1" => {
                let a = ui::font_attrs(val, s).ok_or("font")?;
                let a: Vec<(&str, &str)> =
                    a.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                w.leaf("Font", None, &a);
            }
            _ => {
                let (attrs, kids) = ui::border(val, s).ok_or("border")?;
                let a: Vec<(&str, &str)> = attrs
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                if kids.is_empty() {
                    w.leaf("Border", None, &a);
                } else {
                    w.open("Border", &a);
                    for (ct, xt, text) in &kids {
                        w.leaf(ct, Some(text), &[("xsi:type", xt)]);
                    }
                    w.close("Border");
                }
            }
        }
        w.close("Item");
    }
    let body = w.text();
    let sep = if body.is_empty() { "" } else { "\r\n" };
    Ok(format!(
        "{XML_HEAD}\r\n<Style xmlns=\"http://v8.1c.ru/8.3/xcf/extrnprops\" \
         xmlns:pal=\"http://v8.1c.ru/8.1/data/ui/colors/palette\" xmlns:style=\"http://v8.1c.ru/8.1/data/ui/style\" \
         xmlns:sys=\"http://v8.1c.ru/8.1/data/ui/fonts/system\" xmlns:v8ui=\"http://v8.1c.ru/8.1/data/ui\" \
         xmlns:web=\"http://v8.1c.ru/8.1/data/ui/colors/web\" xmlns:win=\"http://v8.1c.ru/8.1/data/ui/colors/windows\" \
         xmlns:xr=\"http://v8.1c.ru/8.3/xcf/readable\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" version=\"{DUMP_VERSION}\">{sep}{body}\r\n</Style>"
    ))
}

pub fn style_xml(_cfg: &Configuration, s: &Style) -> Option<String> {
    convert_style(s.source.node()?).ok()
}

pub fn dump(cfg: &Configuration, sink: &mut DumpSink) -> Result<PartStatus> {
    each(&cfg.objects, &mut |o| {
        let p = &o.parts;
        let dir = || format!("{}/Ext", object_dir(cfg, o));
        if let Some(s) = &p.schedule {
            let xml = schedule_xml(s);
            if !xml.is_empty() {
                sink.put(
                    Part::Misc,
                    &format!("{}/Schedule.xml", dir()),
                    xml.as_bytes(),
                )?;
            }
        }
        if let Some(items) = &p.exchange_content {
            let xml = content_xml(cfg, items);
            sink.put(
                Part::Misc,
                &format!("{}/Content.xml", dir()),
                xml.as_bytes(),
            )?;
        }
        if let Some(xml) = p.style.as_ref().and_then(|s| style_xml(cfg, s)) {
            sink.put(Part::Misc, &format!("{}/Style.xml", dir()), xml.as_bytes())?;
        }
        for (name, bytes) in p.binary_modules.iter().chain(&p.ws_definition) {
            sink.put(Part::Misc, &format!("{}/{name}", dir()), bytes)?;
        }
        if let Some(b) = &p.xdto_package {
            sink.put(Part::Misc, &format!("{}/Package.bin", dir()), b)?;
        }
        Ok(())
    })?;
    for (name, bytes) in &cfg.parts.binaries {
        sink.put(Part::Misc, &format!("Ext/{name}"), bytes)?;
    }
    Ok(PartStatus::Written)
}
