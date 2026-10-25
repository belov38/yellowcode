use super::XdtoCtx;
use crate::serial::cycle_fatal;
use crate::serial::depth::DepthGuard;
use crate::xdto::data::{ScalarProjection, XdtoData, data_copy_sizes, property_type_error};
use crate::xdto::{EProp, check_structure, object_type};
use bsl_lib::xdto::{Form, QName};
use bsl_vm::{AppXdto, AppXdtoProp, AppXdtoValue, Exception, Execution, Value};
use std::sync::Arc;

fn matches(ep: &EProp, parent_ns: &str, prop: &AppXdtoProp) -> bool {
    ep.xml_name() == prop.name
        && ep.xml_ns().unwrap_or("") == prop.ns.as_deref().unwrap_or(parent_ns)
}

fn ambiguous_props(props: &[EProp]) -> bool {
    props.iter().enumerate().any(|(i, p)| {
        props[..i].iter().any(|other| {
            p.prop.name == other.prop.name
                || (p.xml_name() == other.xml_name() && p.xml_ns() == other.xml_ns())
        })
    })
}

fn host_bytes(
    ex: &mut Execution,
    props: &Vec<AppXdtoProp>,
    guard: &mut DepthGuard,
) -> Result<u64, Exception> {
    if guard.enter().is_err() {
        return Err(cycle_fatal(ex));
    }
    let mut bytes =
        (props.capacity() as u64).saturating_mul(std::mem::size_of::<AppXdtoProp>() as u64);
    for prop in props {
        ex.charge(1)?;
        bytes = bytes
            .saturating_add(prop.name.capacity() as u64)
            .saturating_add(prop.ns.as_ref().map_or(0, |s| s.capacity() as u64));
        bytes = bytes.saturating_add(match &prop.value {
            AppXdtoValue::Object(children) => host_bytes(ex, children, guard)?,
            AppXdtoValue::Text(text) => text.capacity() as u64,
            AppXdtoValue::Value {
                value: Value::String(text),
                ..
            } => (text.len() as u64).saturating_mul(2),
            AppXdtoValue::Value {
                value: Value::Number(n),
                ..
            } => n.size_hint_limbs().saturating_mul(4),
            _ => 0,
        });
    }
    guard.leave();
    Ok(bytes)
}

impl XdtoCtx {
    fn applied_props(
        &mut self,
        ex: &mut Execution,
        ty: &QName,
    ) -> Result<Arc<Vec<EProp>>, Exception> {
        let factory = self.factory.clone();
        let Some(def) = object_type(&factory.model, ty) else {
            return Err(self.mismatch(ex));
        };
        if !def.ordered || def.open || def.mixed || def.sequenced {
            return Err(self.mismatch(ex));
        }
        let mut at = Some(ty);
        let mut bytes = 0u64;
        for _ in 0..crate::xdto::MAX_BASES {
            let Some(def) = at.and_then(|q| object_type(&factory.model, q)) else {
                break;
            };
            if !def.ordered || def.open || def.mixed || def.sequenced {
                return Err(self.mismatch(ex));
            }
            ex.charge(def.properties.len() as u64 + 1)?;
            for prop in &def.properties {
                bytes = bytes
                    .saturating_add((std::mem::size_of::<EProp>() as u64).saturating_mul(2))
                    .saturating_add(
                        (prop.name.len()
                            + prop.ty.ns.len()
                            + prop.ty.name.len()
                            + def.name.ns.len()) as u64,
                    )
                    .saturating_add(prop.local_name.as_ref().map_or(0, |s| s.len() as u64))
                    .saturating_add(prop.default.as_ref().map_or(0, |s| s.len() as u64))
                    .saturating_add(prop.fixed.as_ref().map_or(0, |s| s.len() as u64));
            }
            at = def.base.as_ref();
        }
        self.account(ex, bytes)?;
        let props = factory
            .props
            .get(ex, &factory.model, ty)
            .map_err(|e| self.exc(e))?;
        let n = props.len() as u64;
        ex.charge(n.saturating_mul(n.saturating_add(1)))?;
        if ambiguous_props(&props) {
            return Err(self.mismatch(ex));
        }
        if props.iter().any(|p| p.prop.form != Form::Element) {
            return Err(self.mismatch(ex));
        }
        Ok(props)
    }

    fn host_structure(
        &mut self,
        ex: &mut Execution,
        ty: &QName,
        ns: &str,
        props: &[AppXdtoProp],
        guard: &mut DepthGuard,
    ) -> Result<(), Exception> {
        if guard.enter().is_err() {
            return Err(cycle_fatal(ex));
        }
        let eps = self.applied_props(ex, ty)?;
        let e = eps.len() as u64;
        let p = props.len() as u64;
        ex.charge(e.saturating_mul(p.saturating_mul(2).saturating_add(e).saturating_add(1)))?;
        for prop in props {
            let Some(ep) = eps.iter().find(|ep| matches(ep, ns, prop)) else {
                return Err(self.mismatch(ex));
            };
            match &prop.value {
                AppXdtoValue::Text(_) => return Err(self.mismatch(ex)),
                AppXdtoValue::Nil if !ep.prop.nillable => {
                    return Err(self.fail(property_type_error()));
                }
                AppXdtoValue::Object(children) => self.host_structure(
                    ex,
                    &ep.prop.ty,
                    ep.xml_ns().unwrap_or(""),
                    children,
                    guard,
                )?,
                _ => {}
            }
        }
        if let Err(error) = check_structure(ex, ty, &eps, true, &|name| {
            eps.iter()
                .find(|p| p.prop.name == name)
                .map_or(0, |ep| props.iter().filter(|p| matches(ep, ns, p)).count())
        }) {
            return Err(self.fail((*error).1));
        }
        guard.leave();
        Ok(())
    }

    pub(super) fn applied_object_data(
        &mut self,
        ex: &mut Execution,
        ty: QName,
        props: &Vec<AppXdtoProp>,
    ) -> Result<XdtoData, Exception> {
        let retained = host_bytes(ex, props, &mut DepthGuard::new())?;
        self.account(ex, retained)?;
        self.host_structure(ex, &ty, &ty.ns, props, &mut DepthGuard::new())?;
        self.host_body(ex, &ty, &ty.ns, props)
    }

    fn host_body(
        &mut self,
        ex: &mut Execution,
        ty: &QName,
        ns: &str,
        props: &[AppXdtoProp],
    ) -> Result<XdtoData, Exception> {
        let eps = self.applied_props(ex, ty)?;
        ex.charge(
            (eps.len() as u64)
                .saturating_mul((props.len() as u64).saturating_mul(2).saturating_add(1)),
        )?;
        self.account(
            ex,
            (eps.len() as u64)
                .saturating_mul(std::mem::size_of::<(String, Vec<XdtoData>)>() as u64)
                .saturating_add((ty.ns.len() + ty.name.len()) as u64),
        )?;
        let mut out = Vec::with_capacity(eps.len());
        for ep in eps.iter() {
            let n = props.iter().filter(|p| matches(ep, ns, p)).count();
            if n == 0 {
                continue;
            }
            self.account(
                ex,
                (n as u64)
                    .saturating_mul(std::mem::size_of::<XdtoData>() as u64)
                    .saturating_add(ep.prop.name.len() as u64),
            )?;
            let mut values = Vec::with_capacity(n);
            for prop in props.iter().filter(|p| matches(ep, ns, p)) {
                values.push(match &prop.value {
                    AppXdtoValue::Nil => XdtoData::Nil,
                    AppXdtoValue::Text(_) => return Err(self.mismatch(ex)),
                    AppXdtoValue::Value { value, typed } => {
                        let mut data = self.to_data(ex, value)?;
                        let XdtoData::Value { projection, .. } = &mut data else {
                            return Err(self.mismatch(ex));
                        };
                        *projection = Some(ScalarProjection::Host { typed: *typed });
                        data
                    }
                    AppXdtoValue::Object(children) => {
                        self.enter(ex, false)?;
                        let result =
                            self.host_body(ex, &ep.prop.ty, ep.xml_ns().unwrap_or(""), children);
                        self.leave();
                        result?
                    }
                });
            }
            out.push((ep.prop.name.clone(), values));
        }
        Ok(XdtoData::Object {
            ty: ty.clone(),
            props: out,
        })
    }

    fn wire_structure(
        &mut self,
        ex: &mut Execution,
        data: &XdtoData,
        guard: &mut DepthGuard,
    ) -> Result<(), Exception> {
        if guard.enter().is_err() {
            return Err(cycle_fatal(ex));
        }
        let XdtoData::Object { ty, props } = data else {
            return Err(self.mismatch(ex));
        };
        let eps = self.applied_props(ex, ty)?;
        let e = eps.len() as u64;
        let p = props.len() as u64;
        let items = props
            .iter()
            .fold(0u64, |n, (_, values)| n.saturating_add(values.len() as u64));
        ex.charge(
            p.saturating_mul(p)
                .saturating_add(e.saturating_mul(p.saturating_mul(2).saturating_add(1)))
                .saturating_add(items),
        )?;
        for (name, items) in props {
            let Some(ep) = eps.iter().find(|ep| ep.prop.name == *name) else {
                return Err(self.mismatch(ex));
            };
            if props.iter().filter(|(key, _)| key == name).count() != 1 {
                return Err(self.mismatch(ex));
            }
            for item in items {
                match item {
                    XdtoData::Nil if !ep.prop.nillable => {
                        return Err(self.fail(property_type_error()));
                    }
                    XdtoData::Object { ty, .. } => {
                        if *ty != ep.prop.ty {
                            return Err(self.mismatch(ex));
                        }
                        self.wire_structure(ex, item, guard)?;
                    }
                    XdtoData::Value {
                        projection: Some(ScalarProjection::Read { .. }),
                        ..
                    } => {}
                    XdtoData::Nil => {}
                    _ => return Err(self.mismatch(ex)),
                }
            }
        }
        if let Err(error) = check_structure(ex, ty, &eps, true, &|name| {
            props
                .iter()
                .find(|(key, _)| key == name)
                .map_or(0, |(_, items)| items.len())
        }) {
            return Err(self.fail((*error).1));
        }
        guard.leave();
        Ok(())
    }

    pub(super) fn applied_object_value(
        &mut self,
        ex: &mut Execution,
        data: &XdtoData,
    ) -> Result<AppXdto, Exception> {
        let (retained, _) = data_copy_sizes(ex, data)?;
        self.account(ex, retained)?;
        self.wire_structure(ex, data, &mut DepthGuard::new())?;
        let XdtoData::Object { ty, .. } = data else {
            return Err(self.mismatch(ex));
        };
        Ok(AppXdto::Object(self.wire_body(ex, data, &ty.ns)?))
    }

    fn wire_body(
        &mut self,
        ex: &mut Execution,
        data: &XdtoData,
        ns: &str,
    ) -> Result<Vec<AppXdtoProp>, Exception> {
        let XdtoData::Object { ty, props } = data else {
            return Err(self.mismatch(ex));
        };
        let eps = self.applied_props(ex, ty)?;
        let n = props
            .iter()
            .fold(0usize, |n, (_, v)| n.saturating_add(v.len()));
        if n > isize::MAX as usize / std::mem::size_of::<AppXdtoProp>() {
            return Err(ex.out_of_memory());
        }
        self.account(
            ex,
            (n as u64).saturating_mul(std::mem::size_of::<AppXdtoProp>() as u64),
        )?;
        ex.charge(
            (eps.len() as u64)
                .saturating_mul(props.len() as u64 + 1)
                .saturating_add(n as u64),
        )?;
        let mut out = Vec::with_capacity(n);
        for ep in eps.iter() {
            let Some((_, values)) = props.iter().find(|(name, _)| *name == ep.prop.name) else {
                continue;
            };
            let child_ns = ep.xml_ns().unwrap_or("");
            for value in values {
                self.account(
                    ex,
                    ep.xml_name().len() as u64
                        + if child_ns == ns {
                            0
                        } else {
                            child_ns.len() as u64
                        },
                )?;
                let projected = match value {
                    XdtoData::Nil => AppXdtoValue::Nil,
                    XdtoData::Object { .. } => {
                        self.enter(ex, true)?;
                        let result = self.wire_body(ex, value, child_ns);
                        self.leave();
                        AppXdtoValue::Object(result?)
                    }
                    XdtoData::Value {
                        projection:
                            Some(ScalarProjection::Read {
                                explicit_type,
                                source_text,
                            }),
                        ..
                    } => {
                        if *explicit_type {
                            AppXdtoValue::Value {
                                value: self.from_data(ex, value)?,
                                typed: true,
                            }
                        } else {
                            self.account(ex, source_text.len() as u64)?;
                            AppXdtoValue::Text(source_text.clone())
                        }
                    }
                    _ => return Err(self.mismatch(ex)),
                };
                out.push(AppXdtoProp {
                    name: ep.xml_name().to_string(),
                    ns: (child_ns != ns).then(|| child_ns.to_string()),
                    value: projected,
                });
            }
        }
        Ok(out)
    }
}
