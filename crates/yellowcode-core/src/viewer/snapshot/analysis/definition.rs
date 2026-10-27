use super::{SelectedAnalysis, SelectedAnalysisError};
use crate::viewer::{
    analysis_parent::AnalysisError,
    body::ViewerBody,
    module::{self, ManagedModuleError},
    names::eq_path,
};
use mdconf::{ManagedEntrySource, MdKind, ModuleKind, startup::ModuleLocation};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, MemoryBudget};
use serde::{
    Serialize,
    de::{DeserializeSeed, Visitor},
};
use std::time::{Duration, Instant};

#[derive(Serialize)]
struct Probe<'a> {
    path: &'a str,
    module: &'a str,
    line: u32,
    col: u32,
}
#[derive(Serialize)]
struct Method<'a> {
    path: &'a str,
    name: &'a str,
    exported_only: bool,
}

impl<S: ManagedEntrySource> SelectedAnalysis<'_, S> {
    pub fn definition_body(
        &self,
        path: &str,
        which: &str,
        line: u32,
        col: u32,
    ) -> Result<Option<ViewerBody>, SelectedAnalysisError<S::Error>> {
        let deadline = Instant::now() + Duration::from_secs(120);
        self.check()?;
        let open = self.snapshot.open();
        let key = match module::module_of_managed(open, &self.snapshot.names(), path, Some(which)) {
            Ok(key) => key,
            Err(ManagedModuleError::Path(_) | ManagedModuleError::Selection(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let probe = self.run_before(
            key,
            "definition_probe",
            Probe {
                path,
                module: which,
                line,
                col,
            },
            Some(deadline),
        )?;
        let mut cursor = Cursor::new(probe.as_ref(), || self.check_before(deadline));
        let [attempts] = cursor.fields([b"attempts"])?;
        cursor.finish()?;
        let mut attempts = Cursor::new(attempts, || self.check_before(deadline));
        attempts.punct(b'[')?;
        let first = attempts.value()?;
        attempts.punct(b',')?;
        let second = attempts.value()?;
        attempts.punct(b']')?;
        attempts.finish()?;
        for candidate in [first, second] {
            let mut candidate = Cursor::new(candidate, || self.check_before(deadline));
            let fields = candidate.optional_fields([
                b"kind".as_slice(),
                b"location",
                b"qualifier",
                b"method",
            ])?;
            candidate.finish()?;
            let kind = fields[0].ok_or(AnalysisError::Protocol)?;
            match (kind, fields[1], fields[2], fields[3]) {
                (b"\"missing\"", None, None, None) => {}
                (b"\"own\"", Some(location), None, None) => {
                    validate_location(location, || self.check_before(deadline))?;
                    let out =
                        copy_location(self.snapshot.catalog.memory_budget(), location, || {
                            self.check_before(deadline)
                        })?;
                    return Ok(Some(ViewerBody::Owned(out)));
                }
                (b"\"common\"", None, Some(qualifier), Some(method)) => {
                    let budget = self.snapshot.catalog.memory_budget();
                    let qualifier = text(budget, qualifier)?;
                    let method = text(budget, method)?;
                    self.check_before(deadline)?;
                    let common = open.objects().iter().enumerate().find(|(_, row)| {
                        row.parent.is_none()
                            && row.object.kind == MdKind::CommonModule
                            && eq_path(&row.object.name, qualifier.as_str())
                    });
                    let Some((ordinal, common)) = common else {
                        continue;
                    };
                    let related = open.modules().iter().enumerate().find(|(_, row)| {
                        row.object == Some(ordinal)
                            && row.location == ModuleLocation::Object
                            && row.slot.kind == ModuleKind::Module
                    });
                    let Some((ordinal, _)) = related else {
                        continue;
                    };
                    let related = open.module_key(ordinal).ok_or(AnalysisError::Protocol)?;
                    let mut path = BudgetedText::new(budget, Category::Metadata, u64::MAX)
                        .map_err(AnalysisError::from)?;
                    path.try_push_str("CommonModule.")
                        .map_err(AnalysisError::from)?;
                    path.try_push_str(&common.object.name)
                        .map_err(AnalysisError::from)?;
                    let body = self.run_before(
                        related,
                        "definition_method",
                        Method {
                            path: path.as_str(),
                            name: method.as_str(),
                            exported_only: true,
                        },
                        Some(deadline),
                    )?;
                    if body.as_ref() != b"null" {
                        validate_location(body.as_ref(), || self.check_before(deadline))?;
                        return Ok(Some(body));
                    }
                }
                _ => return Err(AnalysisError::Protocol.into()),
            }
        }
        Ok(None)
    }
    pub(super) fn check_before(&self, deadline: Instant) -> Result<(), AnalysisError> {
        self.check().map_err(|error| match error {
            SelectedAnalysisError::Native(error) => error,
            _ => AnalysisError::Failed,
        })?;
        if Instant::now() >= deadline {
            return Err(AnalysisError::Timeout);
        }
        Ok(())
    }
}

pub(super) struct Cursor<'a, F> {
    bytes: &'a [u8],
    at: usize,
    check: F,
}
impl<'a, F: Fn() -> Result<(), AnalysisError>> Cursor<'a, F> {
    pub(super) fn new(bytes: &'a [u8], check: F) -> Self {
        Self {
            bytes,
            at: 0,
            check,
        }
    }
    fn ws(&mut self) {
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }
    pub(super) fn punct(&mut self, byte: u8) -> Result<(), AnalysisError> {
        (self.check)()?;
        self.ws();
        if self.bytes.get(self.at) != Some(&byte) {
            return Err(AnalysisError::Protocol);
        }
        self.at += 1;
        Ok(())
    }
    pub(super) fn finish(&mut self) -> Result<(), AnalysisError> {
        (self.check)()?;
        self.ws();
        if self.at != self.bytes.len() {
            return Err(AnalysisError::Protocol);
        }
        Ok(())
    }
    pub(super) fn value(&mut self) -> Result<&'a [u8], AnalysisError> {
        (self.check)()?;
        self.ws();
        let start = self.at;
        let (mut depth, mut string, mut escaped) = (0usize, false, false);
        while let Some(&byte) = self.bytes.get(self.at) {
            if self.at.is_multiple_of(64 << 10) {
                (self.check)()?;
            }
            if string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    string = false;
                }
            } else {
                match byte {
                    b'"' => string = true,
                    b'[' | b'{' => depth += 1,
                    b']' | b'}' if depth != 0 => depth -= 1,
                    b',' | b']' | b'}' | b':' if depth == 0 => break,
                    _ => {}
                }
            }
            self.at += 1;
        }
        let value = self
            .bytes
            .get(start..self.at)
            .ok_or(AnalysisError::Protocol)?
            .trim_ascii();
        if value.is_empty() || string || depth != 0 {
            return Err(AnalysisError::Protocol);
        }
        Ok(value)
    }
    pub(super) fn optional_fields<const N: usize>(
        &mut self,
        keys: [&[u8]; N],
    ) -> Result<[Option<&'a [u8]>; N], AnalysisError> {
        self.punct(b'{')?;
        let mut out = [None; N];
        self.ws();
        if self.bytes.get(self.at) == Some(&b'}') {
            self.at += 1;
            return Ok(out);
        }
        loop {
            let key = self.value()?;
            let key = key
                .strip_prefix(b"\"")
                .and_then(|s| s.strip_suffix(b"\""))
                .ok_or(AnalysisError::Protocol)?;
            let at = keys
                .iter()
                .position(|k| *k == key)
                .ok_or(AnalysisError::Protocol)?;
            let slot = out.get_mut(at).ok_or(AnalysisError::Protocol)?;
            if slot.is_some() {
                return Err(AnalysisError::Protocol);
            }
            self.punct(b':')?;
            *slot = Some(self.value()?);
            self.ws();
            if self.bytes.get(self.at) == Some(&b'}') {
                self.at += 1;
                break;
            }
            self.punct(b',')?;
        }
        Ok(out)
    }
    pub(super) fn fields<const N: usize>(
        &mut self,
        keys: [&[u8]; N],
    ) -> Result<[&'a [u8]; N], AnalysisError> {
        let values = self.optional_fields(keys)?;
        let mut out = [&b""[..]; N];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value.ok_or(AnalysisError::Protocol)?;
        }
        Ok(out)
    }
}
fn validate_location(
    bytes: &[u8],
    check: impl Fn() -> Result<(), AnalysisError>,
) -> Result<(), AnalysisError> {
    let mut cursor = Cursor::new(bytes, check);
    let [path, module, line, name] =
        cursor.fields([b"path".as_slice(), b"module", b"line", b"name"])?;
    cursor.finish()?;
    for s in [path, module, name] {
        if !s.starts_with(b"\"") || !s.ends_with(b"\"") {
            return Err(AnalysisError::Protocol);
        }
    }
    unsigned(line, u64::from(u32::MAX))?;
    Ok(())
}
pub(super) fn unsigned(bytes: &[u8], maximum: u64) -> Result<u64, AnalysisError> {
    let digits = maximum.checked_ilog10().unwrap_or(0) + 1;
    if bytes.is_empty()
        || bytes.len() > usize::try_from(digits).map_err(|_| AnalysisError::Protocol)?
        || (bytes.len() > 1 && bytes.first() == Some(&b'0'))
    {
        return Err(AnalysisError::Protocol);
    }
    let mut number = 0u64;
    for &byte in bytes {
        if !byte.is_ascii_digit() {
            return Err(AnalysisError::Protocol);
        }
        number = number
            .checked_mul(10)
            .and_then(|n| n.checked_add(u64::from(byte - b'0')))
            .filter(|&n| n <= maximum)
            .ok_or(AnalysisError::Protocol)?;
    }
    Ok(number)
}
fn copy_location(
    budget: &MemoryBudget,
    bytes: &[u8],
    check: impl Fn() -> Result<(), AnalysisError>,
) -> Result<BudgetedVec<u8>, AnalysisError> {
    check()?;
    let mut out = BudgetedVec::try_with_capacity(budget, Category::Response, bytes.len())?;
    for chunk in bytes.chunks(64 << 10) {
        check()?;
        out.try_extend_from_slice(chunk)?;
    }
    check()?;
    Ok(out)
}
fn text(budget: &MemoryBudget, bytes: &[u8]) -> Result<BudgetedText, AnalysisError> {
    let scratch = (bytes.len() as u64)
        .checked_mul(4)
        .and_then(|n| n.checked_add(8192))
        .ok_or(BudgetError::Overflow)?;
    let _scratch = budget
        .try_reserve(Category::Execution, scratch)
        .map_err(BudgetError::from)?;
    struct Seed<'a> {
        budget: &'a MemoryBudget,
        error: &'a mut Option<BudgetError>,
    }
    impl<'de> DeserializeSeed<'de> for Seed<'_> {
        type Value = BudgetedText;
        fn deserialize<D: serde::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
            de.deserialize_str(self)
        }
    }
    impl Visitor<'_> for Seed<'_> {
        type Value = BudgetedText;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a lexical identifier")
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
            let result = BudgetedText::try_with_capacity(
                self.budget,
                Category::Metadata,
                value.len() as u64,
                value.len(),
            )
            .and_then(|mut out| {
                out.try_push_str(value)?;
                Ok(out)
            });
            result.map_err(|error| {
                *self.error = Some(error);
                E::custom("admission refused")
            })
        }
    }
    let mut error = None;
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let value = Seed {
        budget,
        error: &mut error,
    }
    .deserialize(&mut de);
    if let Some(error) = error {
        return Err(error.into());
    }
    let value = value.map_err(|_| AnalysisError::Protocol)?;
    de.end().map_err(|_| AnalysisError::Protocol)?;
    Ok(value)
}
