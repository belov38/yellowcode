use std::collections::{HashMap, HashSet};

use onec_client::auth::printable;
use onec_client::dcs::{DcsForm, DetailsItem, SettingKind, UserSetting, Variant};
use onec_client::formdescr::platform_type;
use onec_client::spreadsheet::Cell;
use onec_client::{FormAttribute, ReportSheet, ReportView, Sheet, Step};
use serde_json::{Value as Json, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    Text,
    Csv,
    Json,
}

pub(super) fn report_lines(
    view: &ReportView,
    attribute: Option<&str>,
    format: ReportFormat,
    steps: &[Step],
) -> Result<Vec<String>, String> {
    let sheets = chosen(view, attribute)?;
    match format {
        ReportFormat::Text => Ok(text_lines(view, &sheets)),
        ReportFormat::Csv => match sheets.as_slice() {
            [one] => Ok(csv_lines(&one.sheet)),
            [] => Err("--format csv: the call changed no spreadsheet document".into()),
            many => Err(format!(
                "--format csv writes one spreadsheet document, the call changed {}: choose one with --attribute ({})",
                many.len(),
                many.iter()
                    .map(|s| attribute_name(view, s.position))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        },
        ReportFormat::Json => {
            let j = report_json(view, &sheets, steps);
            serde_json::to_string_pretty(&j)
                .map(|s| vec![s])
                .map_err(|e| e.to_string())
        }
    }
}

fn chosen<'v>(
    view: &'v ReportView,
    attribute: Option<&str>,
) -> Result<Vec<&'v ReportSheet>, String> {
    let Some(want) = attribute else {
        return Ok(view.sheets.iter().collect());
    };
    let lower = want.to_lowercase();
    let found: Vec<&ReportSheet> = view
        .sheets
        .iter()
        .filter(|s| attribute_name(view, s.position).to_lowercase() == lower)
        .collect();
    if found.is_empty() {
        let changed: Vec<String> = view
            .sheets
            .iter()
            .map(|s| attribute_name(view, s.position))
            .collect();
        return Err(format!(
            "--attribute {want}: the call changed no spreadsheet document of that name (changed: {})",
            if changed.is_empty() {
                "none".to_string()
            } else {
                changed.join(", ")
            }
        ));
    }
    Ok(found)
}

fn attribute_name(view: &ReportView, position: usize) -> String {
    view.attributes
        .iter()
        .find(|a| a.position == position)
        .map_or_else(
            || format!("#{position}"),
            |a| a.field.display_name().to_string(),
        )
}

fn type_text(a: &FormAttribute) -> String {
    if let Some(s) = &a.object {
        return format!("object {s}");
    }
    if a.field.ty.ids.is_empty() {
        return "Произвольный".into();
    }
    a.field
        .ty
        .ids
        .iter()
        .map(|id| platform_type(id).map_or_else(|| id.clone(), |(_, ru)| ru.to_string()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn text_lines(view: &ReportView, sheets: &[&ReportSheet]) -> Vec<String> {
    let mut l = vec![format!(
        "[form] {} {}",
        view.form
            .names
            .get(1)
            .or(view.form.names.first())
            .map_or_else(|| "-".to_string(), |n| printable(n)),
        printable(&view.form.id)
    )];
    for a in &view.attributes {
        let mark = if a.is_spreadsheet() {
            " (spreadsheet)"
        } else {
            ""
        };
        l.push(format!(
            "attribute: {}: {}{mark}",
            printable(a.field.display_name()),
            printable(&type_text(a))
        ));
    }
    if let Some(d) = &view.dcs {
        l.extend(settings_lines(&d.form));
    }
    for s in sheets {
        l.push(format!(
            "[spreadsheet] {}: {} rows, {} columns{}",
            printable(&attribute_name(view, s.position)),
            s.sheet.height,
            s.sheet.width,
            if s.rows_asked {
                ", cells asked by rows"
            } else {
                ""
            }
        ));
        for row in s.sheet.rows() {
            l.push(text_row(row));
        }
    }
    if let Some(d) = view.dcs.as_ref().filter(|d| !d.details.is_empty()) {
        l.push(format!("[details] {} items", d.details.len()));
        let by_id: HashMap<u32, &DetailsItem> = d.details.iter().map(|i| (i.id, i)).collect();
        for s in sheets {
            for c in &s.sheet.cells {
                if let Some(id) = c.details_id {
                    l.push(details_line(c, id, &by_id));
                }
            }
        }
    }
    l
}

fn variant_text(v: &Variant) -> String {
    if v.key == v.presentation {
        printable(&v.key)
    } else {
        format!("{} ({})", printable(&v.key), printable(&v.presentation))
    }
}

fn settings_lines(f: &DcsForm) -> Vec<String> {
    let mut l = vec![format!(
        "variant: {}",
        f.variant.as_ref().map_or_else(|| "-".into(), variant_text)
    )];
    if !f.variants.is_empty() {
        l.push(format!(
            "variants: {}",
            f.variants
                .iter()
                .map(|v| {
                    let mark = if f.variant.as_ref().is_some_and(|c| c.key == v.key) {
                        "*"
                    } else {
                        ""
                    };
                    format!("{}{mark}", variant_text(v))
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for s in &f.settings {
        l.push(format!("setting: {}", setting_text(s)));
    }
    l
}

fn setting_text(s: &UserSetting) -> String {
    let name = match &s.title {
        Some(t) if t != s.name() => format!("{} ({})", printable(s.name()), printable(t)),
        _ => printable(s.name()),
    };
    let kind = match &s.kind {
        SettingKind::Parameter { .. } => "parameter".to_string(),
        SettingKind::Filter { comparison, .. } => format!("filter {}", printable(comparison)),
        SettingKind::Other(t) => return format!("{name}: {} (not changed here)", printable(t)),
    };
    let used = if s.used { "used" } else { "not used" };
    match &s.value {
        Some(v) => format!(
            "{name}: {kind}, {used}, {}{}",
            one_line(&v.text),
            v.type_name
                .as_ref()
                .map_or_else(String::new, |t| format!(" [{}]", printable(t)))
        ),
        None => format!("{name}: {kind}, {used}"),
    }
}

fn details_line(c: &Cell, id: u32, items: &HashMap<u32, &DetailsItem>) -> String {
    let mut parts = Vec::new();
    let mut action = None;
    let mut next = Some(id);
    let mut seen = HashSet::new();
    while let Some(i) = next {
        if !seen.insert(i) {
            break;
        }
        let Some(item) = items.get(&i) else {
            break;
        };
        if action.is_none() {
            action = item.main_action.clone();
        }
        for v in &item.values {
            let part = match &v.text {
                Some(t) => format!(
                    "{}={}{}",
                    printable(&v.field),
                    one_line(t),
                    v.type_name
                        .as_ref()
                        .map_or_else(String::new, |t| format!(" [{}]", printable(t)))
                ),
                None => printable(&v.field),
            };
            if !parts.contains(&part) {
                parts.push(part);
            }
        }
        next = item.parent;
    }
    format!(
        "details: R{}C{} #{id}: {}{}",
        c.row + 1,
        c.column + 1,
        if parts.is_empty() {
            "-".to_string()
        } else {
            parts.join("; ")
        },
        action.map_or_else(String::new, |a| format!(" ({})", printable(&a)))
    )
}

fn dcs_json(f: &DcsForm, details: &[DetailsItem]) -> Json {
    let variant = |v: &Variant| json!({"key": v.key, "presentation": v.presentation});
    json!({
        "variant": f.variant.as_ref().map(variant),
        "variants": f.variants.iter().map(variant).collect::<Vec<_>>(),
        "settings": f.settings.iter().map(|s| {
            let (kind, comparison) = match &s.kind {
                SettingKind::Parameter { .. } => ("parameter".to_string(), None),
                SettingKind::Filter { comparison, .. } => ("filter".to_string(), Some(comparison.clone())),
                SettingKind::Other(t) => (t.clone(), None),
            };
            json!({
                "id": s.id,
                "kind": kind,
                "name": s.name(),
                "title": s.title,
                "used": s.used,
                "comparison": comparison,
                "value": s.value.as_ref().map(|v| json!({"type": v.type_name, "text": v.text})),
            })
        }).collect::<Vec<_>>(),
        "parameters": f.parameters.iter().map(|p| json!({
            "name": p.name, "title": p.title, "types": p.types, "visible": p.visible,
        })).collect::<Vec<_>>(),
        "fields": f.fields.iter().filter(|p| !p.folder).map(|p| json!({
            "name": p.name, "title": p.title, "types": p.types,
        })).collect::<Vec<_>>(),
        "details": details.iter().map(|d| json!({
            "id": d.id,
            "parent": d.parent,
            "values": d.values.iter().map(|v| json!({"field": v.field, "type": v.type_name, "text": v.text})).collect::<Vec<_>>(),
            "main_action": d.main_action,
        })).collect::<Vec<_>>(),
    })
}

fn one_line(s: &str) -> String {
    printable(
        &s.replace('\\', "\\\\")
            .replace("\r\n", "\\n")
            .replace(['\n', '\r'], "\\n"),
    )
}

fn text_row(row: &[Cell]) -> String {
    let Some(last) = row
        .iter()
        .filter(|c| !c.text.is_empty())
        .map(|c| c.column)
        .max()
    else {
        return String::new();
    };
    let mut fields = vec![String::new(); last as usize + 1];
    for c in row {
        if let Some(f) = fields.get_mut(c.column as usize) {
            *f = one_line(&c.text);
        }
    }
    fields.join(" | ")
}

fn csv_lines(sheet: &Sheet) -> Vec<String> {
    sheet
        .rows()
        .into_iter()
        .map(|row| {
            let mut fields = vec![String::new(); sheet.width as usize];
            for c in row {
                if let Some(f) = fields.get_mut(c.column as usize) {
                    *f = csv_field(&c.text);
                }
            }
            let mut line = fields.join(",");
            line.push('\r');
            line
        })
        .collect()
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn report_json(view: &ReportView, sheets: &[&ReportSheet], steps: &[Step]) -> Json {
    let mut j = report_json_of(view, sheets, steps);
    if let (Some(d), Some(o)) = (&view.dcs, j.as_object_mut()) {
        o.insert("dcs".into(), dcs_json(&d.form, &d.details));
    }
    j
}

fn report_json_of(view: &ReportView, sheets: &[&ReportSheet], steps: &[Step]) -> Json {
    json!({
        "form": {"id": view.form.id, "names": view.form.names},
        "attributes": view.attributes.iter().map(|a| json!({
            "position": a.position,
            "name": a.field.name,
            "name_ru": a.field.name_ru,
            "types": a.field.ty.ids,
            "object": a.object,
            "spreadsheet": a.is_spreadsheet(),
        })).collect::<Vec<_>>(),
        "sheets": sheets.iter().map(|s| json!({
            "attribute": attribute_name(view, s.position),
            "position": s.position,
            "rows": s.sheet.height,
            "columns": s.sheet.width,
            "rows_asked": s.rows_asked,
            "cells": s.sheet.cells.iter().map(|c| json!({
                "row": c.row,
                "column": c.column,
                "text": c.text,
                "details": c.details.as_ref().map(|d| json!({"type": d.type_name, "text": d.text})),
                "details_id": c.details_id,
            })).collect::<Vec<_>>(),
            "merges": s.sheet.merges.iter().map(|m| json!({
                "row": m.row, "column": m.column, "columns": m.columns, "rows": m.rows,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "steps": steps.iter().map(|s| json!({"name": s.name, "status": s.status})).collect::<Vec<_>>(),
    })
}
