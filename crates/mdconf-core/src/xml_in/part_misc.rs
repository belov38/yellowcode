use super::{
    parts::{Context, boolean, text},
    shell_xml::{N, attr},
    walk::err,
};
use crate::{Result, parts::*};
use brace::Node;
use md_model::xml_origin::XmlPart;
use std::path::Path;

pub(super) fn content(cx: &Context<'_>, n: N<'_, '_>, path: &Path) -> Result<Vec<ContentItem>> {
    if n.local() != "ExchangePlanContent" || n.ns() != Some("http://v8.1c.ru/8.3/xcf/extrnprops") {
        return Err(err(path, "wrong exchange content root"));
    }
    n.children()
        .map(|n| {
            if n.local() != "Item" {
                return Err(err(path, "wrong exchange content record"));
            }
            let auto_record = match text(n, "AutoRecord") {
                "Allow" => true,
                "Deny" => false,
                _ => return Err(err(path, "invalid AutoRecord")),
            };
            Ok(ContentItem {
                object: cx.reference(text(n, "Metadata"), path)?,
                auto_record,
            })
        })
        .collect()
}
pub(super) fn schedule(n: N<'_, '_>, path: &Path) -> Result<Schedule> {
    if n.local() != "JobSchedule" || n.ns() != Some("http://v8.1c.ru/8.3/xcf/extrnprops") {
        return Err(err(path, "wrong schedule root"));
    }
    let n = n
        .children()
        .find(|n| n.local() == "Schedule")
        .ok_or_else(|| err(path, "missing Schedule"))?;
    schedule_value(n, path, false)
}
fn schedule_value(n: N<'_, '_>, path: &Path, daily: bool) -> Result<Schedule> {
    let (tag, ns) = if daily {
        (
            "DetailedDailySchedules",
            "http://v8.1c.ru/8.1/data/enterprise",
        )
    } else {
        ("Schedule", "http://v8.1c.ru/8.3/xcf/extrnprops")
    };
    if n.local() != tag || n.ns() != Some(ns) {
        return Err(err(path, "wrong nested schedule"));
    }
    let value = |name| attr(n, None, name).unwrap_or_default().to_string();
    let num = |name| {
        attr(n, None, name)
            .unwrap_or_default()
            .parse::<i64>()
            .map_err(|_| err(path, format!("invalid schedule {name}")))
    };
    let nums = |tag| {
        text(n, tag)
            .split_whitespace()
            .map(|s| s.parse().map_err(|_| err(path, "invalid schedule list")))
            .collect::<Result<Vec<i64>>>()
    };
    Ok(Schedule {
        begin_date: value("BeginDate"),
        end_date: value("EndDate"),
        begin_time: value("BeginTime"),
        end_time: value("EndTime"),
        completion_time: value("CompletionTime"),
        completion_interval: num("CompletionInterval")?,
        repeat_period_in_day: num("RepeatPeriodInDay")?,
        repeat_pause: num("RepeatPause")?,
        week_days: nums("WeekDays")?,
        week_day_in_month: num("WeekDayInMonth")?,
        day_in_month: num("DayInMonth")?,
        months: nums("Months")?,
        weeks_period: num("WeeksPeriod")?,
        days_repeat_period: num("DaysRepeatPeriod")?,
        detailed: n
            .children()
            .filter(|n| n.local() == "DetailedDailySchedules")
            .map(|n| schedule_value(n, path, true))
            .collect::<Result<_>>()?,
        source: MiscSource::native(Node::list(Vec::new())),
    })
}
pub(super) fn help(cx: &Context<'_>, ext: &Path) -> Option<Help> {
    let base = ext.join("Help");
    let mut out = Help::default();
    for (path, file) in cx.origin.files() {
        if let Ok(rel) = path.strip_prefix(&base) {
            if let Ok(asset) = rel.strip_prefix("_files") {
                out.files.push((slash(asset), file.bytes().to_vec()));
            } else if rel.extension().is_some_and(|s| s == "html") {
                out.pages
                    .push((rel.file_stem()?.to_str()?.into(), file.bytes().to_vec()));
            }
        }
    }
    (cx.origin.get(&ext.join("Help.xml")).is_some()
        || !out.pages.is_empty()
        || !out.files.is_empty())
    .then_some(out)
}
fn slash(path: &Path) -> String {
    path.iter()
        .filter_map(|s| s.to_str())
        .collect::<Vec<_>>()
        .join("/")
}
fn picture(cx: &mut Context<'_>, ext: &Path, stem: &str) -> Result<Option<Picture>> {
    let path = ext.join(format!("{stem}.xml"));
    let Some(file) = cx.origin.get(&path) else {
        let payload = ext.join(stem);
        if cx
            .origin
            .files()
            .any(|(p, f)| f.part() == XmlPart::Pictures && p.starts_with(&payload))
        {
            return Err(err(&path, "orphan picture payload without descriptor"));
        }
        return Ok(None);
    };
    if file.part() != XmlPart::Pictures {
        return Ok(None);
    }
    let arena = cx.arena(&path, file.bytes())?;
    let root = arena
        .root()
        .ok_or_else(|| err(&path, "missing picture root"))?;
    if root.local() != "ExtPicture" || root.ns() != Some("http://v8.1c.ru/8.3/xcf/extrnprops") {
        return Err(err(&path, "wrong picture root"));
    }
    let node = root
        .children()
        .find(|n| n.local() == "Picture")
        .ok_or_else(|| err(&path, "missing picture descriptor"))?;
    let name = text(node, "Abs");
    super::walk::component(&path, name)?;
    let data = cx
        .origin
        .get(&ext.join(stem).join(name))
        .ok_or_else(|| err(&path, "missing picture payload"))?
        .bytes()
        .to_vec();
    let transparent_pixel = node
        .children()
        .find(|n| n.local() == "TransparentPixel")
        .map(|n| {
            Ok::<(i64, i64), crate::Error>((
                attr(n, None, "x")
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| err(&path, "invalid transparent x"))?,
                attr(n, None, "y")
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| err(&path, "invalid transparent y"))?,
            ))
        })
        .transpose()?;
    Ok(Some(Picture {
        data,
        ext: name
            .strip_prefix("Picture.")
            .ok_or_else(|| err(&path, "invalid picture name"))?
            .into(),
        load_transparent: boolean(node, "LoadTransparent", &path)?,
        transparent_pixel,
    }))
}
pub(super) fn payloads(
    cx: &mut Context<'_>,
    ext: &Path,
    parts: &mut ObjectParts,
    kind: crate::MdKind,
) -> Result<()> {
    parts.help = help(cx, ext);
    parts.picture = picture(cx, ext, "Picture")?;
    for (path, file) in cx.origin.files() {
        let Ok(rel) = path.strip_prefix(ext) else {
            continue;
        };
        if rel.components().count() != 1 || file.part() != XmlPart::Misc {
            continue;
        }
        let name = slash(rel);
        if kind == crate::MdKind::WSReference {
            parts.ws_definition.push((name, file.bytes().to_vec()));
        } else if name == "Package.bin" {
            parts.xdto_package = Some(file.bytes().to_vec());
        } else if name.ends_with(".bin") {
            parts.binary_modules.push((name, file.bytes().to_vec()));
        }
    }
    Ok(())
}
pub(super) fn config_payloads(cx: &mut Context<'_>, parts: &mut ConfigParts) -> Result<()> {
    let ext = Path::new("Ext");
    parts.help = help(cx, ext);
    for stem in ["Logo", "Splash", "MainSectionPicture"] {
        if let Some(p) = picture(cx, ext, stem)? {
            parts.pictures.push((stem.into(), p));
        }
    }
    for name in ["ParentConfigurations.bin", "MobileClientSignature.bin"] {
        if let Some(f) = cx.origin.get(&ext.join(name)) {
            parts.binaries.push((name.into(), f.bytes().to_vec()));
        }
    }
    Ok(())
}
