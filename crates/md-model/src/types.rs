use crate::{Configuration, MdKind, MdObject, TypeDescription, TypeRef, Value};
use std::collections::HashSet;

impl Configuration {
    pub fn expand_type(&self, t: &TypeDescription) -> TypeDescription {
        expand_with(t, &|r| {
            self.set_target(r).and_then(|o| match o.prop("Type") {
                Some(Value::Type(inner)) => Some(inner.clone()),
                _ => None,
            })
        })
    }

    fn set_target(&self, r: &TypeRef) -> Option<&MdObject> {
        let (kind, name) = if let Some(n) = r.qname.strip_prefix("cfg:DefinedType.") {
            (MdKind::DefinedType, n)
        } else {
            let n = r.qname.strip_prefix("cfg:Characteristic.")?;
            (MdKind::ChartOfCharacteristicTypes, n)
        };
        let by_id = r
            .type_id
            .and_then(|id| self.generated_type(id))
            .map(|g| g.object)
            .filter(|o| o.kind == kind);
        by_id.or_else(|| self.find(kind, name))
    }
}

#[doc(hidden)]
pub fn expand_with(
    t: &TypeDescription,
    set_types: &dyn Fn(&TypeRef) -> Option<TypeDescription>,
) -> TypeDescription {
    let mut out = TypeDescription {
        types: Vec::new(),
        number: t.number,
        string: t.string,
        date: t.date,
        binary: t.binary,
    };
    let mut frames: Vec<(std::vec::IntoIter<TypeRef>, Option<String>)> =
        vec![(t.types.clone().into_iter(), None)];
    let mut done: HashSet<String> = HashSet::new();
    while let Some((iter, _)) = frames.last_mut() {
        let Some(r) = iter.next() else {
            if let Some((_, Some(name))) = frames.pop() {
                done.insert(name);
            }
            continue;
        };
        if done.contains(&r.qname) {
            continue;
        }
        let target = set_types(&r).filter(|inner| !inner.types.is_empty());
        match target {
            Some(inner) if !frames.iter().any(|(_, f)| f.as_deref() == Some(&r.qname)) => {
                out.number = out.number.or(inner.number);
                out.string = out.string.or(inner.string);
                out.date = out.date.or(inner.date);
                out.binary = out.binary.or(inner.binary);
                frames.push((inner.types.into_iter(), Some(r.qname)));
            }
            _ => {
                if !out.types.iter().any(|x| x.qname == r.qname) {
                    out.types.push(r);
                }
            }
        }
    }
    out
}
