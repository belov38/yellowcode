use super::{Category, CategorySet, ErrorInfo, StackEntry};
use crate::Value;
use bsl_prim::Str;
use std::rc::{Rc, Weak};

const CLASS: [u8; 16] = [
    0x8d, 0x36, 0x60, 0x56, 0x4d, 0x5a, 0x4d, 0x88, 0xa2, 0x07, 0x0a, 0xe5, 0x35, 0xb7, 0xd2, 0x8e,
];

#[derive(Clone, Debug)]
pub struct ErrorWireIdentityParts {
    pub class_id: [u8; 16],
    pub reason: u32,
    pub positioned_description: Str,
    pub machine_stack: Str,
    pub category_width: u8,
    pub category_bits: u32,
    pub ui_helper_id: [u8; 16],
    pub module_id: Str,
}

#[derive(Clone, Debug)]
pub struct ErrorWireIdentity(ErrorWireIdentityParts);

impl ErrorWireIdentity {
    pub fn new(parts: ErrorWireIdentityParts) -> Option<Self> {
        let mask = (1 << 5) | (1 << 6) | (1 << 7) | (1 << 21);
        (parts.class_id == CLASS
            && matches!(parts.reason, 8 | 36)
            && parts.category_width == 26
            && parts.category_bits & !mask == 0)
            .then_some(Self(parts))
    }

    pub fn parts(&self) -> &ErrorWireIdentityParts {
        &self.0
    }

    pub fn categories(&self) -> CategorySet {
        let mut out = CategorySet::default();
        for (bit, category) in [
            (5, Category::ScriptRuntimeError),
            (6, Category::ScriptUseError),
            (7, Category::ExceptionRaisedFromScript),
            (21, Category::ConfigurationError),
        ] {
            if self.0.category_bits & (1 << bit) != 0 {
                out = out.with(category);
            }
        }
        out
    }
}

#[derive(Debug)]
pub struct ErrorWireMetadata {
    identity: ErrorWireIdentity,
    frame_module_ids: Vec<Str>,
    snapshot: Projection,
}

#[derive(Debug)]
struct Projection {
    description: Str,
    module_name: Str,
    line: u32,
    source_line: Str,
    code: Str,
    additional: Str,
    categories: CategorySet,
    stack: Vec<StackEntry>,
    key: Option<&'static str>,
    cause: Option<Weak<ErrorInfo>>,
}

impl ErrorWireMetadata {
    pub fn identity(&self) -> &ErrorWireIdentity {
        &self.identity
    }
    pub fn frame_module_ids(&self) -> &[Str] {
        &self.frame_module_ids
    }

    fn matches(&self, info: &ErrorInfo) -> bool {
        let s = &self.snapshot;
        let cause_matches = match (&s.cause, &info.cause) {
            (None, None) => true,
            (Some(old), Some(now)) => old.upgrade().is_some_and(|old| Rc::ptr_eq(&old, now)),
            _ => false,
        };
        cause_matches
            && s.description == info.description
            && s.module_name == info.module_name
            && s.line == info.line
            && s.source_line == info.source_line
            && s.code == info.code
            && matches!(&info.additional, Value::String(text) if *text == s.additional)
            && s.categories == info.categories
            && s.stack == info.stack
            && s.key == info.key
    }
}

impl ErrorInfo {
    pub fn new(description: Str, categories: CategorySet) -> Self {
        Self {
            description,
            module_name: Str::new(),
            line: 0,
            source_line: Str::new(),
            cause: None,
            code: Str::new(),
            additional: Value::Undefined,
            categories,
            stack: Vec::new(),
            key: None,
            wire: None,
        }
    }

    pub fn attach_wire_metadata(
        &mut self,
        identity: ErrorWireIdentity,
        frame_module_ids: Vec<Str>,
    ) -> Result<(), &'static str> {
        let Value::String(additional) = &self.additional else {
            return Err("wire additional information must be a string");
        };
        if self.wire.is_some()
            || frame_module_ids.len() != self.stack.len()
            || identity.categories() != self.categories
        {
            return Err("wire identity does not match the error projection");
        }
        let snapshot = Projection {
            description: self.description.clone(),
            module_name: self.module_name.clone(),
            line: self.line,
            source_line: self.source_line.clone(),
            code: self.code.clone(),
            additional: additional.clone(),
            categories: self.categories,
            stack: self.stack.clone(),
            key: self.key,
            cause: self.cause.as_ref().map(Rc::downgrade),
        };
        self.wire = Some(Box::new(ErrorWireMetadata {
            identity,
            frame_module_ids,
            snapshot,
        }));
        Ok(())
    }

    pub fn wire_metadata(&self) -> Option<&ErrorWireMetadata> {
        self.wire.as_deref()
    }
    pub fn wire_matches_projection(&self) -> bool {
        self.wire.as_ref().is_some_and(|wire| wire.matches(self))
    }
}
