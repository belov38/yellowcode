use md_xmlout::codec::parts::misc::*;

use super::{
    ContentItem, MiscSource, R, Schedule, Style, count, fetch, fetch_brace, find_mut, miss,
    plain_path,
};
use crate::entries::EntryData;
use crate::load::LoadCtx;
use crate::{Configuration, MdKind, MdObject, MdRef, Result};
use brace::{Guid, Node};

fn num(s: &str) -> i64 {
    s.trim().parse().unwrap_or(0)
}

fn nums(v: &[String]) -> Vec<i64> {
    v.iter().filter_map(|s| s.trim().parse().ok()).collect()
}

fn typed_schedule(p: &Parsed, source: &Node) -> Schedule {
    let a = &p.attrs;
    let detailed = p
        .detailed
        .iter()
        .zip(&p.det_src)
        .map(|(d, n)| typed_schedule(d, n))
        .collect();
    Schedule {
        begin_date: a[0].clone(),
        end_date: a[1].clone(),
        begin_time: a[2].clone(),
        end_time: a[3].clone(),
        completion_time: a[4].clone(),
        completion_interval: num(&a[5]),
        repeat_period_in_day: num(&a[6]),
        repeat_pause: num(&a[7]),
        week_days: nums(&p.week_days),
        week_day_in_month: num(&a[8]),
        day_in_month: num(&a[9]),
        months: nums(&p.months),
        weeks_period: num(&a[10]),
        days_repeat_period: num(&a[11]),
        detailed,
        source: MiscSource::native(source.clone()),
    }
}

fn parse_content(cfg: &Configuration, t: &Node) -> R<Vec<ContentItem>> {
    let l = t.as_list().ok_or("shape")?;
    if atext(l, 0)? != "2" {
        return Err("version");
    }
    let n = count(at(l, 1)?)?;
    let mut raw = Vec::new();
    for k in 0..n {
        let i = k
            .checked_mul(2)
            .and_then(|i| i.checked_add(2))
            .ok_or("number")?;
        raw.push((atext(l, i)?, atext(l, i + 1)?));
    }
    raw.sort();
    let mut items = Vec::new();
    for (u, ar) in raw {
        let id = Guid::parse(u).ok_or("object")?;
        let name = cfg.full_name(id).ok_or("object")?;
        let auto_record = match ar {
            "1" => true,
            "0" => false,
            _ => return Err("auto_record"),
        };
        items.push(ContentItem {
            object: MdRef {
                id: Some(id),
                name: name.to_string(),
            },
            auto_record,
        });
    }
    let end = 2 + 2 * n;
    match l.get(end..).unwrap_or_default() {
        [] => {}
        [x] if x.text() == Some("0") => {}
        _ => return Err("tail"),
    }
    Ok(items)
}

fn plain_bytes(cx: &LoadCtx, name: &str) -> Result<Option<R<Vec<u8>>>> {
    Ok(match fetch(cx, name)?.as_deref() {
        None => None,
        Some(EntryData::Nested(_)) => Some(Err("container")),
        Some(_) => cx.entries.raw(name)?.map(Ok),
    })
}

fn ids_of(cfg: &Configuration, kind: MdKind) -> Vec<Guid> {
    cfg.of_kind(kind).map(|o| o.id).collect()
}

fn all_ids(objs: &[MdObject], out: &mut Vec<(MdKind, Guid)>) {
    for o in objs {
        out.push((o.kind, o.id));
        all_ids(&o.subsystems, out);
    }
}

pub(crate) fn load(cx: &LoadCtx, cfg: &mut Configuration) -> Result<()> {
    for id in ids_of(cfg, MdKind::ScheduledJob) {
        let Some(node) = fetch_brace(cx, cfg, &format!("{id}.0"), &["misc", "sched"])? else {
            continue;
        };
        match parse_root(&node) {
            Ok(p) => {
                if let Some(o) = find_mut(&mut cfg.objects, id) {
                    o.parts.schedule = Some(typed_schedule(&p, &node));
                }
            }
            Err(why) => miss(cfg, &["misc", "sched", why]),
        }
    }
    for id in ids_of(cfg, MdKind::ExchangePlan) {
        let Some(node) = fetch_brace(cx, cfg, &format!("{id}.1"), &["misc", "content"])? else {
            continue;
        };
        match parse_content(cfg, &node) {
            Ok(items) => {
                if let Some(o) = find_mut(&mut cfg.objects, id) {
                    o.parts.exchange_content = Some(items);
                }
            }
            Err(why) => miss(cfg, &["misc", "content", why]),
        }
    }
    for id in ids_of(cfg, MdKind::Style) {
        let Some(node) = fetch_brace(cx, cfg, &format!("{id}.0"), &["misc", "style"])? else {
            continue;
        };
        match convert_style(&node) {
            Ok(_) => {
                if let Some(o) = find_mut(&mut cfg.objects, id) {
                    o.parts.style = Some(Style {
                        source: MiscSource::native(node),
                    });
                }
            }
            Err(why) => miss(cfg, &["misc", "style", why]),
        }
    }
    let mut all = Vec::new();
    all_ids(&cfg.objects, &mut all);
    for (kind, id) in all {
        match kind {
            MdKind::WSReference => load_ws(cx, cfg, id)?,
            MdKind::XDTOPackage => match plain_bytes(cx, &format!("{id}.0"))? {
                Some(Ok(b)) => {
                    if let Some(o) = find_mut(&mut cfg.objects, id) {
                        o.parts.xdto_package = Some(b);
                    }
                }
                Some(Err(why)) => miss(cfg, &["misc", "xdto", why]),
                None => {}
            },
            _ => load_modules(cx, cfg, kind, id)?,
        }
    }
    let h = cfg.header_id;
    for (suffix, file) in [
        ("4", "ParentConfigurations.bin"),
        ("10", "MobileClientSignature.bin"),
    ] {
        match plain_bytes(cx, &format!("{h}.{suffix}"))? {
            Some(Ok(b)) => cfg.parts.binaries.push((file.to_string(), b)),
            Some(Err(why)) => miss(cfg, &["misc", "config_bin", why]),
            None => {}
        }
    }
    Ok(())
}

fn load_modules(cx: &LoadCtx, cfg: &mut Configuration, kind: MdKind, id: Guid) -> Result<()> {
    let mods = cx.schema.modules(kind.as_str());
    let mut want: Vec<(String, String)> = Vec::new();
    for (suffix, file) in mods {
        let name = format!("{id}.{suffix}");
        let Some(d) = fetch(cx, &name)? else { continue };
        let EntryData::Nested(items) = &*d else {
            continue;
        };
        if items.iter().any(|(n, _)| n == "text") || !items.iter().any(|(n, _)| n == "image") {
            continue;
        }
        let bin = format!("{}.bin", file.strip_suffix(".bsl").unwrap_or(file));
        want.push((name, bin));
    }
    want.sort();
    for (name, bin) in want {
        if let Some(b) = cx.entries.raw(&name)?
            && let Some(o) = find_mut(&mut cfg.objects, id)
        {
            o.parts.binary_modules.push((bin, b));
        }
    }
    Ok(())
}

fn load_ws(cx: &LoadCtx, cfg: &mut Configuration, id: Guid) -> Result<()> {
    let Some(d) = fetch(cx, &format!("{id}.0"))? else {
        return Ok(());
    };
    let EntryData::Nested(items) = &*d else {
        return Ok(());
    };
    let mut sorted: Vec<&(String, Vec<u8>)> = items.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut keyed: Vec<(String, Vec<u8>)> = Vec::with_capacity(sorted.len());
    for (n, data) in sorted {
        if n == "0.wsdl" {
            let mut b = Vec::new();
            if !data.starts_with(b"\xef\xbb\xbf") {
                b.extend_from_slice(b"\xef\xbb\xbf");
            }
            b.extend_from_slice(data);
            keyed.push(("WSDefinition.xml".to_string(), b));
        } else if plain_path(n) {
            keyed.push((n.clone(), data.clone()));
        } else {
            miss(cfg, &["misc", "ws", "path"]);
        }
    }
    if let Some(o) = find_mut(&mut cfg.objects, id) {
        o.parts.ws_definition = keyed;
    }
    Ok(())
}
