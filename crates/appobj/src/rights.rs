use crate::model::Uuid16;
use md_model::MdKind;
use md_model::parts::{Right as MdRight, RightTarget as MdTarget, Rights};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightTarget<'a> {
    Object(Uuid16),
    HttpMethod {
        service: Uuid16,
        template: &'a str,
        method: &'a str,
    },
    Configuration,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Right(pub MdRight);

#[derive(Clone, Debug)]
struct Bits(Vec<u64>);

impl Bits {
    fn filled(words: usize, on: bool) -> Bits {
        Bits(vec![if on { u64::MAX } else { 0 }; words])
    }

    fn set(&mut self, i: usize, on: bool) {
        if let Some(w) = self.0.get_mut(i / 64) {
            let m = 1u64 << (i % 64);
            if on {
                *w |= m;
            } else {
                *w &= !m;
            }
        }
    }

    fn get(&self, i: usize) -> bool {
        self.0
            .get(i / 64)
            .is_some_and(|w| w & (1u64 << (i % 64)) != 0)
    }
}

#[derive(Clone, Debug)]
struct RoleRights {
    default: bool,
    configuration: Option<Bits>,
    objects: HashMap<Uuid16, Bits>,
}

#[derive(Debug, Default)]
pub(crate) struct RightsMatrix {
    index: HashMap<MdRight, usize>,
    role_ix: HashMap<Uuid16, usize>,
    roles: Vec<RoleRights>,
}

impl RightsMatrix {
    pub(crate) fn build(config: &md_model::Configuration) -> RightsMatrix {
        let index: HashMap<MdRight, usize> =
            MdRight::all().enumerate().map(|(i, r)| (r, i)).collect();
        let words = index.len().div_ceil(64);
        let mut m = RightsMatrix {
            index,
            ..RightsMatrix::default()
        };
        for role in config.of_kind(MdKind::Role) {
            let rr = match role.rights() {
                Some(r) => role_rights(r, &m.index, words),
                None => RoleRights {
                    default: false,
                    configuration: None,
                    objects: HashMap::new(),
                },
            };
            m.role_ix.insert(role.id, m.roles.len());
            m.roles.push(rr);
        }
        m
    }

    pub(crate) fn any_role(&self, roles: &[Uuid16], target: Option<&Uuid16>, right: Right) -> bool {
        let Some(&i) = self.index.get(&right.0) else {
            return false;
        };
        roles.iter().any(|r| {
            let Some(rr) = self.role_ix.get(r).and_then(|&k| self.roles.get(k)) else {
                return false;
            };
            let bits = match target {
                Some(t) => rr.objects.get(t),
                None => rr.configuration.as_ref(),
            };
            bits.map_or(rr.default, |b| b.get(i))
        })
    }

    pub(crate) fn has_role(&self, id: &Uuid16) -> bool {
        self.role_ix.contains_key(id)
    }
}

fn role_rights(r: &Rights, index: &HashMap<MdRight, usize>, words: usize) -> RoleRights {
    let mut rr = RoleRights {
        default: r.set_for_new_objects,
        configuration: None,
        objects: HashMap::new(),
    };
    let mut seen: HashMap<Option<Uuid16>, Vec<bool>> = HashMap::new();
    for o in &r.objects {
        let key = match &o.target {
            MdTarget::Configuration => None,
            MdTarget::Object(id) | MdTarget::Child { id, .. } => Some(*id),
            MdTarget::StandardAttribute { .. } => continue,
        };
        let slot = match key {
            Some(id) => rr
                .objects
                .entry(id)
                .or_insert_with(|| Bits::filled(words, r.default_for(&o.target))),
            None => rr
                .configuration
                .get_or_insert_with(|| Bits::filled(words, r.default_for(&o.target))),
        };
        let seen = seen.entry(key).or_insert_with(|| vec![false; index.len()]);
        for rv in &o.rights {
            let Some(&i) = index.get(&rv.right) else {
                continue;
            };
            if seen.get(i).copied().unwrap_or(true) {
                continue;
            }
            if let Some(s) = seen.get_mut(i) {
                *s = true;
            }
            slot.set(i, rv.value);
        }
    }
    rr
}

pub(crate) fn register(app: &std::sync::Arc<crate::Application>, reg: &mut bsl_vm::Registry) {
    use crate::meta::{MdKey, arg_value, key_of};
    use crate::model::MdPath;
    use crate::session::call_ctx;
    use bsl_vm::{Arg, CallResult, Execution, HostError, Value};

    reg.global_fn(
        "ПривилегированныйРежим",
        "PrivilegedMode",
        std::sync::Arc::new(|ex: &mut Execution, _: &mut [Arg]| -> CallResult<Value> {
            Ok(Value::Bool(ex.effective_flags().privileged))
        }),
    );
    reg.global_fn(
        "ИмяПользователя",
        "UserName",
        std::sync::Arc::new(|ex: &mut Execution, _: &mut [Arg]| -> CallResult<Value> {
            let name = call_ctx(ex).map_or(String::new(), |c| c.sess.user().name.clone());
            Ok(Value::from(name.as_str()))
        }),
    );
    reg.global_fn(
        "ПолноеИмяПользователя",
        "UserFullName",
        std::sync::Arc::new(|ex: &mut Execution, _: &mut [Arg]| -> CallResult<Value> {
            let name = call_ctx(ex).map_or(String::new(), |c| c.sess.user().full_name.clone());
            Ok(Value::from(name.as_str()))
        }),
    );
    let app = app.clone();
    reg.global_fn(
        "РольДоступна",
        "IsInRole",
        std::sync::Arc::new(
            move |ex: &mut Execution, args: &mut [Arg]| -> CallResult<Value> {
                let v = arg_value(ex, args, 0)?;
                let bad = || -> bsl_vm::CallError {
                    let mut e = HostError::key(bsl_stdlib::bind::msg::VALUE_MISMATCH);
                    e.msg = bsl_vm::error::MsgRef::Key(bsl_vm::error::MsgKey {
                        key: bsl_stdlib::bind::msg::VALUE_MISMATCH,
                        args: vec!["1".into()],
                        suffix: String::new(),
                    });
                    e.into()
                };
                let cfg = &app.model().config;
                let role = match &v {
                    Value::String(s) => {
                        let want = bsl_prim::fold_name(&s.to_string_lossy());
                        cfg.of_kind(MdKind::Role)
                            .find(|r| bsl_prim::fold_name(&r.name) == want)
                            .map(|r| r.id)
                    }
                    other => match key_of(other) {
                        Some(MdKey::Path(MdPath::Other(i))) => cfg
                            .objects
                            .get(i)
                            .filter(|o| o.kind == MdKind::Role)
                            .map(|o| o.id),
                        _ => None,
                    },
                };
                let Some(role) = role.filter(|r| app.rights.has_role(r)) else {
                    return Err(bad());
                };
                let member = call_ctx(ex)
                    .map(|c| c.sess.user())
                    .is_some_and(|u| !u.no_users && u.roles.contains(&role));
                Ok(Value::Bool(member))
            },
        ),
    );
}
