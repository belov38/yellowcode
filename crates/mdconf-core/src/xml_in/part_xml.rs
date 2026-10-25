use super::{shell_xml::N, walk::err};
use crate::Result;
use std::{collections::HashSet, path::Path};
const PREDEF: &str = "http://v8.1c.ru/8.3/xcf/predef";
const CORE: &str = "http://v8.1c.ru/8.1/data/core";
const EXTRA: &str = "http://v8.1c.ru/8.3/xcf/extrnprops";
const ENT: &str = "http://v8.1c.ru/8.1/data/enterprise";
const XR: &str = "http://v8.1c.ru/8.3/xcf/readable";

pub(super) fn validate(root: N<'_, '_>, path: &Path) -> Result<()> {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let valid = match root.local() {
            "PredefinedData" => match n.ns() {
                Some(PREDEF) => matches!(
                    n.local(),
                    "PredefinedData"
                        | "Item"
                        | "Name"
                        | "Code"
                        | "Description"
                        | "IsFolder"
                        | "Type"
                        | "ChildItems"
                        | "AccountType"
                        | "OffBalance"
                        | "Order"
                        | "AccountingFlags"
                        | "Flag"
                        | "ExtDimensionTypes"
                        | "ExtDimensionType"
                        | "Turnover"
                        | "ActionPeriodIsBase"
                        | "Base"
                        | "Leading"
                        | "Displaced"
                        | "CalculationType"
                        | "ExtensionState"
                ),
                Some(CORE) => matches!(
                    n.local(),
                    "Type"
                        | "TypeSet"
                        | "NumberQualifiers"
                        | "Digits"
                        | "FractionDigits"
                        | "AllowedSign"
                        | "StringQualifiers"
                        | "Length"
                        | "AllowedLength"
                        | "DateQualifiers"
                        | "DateFractions"
                        | "BinaryDataQualifiers"
                ),
                _ => false,
            },
            "ExchangePlanContent" => {
                n.ns() == Some(EXTRA)
                    && matches!(
                        n.local(),
                        "ExchangePlanContent" | "Item" | "Metadata" | "AutoRecord"
                    )
            }
            "JobSchedule" => match n.local() {
                "JobSchedule" | "Schedule" => n.ns() == Some(EXTRA),
                "WeekDays" | "Months" | "DetailedDailySchedules" => n.ns() == Some(ENT),
                _ => false,
            },
            "ExtPicture" => match n.local() {
                "ExtPicture" | "Picture" => n.ns() == Some(EXTRA),
                "Abs" | "LoadTransparent" | "TransparentPixel" => n.ns() == Some(XR),
                _ => false,
            },
            _ => return Ok(()),
        };
        if !valid {
            return Err(err(path, "wrong part element or namespace"));
        }
        let mut seen = HashSet::new();
        for child in n.children() {
            let placed = match root.local() {
                "PredefinedData" => {
                    let fields: &[&str] = match (n.ns(), n.local()) {
                        (Some(PREDEF), "PredefinedData" | "ChildItems") => &["Item"],
                        (Some(PREDEF), "Item") => &[
                            "Name",
                            "Code",
                            "Description",
                            "IsFolder",
                            "Type",
                            "ChildItems",
                            "AccountType",
                            "OffBalance",
                            "Order",
                            "AccountingFlags",
                            "ExtDimensionTypes",
                            "ActionPeriodIsBase",
                            "Base",
                            "Leading",
                            "Displaced",
                            "ExtensionState",
                        ],
                        (Some(PREDEF), "AccountingFlags") => &["Flag"],
                        (Some(PREDEF), "ExtDimensionTypes") => &["ExtDimensionType"],
                        (Some(PREDEF), "ExtDimensionType") => &["Turnover", "AccountingFlags"],
                        (Some(PREDEF), "Base" | "Leading" | "Displaced") => &["CalculationType"],
                        (Some(PREDEF), "Type") => &[
                            "Type",
                            "TypeSet",
                            "NumberQualifiers",
                            "StringQualifiers",
                            "DateQualifiers",
                            "BinaryDataQualifiers",
                        ],
                        (Some(CORE), "NumberQualifiers") => {
                            &["Digits", "FractionDigits", "AllowedSign"]
                        }
                        (Some(CORE), "StringQualifiers" | "BinaryDataQualifiers") => {
                            &["Length", "AllowedLength"]
                        }
                        (Some(CORE), "DateQualifiers") => &["DateFractions"],
                        _ => &[],
                    };
                    let ns = if n.ns() == Some(CORE) || n.local() == "Type" {
                        CORE
                    } else {
                        PREDEF
                    };
                    child.ns() == Some(ns) && fields.contains(&child.local())
                }
                "ExchangePlanContent" => match n.local() {
                    "ExchangePlanContent" => child.local() == "Item",
                    "Item" => matches!(child.local(), "Metadata" | "AutoRecord"),
                    _ => false,
                },
                "JobSchedule" => match n.local() {
                    "JobSchedule" => child.local() == "Schedule",
                    "Schedule" => matches!(
                        child.local(),
                        "WeekDays" | "Months" | "DetailedDailySchedules"
                    ),
                    "DetailedDailySchedules" => matches!(child.local(), "WeekDays" | "Months"),
                    _ => false,
                },
                "ExtPicture" => match n.local() {
                    "ExtPicture" => child.local() == "Picture",
                    "Picture" => matches!(
                        child.local(),
                        "Abs" | "LoadTransparent" | "TransparentPixel"
                    ),
                    _ => false,
                },
                _ => true,
            };
            if !placed {
                return Err(err(path, "misplaced part field or container"));
            }
            let repeat = matches!(
                child.local(),
                "Item" | "Flag" | "ExtDimensionType" | "CalculationType" | "DetailedDailySchedules"
            ) || (child.ns() == Some(CORE)
                && matches!(child.local(), "Type" | "TypeSet"));
            if !repeat && !seen.insert((child.ns(), child.local())) {
                return Err(err(path, "duplicate part field"));
            }
            stack.push(child);
        }
        if matches!(
            n.local(),
            "Name"
                | "Code"
                | "Description"
                | "IsFolder"
                | "AccountType"
                | "OffBalance"
                | "Order"
                | "Flag"
                | "Turnover"
                | "ActionPeriodIsBase"
                | "CalculationType"
                | "Metadata"
                | "AutoRecord"
                | "WeekDays"
                | "Months"
                | "Abs"
                | "LoadTransparent"
        ) && n.children().next().is_some()
        {
            return Err(err(path, "part scalar contains elements"));
        }
    }
    Ok(())
}
