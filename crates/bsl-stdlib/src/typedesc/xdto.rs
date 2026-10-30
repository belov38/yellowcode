use super::{
    AllowedLength, AllowedSign, BinaryQualifiers, DateParts, DateQualifiers, NumberQualifiers,
    StringQualifiers, TypeDescription, idx, new_type_description, qualifiers::Qualifier,
};
use crate::xdto::{XdtoCodec, XdtoCtx, XdtoData, core, type_from_qname, type_qname};
use bsl_lib::xdto::QName;
use bsl_vm::{Exception, Execution, ObjRef, TypeId, TypeIdx, Value};
use std::{mem::size_of, sync::Arc};

struct Description;
struct Qualifiers(&'static str);

pub(crate) fn codecs() -> Vec<(TypeIdx, Arc<dyn XdtoCodec>)> {
    let mut codecs: Vec<(TypeIdx, Arc<dyn XdtoCodec>)> =
        vec![(idx("TypeDescription"), Arc::new(Description))];
    for name in [
        "NumberQualifiers",
        "StringQualifiers",
        "DateQualifiers",
        "BinaryDataQualifiers",
    ] {
        codecs.push((idx(name), Arc::new(Qualifiers(name))));
    }
    codecs
}

fn write_admission(
    ex: &mut Execution,
    td: &TypeDescription,
    s: &mut XdtoCtx,
) -> Result<(), Exception> {
    ex.charge(td.types().len() as u64)?;
    let mut bytes = (td.types().len() as u64)
        .saturating_mul(1024)
        .saturating_add(8192);
    for &ty in td.types() {
        let names = if let Some(app) = ex.runtime().types().app_type(ty) {
            app.xdto.as_ref().map_or(app.name_en.len(), |q| {
                q.namespace.len().saturating_add(q.name.len())
            })
        } else if let Some(index) = ex.runtime().types().catalog_type(ty) {
            let info = platform_catalog::catalog().type_info(index);
            info.xdto.as_ref().map_or(info.name_en.len(), |q| {
                q.namespace.len().saturating_add(q.name.len())
            })
        } else {
            0
        };
        bytes = bytes.saturating_add((names as u64).saturating_mul(4));
    }
    s.account(ex, bytes)
}

impl XdtoCodec for Description {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        value: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception> {
        let Some(td) = value.as_any().downcast_ref::<TypeDescription>() else {
            return Err(s.mismatch(ex));
        };
        write_admission(ex, td, s)?;
        let mut props = Vec::with_capacity(5);
        let mut types = Vec::with_capacity(td.types().len());
        for &ty in td.types() {
            if ty != TypeId::UNDEFINED {
                types.push(XdtoData::Value {
                    projection: None,
                    ty: QName::new(crate::xdto::XS, "QName"),
                    lexical: type_qname(ex, ty).clark(),
                    value: Value::Type(ty),
                });
            }
        }
        if !types.is_empty() {
            props.push(("Type".into(), types));
        }
        for (ty, q) in [
            (TypeId::NUMBER, Qualifier::Number(td.number())),
            (TypeId::STRING, Qualifier::String(td.string())),
            (TypeId::DATE, Qualifier::Date(td.date())),
            (
                TypeId::of_catalog(idx("BinaryData")),
                Qualifier::Binary(td.binary()),
            ),
        ] {
            if td.contains(ty) {
                props.push((q.id().into(), vec![qualifier_data(q)]));
            }
        }
        Ok(XdtoData::Object {
            ty: core("TypeDescription"),
            props,
        })
    }

    fn from_xdto(
        &self,
        ex: &mut Execution,
        data: &XdtoData,
        s: &mut XdtoCtx,
    ) -> Result<Value, Exception> {
        let XdtoData::Object { props, .. } = data else {
            return Err(s.mismatch(ex));
        };
        ex.charge(props.len() as u64)?;
        let entries = prop(props, "Type").unwrap_or_default();
        ex.charge(entries.len() as u64)?;
        let mut input_bytes = 0u64;
        for entry in entries {
            if let XdtoData::Value { lexical, .. } = entry {
                input_bytes = input_bytes.saturating_add(lexical.len() as u64);
            }
        }
        s.account(ex, input_bytes.saturating_mul(2).saturating_add(256))?;
        let n = entries.len() as u64;
        ex.charge(n.saturating_mul(u64::from(n.max(1).ilog2()) + 1))?;
        ex.reserve(
            n.saturating_mul(4 * size_of::<TypeId>() as u64)
                .saturating_add(crate::collections::retained_rc_size::<TypeDescription>()),
        )?;
        let mut types = Vec::new();
        types
            .try_reserve_exact(entries.len())
            .map_err(|_| ex.out_of_memory())?;
        for entry in entries {
            let XdtoData::Value { lexical, .. } = entry else {
                return Err(s.mismatch(ex));
            };
            let ty = type_from_qname(ex, lexical)
                .ok_or_else(|| s.conversion_error(ex, lexical, &core("TypeDescription")))?;
            types.push(ty);
        }
        let mut number = None;
        let mut string = None;
        let mut date = None;
        let mut binary = None;
        for (name, target) in [
            ("NumberQualifiers", &mut number),
            ("StringQualifiers", &mut string),
            ("DateQualifiers", &mut date),
            ("BinaryDataQualifiers", &mut binary),
        ] {
            if let Some(data) = prop(props, name).and_then(|values| values.first()) {
                *target = Some(read_qualifier(ex, data, name, s)?);
            }
        }
        Ok(new_type_description(TypeDescription::new(
            &types,
            number.and_then(|q| {
                if let Qualifier::Number(q) = q {
                    Some(q)
                } else {
                    None
                }
            }),
            string.and_then(|q| {
                if let Qualifier::String(q) = q {
                    Some(q)
                } else {
                    None
                }
            }),
            date.and_then(|q| {
                if let Qualifier::Date(q) = q {
                    Some(q)
                } else {
                    None
                }
            }),
            binary.and_then(|q| {
                if let Qualifier::Binary(q) = q {
                    Some(q)
                } else {
                    None
                }
            }),
            ex.runtime(),
        )))
    }

    fn xdto_type(&self) -> QName {
        core("TypeDescription")
    }
}

fn prop<'a>(props: &'a [(String, Vec<XdtoData>)], name: &str) -> Option<&'a [XdtoData]> {
    props
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, values)| values.as_slice())
}

fn scalar(ty: QName, lexical: &str, value: Value) -> XdtoData {
    XdtoData::Value {
        projection: None,
        ty,
        lexical: lexical.into(),
        value,
    }
}

fn decimal(value: u32) -> XdtoData {
    scalar(
        QName::new(crate::xdto::XS, "decimal"),
        &value.to_string(),
        Value::from(i64::from(value)),
    )
}

fn enumeration(ty: &str, lexical: &str) -> XdtoData {
    scalar(core(ty), lexical, Value::Undefined)
}

fn field(name: &str, value: XdtoData) -> (String, Vec<XdtoData>) {
    (name.into(), vec![value])
}

fn qualifier_data(q: Qualifier) -> XdtoData {
    let props = match q {
        Qualifier::Number(q) => vec![
            field("Digits", decimal(q.digits)),
            field("FractionDigits", decimal(q.fraction)),
            field(
                "AllowedSign",
                enumeration(
                    "AllowedSign",
                    if q.sign == AllowedSign::Any {
                        "Any"
                    } else {
                        "Nonnegative"
                    },
                ),
            ),
        ],
        Qualifier::String(StringQualifiers { length, allowed })
        | Qualifier::Binary(BinaryQualifiers { length, allowed }) => vec![
            field("Length", decimal(length)),
            field(
                "AllowedLength",
                enumeration(
                    "AllowedLength",
                    if allowed == AllowedLength::Variable {
                        "Variable"
                    } else {
                        "Fixed"
                    },
                ),
            ),
        ],
        Qualifier::Date(q) => vec![field(
            "DateFractions",
            enumeration(
                "DateFractions",
                match q.parts {
                    DateParts::Date => "Date",
                    DateParts::Time => "Time",
                    DateParts::DateTime => "DateTime",
                },
            ),
        )],
    };
    XdtoData::Object {
        ty: core(q.id()),
        props,
    }
}

fn read_qualifier(
    ex: &mut Execution,
    data: &XdtoData,
    name: &str,
    s: &mut XdtoCtx,
) -> Result<Qualifier, Exception> {
    let XdtoData::Object { props, .. } = data else {
        return Err(s.mismatch(ex));
    };
    ex.charge(props.len() as u64)?;
    let text = |name| match prop(props, name).and_then(|values| values.first()) {
        Some(XdtoData::Value { lexical, .. }) => Some(lexical.as_str()),
        _ => None,
    };
    let number = |name| {
        text(name)
            .filter(|text| text.len() <= 10)
            .and_then(|text| text.parse::<u32>().ok())
    };
    let q = match name {
        "NumberQualifiers" => {
            let (Some(digits), Some(fraction), Some(sign)) = (
                number("Digits"),
                number("FractionDigits"),
                text("AllowedSign"),
            ) else {
                return Err(s.mismatch(ex));
            };
            if digits > 32 || fraction > digits {
                return Err(s.mismatch(ex));
            }
            Qualifier::Number(NumberQualifiers {
                digits,
                fraction,
                sign: match sign {
                    "Any" => AllowedSign::Any,
                    "Nonnegative" => AllowedSign::Nonnegative,
                    _ => return Err(s.mismatch(ex)),
                },
            })
        }
        "StringQualifiers" | "BinaryDataQualifiers" => {
            let (Some(length), Some(allowed)) = (number("Length"), text("AllowedLength")) else {
                return Err(s.mismatch(ex));
            };
            let allowed = match allowed {
                "Variable" => AllowedLength::Variable,
                "Fixed" => AllowedLength::Fixed,
                _ => return Err(s.mismatch(ex)),
            };
            if name == "StringQualifiers" {
                Qualifier::String(StringQualifiers { length, allowed })
            } else {
                Qualifier::Binary(BinaryQualifiers { length, allowed })
            }
        }
        "DateQualifiers" => Qualifier::Date(DateQualifiers {
            parts: match text("DateFractions") {
                Some("Date") => DateParts::Date,
                Some("Time") => DateParts::Time,
                Some("DateTime") => DateParts::DateTime,
                _ => return Err(s.mismatch(ex)),
            },
        }),
        _ => return Err(s.mismatch(ex)),
    };
    Ok(q)
}

impl XdtoCodec for Qualifiers {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        value: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception> {
        let Some(&q) = value.as_any().downcast_ref::<Qualifier>() else {
            return Err(s.mismatch(ex));
        };
        s.account(ex, 4096)?;
        Ok(qualifier_data(q))
    }
    fn from_xdto(
        &self,
        ex: &mut Execution,
        data: &XdtoData,
        s: &mut XdtoCtx,
    ) -> Result<Value, Exception> {
        let q = read_qualifier(ex, data, self.0, s)?;
        ex.reserve(crate::collections::retained_rc_size::<Qualifier>())?;
        Ok(q.value())
    }
    fn xdto_type(&self) -> QName {
        core(self.0)
    }
}
