use super::re::{self, FindRegex, RegexError};
use crate::viewer::{
    Lang,
    body::ViewerBody,
    names::{child_tag_en, module_label, sub_kind_en},
    snapshot::ManagedSnapshot,
};
use mdconf::{
    ChildTag, LocalString, ManagedEntrySource,
    startup::{ChildFacts, ManagedSelectionError, ManagedViewerOpen, ModuleLocation},
};
use memory_budget::{
    BudgetError, BudgetedText, BudgetedVec, BudgetedWriter, Category, MemoryBudget, Reservation,
};
use serde::Serialize;
use std::{
    fmt,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

#[derive(Debug, thiserror::Error)]
pub enum ManagedFindError<E> {
    #[error("{0}")]
    Memory(#[from] BudgetError),
    #[error("{0}")]
    Selection(#[from] ManagedSelectionError<E>),
    #[error("canceled")]
    Canceled,
    #[error("validated module inventory is inconsistent")]
    InvalidInventory,
    #[error("{0}")]
    Pattern(PatternError),
}
type Result<T, E> = std::result::Result<T, ManagedFindError<E>>;

#[derive(Debug)]
pub struct PatternError(BudgetedText);

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

pub struct FindQuery<'a> {
    pub pattern: &'a str,
    pub regex: bool,
    pub names_only: bool,
    pub limit: usize,
    pub wire_limit: u64,
    pub canceled: &'a AtomicBool,
    pub deadline: Option<Instant>,
}

impl<S: ManagedEntrySource> ManagedSnapshot<S> {
    pub fn find_body(&self, query: &FindQuery<'_>) -> Result<ViewerBody, S::Error> {
        self.find_body_with_control(query, &|| false)
    }

    pub fn find_body_with_control(
        &self,
        query: &FindQuery<'_>,
        interrupted: &dyn Fn() -> bool,
    ) -> Result<ViewerBody, S::Error> {
        let budget = self.catalog().memory_budget();
        let canceled = Cancel {
            flag: query.canceled,
            interrupted,
            deadline: query.deadline,
        };
        canceled.check()?;
        let mut finder = Finder {
            open: self.open(),
            budget,
            canceled,
            matcher: matcher(budget, query.pattern, query.regex)?,
            scratch: BudgetedVec::new(budget, Category::Metadata)?,
            names_only: query.names_only,
            limit: query.limit,
            total: 0,
            out: BudgetedWriter::new(budget, query.wire_limit)?,
        };
        finder.check()?;
        finder.write(b"{\"hits\":[")?;
        finder.run()?;
        finder.write(b"],\"total\":")?;
        let total = finder.total;
        finder.serialize(&total)?;
        finder.write(b"}")?;
        Ok(ViewerBody::Owned(finder.out.into_buffer()))
    }
}

#[derive(Serialize)]
struct Hit<'a> {
    path: &'a str,
    module: Option<&'a str>,
    line: Option<u32>,
    text: &'a str,
}

struct Cancel<'a> {
    flag: &'a AtomicBool,
    interrupted: &'a dyn Fn() -> bool,
    deadline: Option<Instant>,
}

impl Cancel<'_> {
    fn check<E>(&self) -> Result<(), E> {
        if self.flag.load(Ordering::Acquire)
            || (self.interrupted)()
            || self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(ManagedFindError::Canceled);
        }
        Ok(())
    }
}

enum Matcher {
    Sub(BudgetedText),
    Re {
        regex: Box<FindRegex>,
        _allowance: Reservation,
    },
}

fn matcher<E>(budget: &MemoryBudget, pattern: &str, regex: bool) -> Result<Matcher, E> {
    if !regex {
        return Ok(Matcher::Sub(fold_into(text(budget)?, pattern)?));
    }
    if let Err(error) = re::check_len(pattern) {
        return Err(refusal(budget, &error)?);
    }
    let allowance = budget
        .try_reserve(Category::Execution, re::ALLOWANCE)
        .map_err(BudgetError::from)?;
    match FindRegex::compile(pattern) {
        Ok(compiled) => Ok(Matcher::Re {
            regex: Box::new(compiled),
            _allowance: allowance,
        }),
        Err(error) => {
            let refused = refusal(budget, &error);
            drop(error);
            drop(allowance);
            Err(refused?)
        }
    }
}

fn refusal<E>(
    budget: &MemoryBudget,
    error: &RegexError,
) -> std::result::Result<ManagedFindError<E>, BudgetError> {
    struct Sink {
        text: BudgetedText,
        error: Option<BudgetError>,
    }
    impl fmt::Write for Sink {
        fn write_str(&mut self, value: &str) -> fmt::Result {
            self.text.try_push_str(value).map_err(|error| {
                self.error = Some(error);
                fmt::Error
            })
        }
    }
    let mut sink = Sink {
        text: text(budget)?,
        error: None,
    };
    let written = fmt::Write::write_fmt(&mut sink, format_args!("{error}"));
    if let Some(error) = sink.error {
        return Err(error);
    }
    written.map_err(|_| BudgetError::Overflow)?;
    Ok(ManagedFindError::Pattern(PatternError(sink.text)))
}

struct Finder<'a, S: ManagedEntrySource> {
    open: &'a ManagedViewerOpen<S>,
    budget: &'a MemoryBudget,
    canceled: Cancel<'a>,
    matcher: Matcher,
    scratch: BudgetedVec<u8>,
    names_only: bool,
    limit: usize,
    total: usize,
    out: BudgetedWriter,
}

fn text(budget: &MemoryBudget) -> std::result::Result<BudgetedText, BudgetError> {
    BudgetedText::new(budget, Category::Metadata, u64::MAX)
}

fn fold_char(c: char) -> char {
    let mut lower = c.to_lowercase();
    let c = match (lower.next(), lower.next()) {
        (Some(x), None) => x,
        _ => c,
    };
    if c == 'ё' { 'е' } else { c }
}

fn fold_into(mut out: BudgetedText, value: &str) -> std::result::Result<BudgetedText, BudgetError> {
    for c in value.chars() {
        out.try_push(fold_char(c))?;
    }
    Ok(out)
}

struct Lines<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Iterator for Lines<'a> {
    type Item = &'a str;
    fn next(&mut self) -> Option<&'a str> {
        let rest = self.text.get(self.at..)?;
        if rest.is_empty() {
            return None;
        }
        match rest.find(['\n', '\r']) {
            Some(end) => {
                let line = rest.get(..end)?;
                let skip = if rest.get(end..end + 2) == Some("\r\n") {
                    2
                } else {
                    1
                };
                self.at += end + skip;
                Some(line)
            }
            None => {
                self.at = self.text.len();
                Some(rest)
            }
        }
    }
}

fn lines(text: &str) -> Lines<'_> {
    Lines { text, at: 0 }
}

enum Source<'a> {
    Borrowed(&'a str),
    Owned(BudgetedText),
}

impl Source<'_> {
    fn as_str(&self) -> &str {
        match self {
            Self::Borrowed(text) => text,
            Self::Owned(text) => text.as_str(),
        }
    }
}

fn decode<'a>(
    budget: &MemoryBudget,
    bytes: &'a [u8],
) -> std::result::Result<Source<'a>, BudgetError> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(Source::Borrowed(text));
    }
    let mut out = text(budget)?;
    for chunk in bytes.utf8_chunks() {
        out.try_push_str(chunk.valid())?;
        if !chunk.invalid().is_empty() {
            out.try_push(char::REPLACEMENT_CHARACTER)?;
        }
    }
    Ok(Source::Owned(out))
}

impl<S: ManagedEntrySource> Finder<'_, S> {
    fn check(&self) -> Result<(), S::Error> {
        self.canceled.check()
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), S::Error> {
        Ok(self.out.try_write(bytes)?)
    }

    fn is_match(&mut self, value: &str) -> Result<bool, S::Error> {
        let pattern = match &mut self.matcher {
            Matcher::Sub(pattern) => pattern,
            Matcher::Re { regex, .. } => return Ok(regex.is_match(value)),
        };
        self.scratch.clear();
        for c in value.chars() {
            let mut units = [0; 4];
            self.scratch
                .try_extend_from_slice(fold_char(c).encode_utf8(&mut units).as_bytes())?;
        }
        let folded = std::str::from_utf8(self.scratch.as_slice())
            .map_err(|_| ManagedFindError::InvalidInventory)?;
        Ok(folded.contains(pattern.as_str()))
    }

    fn serialize<T: Serialize>(&mut self, value: &T) -> Result<(), S::Error> {
        let result = serde_json::to_writer(self.out.serialization_sink(), value);
        if let Some(error) = self.out.error() {
            return Err((*error).into());
        }
        result.map_err(|_| BudgetError::Overflow)?;
        Ok(())
    }

    fn add(&mut self, hit: &Hit<'_>) -> Result<(), S::Error> {
        if self.total < self.limit {
            if self.total != 0 {
                self.write(b",")?;
            }
            self.serialize(hit)?;
        }
        self.total = self.total.checked_add(1).ok_or(BudgetError::Overflow)?;
        Ok(())
    }

    fn name(&mut self, path: &str, name: &str, synonym: &LocalString) -> Result<(), S::Error> {
        let mut first = None;
        let mut found = self.is_match(name)?;
        for (_, value) in &synonym.0 {
            if value.is_empty() {
                continue;
            }
            first.get_or_insert(value.as_str());
            if !found && self.is_match(value)? {
                found = true;
            }
        }
        if !found {
            return Ok(());
        }
        let mut label = text(self.budget)?;
        label.try_push_str(name)?;
        if let Some(first) = first {
            label.try_push_str(" (")?;
            label.try_push_str(first)?;
            label.try_push_str(")")?;
        }
        self.add(&Hit {
            path,
            module: None,
            line: None,
            text: label.as_str(),
        })
    }

    fn modules(
        &mut self,
        path: &str,
        object: Option<usize>,
        location: ModuleLocation,
    ) -> Result<(), S::Error> {
        if self.names_only {
            return Ok(());
        }
        let open = self.open;
        let rows = open.modules();
        let start = rows.partition_point(|row| match (row.object, object) {
            (Some(at), Some(object)) => at < object,
            (Some(_), None) => true,
            (None, _) => false,
        });
        let end = rows.partition_point(|row| match (row.object, object) {
            (Some(at), Some(object)) => at <= object,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => true,
        });
        let group = rows.get(start..end).unwrap_or_default();
        for (offset, row) in group.iter().enumerate() {
            if row.object != object || row.location != location {
                continue;
            }
            let ordinal = start + offset;
            self.check()?;
            let key = open
                .module_key(ordinal)
                .ok_or(ManagedFindError::InvalidInventory)?;
            let read = open.read_selected_module(key)?;
            self.check()?;
            let module = read
                .module
                .as_ref()
                .ok_or(ManagedSelectionError::MissingValidated)?;
            let source = decode(self.budget, &module.source)?;
            let label = module_label(row.slot.kind, Lang::En);
            for (at, line) in lines(source.as_str()).enumerate() {
                self.check()?;
                #[cfg(test)]
                tests::probe(tests::Event::Line, self.canceled.flag);
                if self.is_match(line)? {
                    self.add(&Hit {
                        path,
                        module: Some(label),
                        line: at.checked_add(1).and_then(|n| u32::try_from(n).ok()),
                        text: line.trim(),
                    })?;
                }
            }
        }
        Ok(())
    }

    fn joined(&self, owner: &str, tag: &str, name: &str) -> Result<BudgetedText, S::Error> {
        let mut path = text(self.budget)?;
        path.try_push_str(owner)?;
        path.try_push_str(".")?;
        path.try_push_str(tag)?;
        path.try_push_str(".")?;
        path.try_push_str(name)?;
        Ok(path)
    }

    fn child(
        &mut self,
        owner: &str,
        object: usize,
        command: Option<usize>,
        child: &ChildFacts,
    ) -> Result<(), S::Error> {
        let path = self.joined(owner, child_tag_en(child.tag), &child.name)?;
        self.name(path.as_str(), &child.name, &child.synonym)?;
        if let Some(at) = command {
            self.modules(path.as_str(), Some(object), ModuleLocation::Command(at))?;
        }
        for nested in child.children.iter() {
            self.child(path.as_str(), object, None, nested)?;
        }
        Ok(())
    }

    fn object(&mut self, path: &str, ordinal: usize) -> Result<(), S::Error> {
        let open = self.open;
        let object = &open
            .objects()
            .get(ordinal)
            .ok_or(ManagedFindError::InvalidInventory)?
            .object;
        self.name(path, &object.name, &object.synonym)?;
        self.modules(path, Some(ordinal), ModuleLocation::Object)?;
        for (at, child) in object.children.iter().enumerate() {
            let command = (child.tag == ChildTag::Command).then_some(at);
            self.child(path, ordinal, command, child)?;
        }
        let empty = LocalString::default();
        for (at, form) in object.forms.iter().enumerate() {
            let sub = self.joined(path, sub_kind_en(form.kind), &form.name)?;
            self.name(sub.as_str(), &form.name, &empty)?;
            self.modules(sub.as_str(), Some(ordinal), ModuleLocation::Form(at))?;
        }
        for template in object.templates.iter() {
            let sub = self.joined(path, sub_kind_en(template.kind), &template.name)?;
            self.name(sub.as_str(), &template.name, &empty)?;
        }
        for (at, recalc) in object.recalculations.iter().enumerate() {
            let sub = self.joined(path, "Recalculation", &recalc.name)?;
            self.name(sub.as_str(), &recalc.name, &recalc.synonym)?;
            self.modules(
                sub.as_str(),
                Some(ordinal),
                ModuleLocation::Recalculation(at),
            )?;
        }
        Ok(())
    }

    fn run(&mut self) -> Result<(), S::Error> {
        let mut previous = Some(0);
        for row in self.open.modules().iter() {
            match (previous, row.object) {
                (Some(last), Some(at)) if last <= at => previous = Some(at),
                (_, None) => previous = None,
                _ => return Err(ManagedFindError::InvalidInventory),
            }
        }
        self.modules("Configuration", None, ModuleLocation::Configuration)?;
        let open = self.open;
        let mut stack: BudgetedVec<(usize, BudgetedText)> =
            BudgetedVec::new(self.budget, Category::Metadata)?;
        for (ordinal, row) in open.objects().iter().enumerate() {
            self.check()?;
            while let Some((top, _)) = stack.last() {
                if Some(*top) == row.parent {
                    break;
                }
                stack.pop();
            }
            let path = match (row.parent, stack.last()) {
                (None, _) => {
                    let mut path = text(self.budget)?;
                    path.try_push_str(row.object.kind.as_str())?;
                    path.try_push_str(".")?;
                    path.try_push_str(&row.object.name)?;
                    path
                }
                (Some(_), Some((_, owner))) => {
                    self.joined(owner.as_str(), "Subsystem", &row.object.name)?
                }
                (Some(_), None) => return Err(ManagedFindError::InvalidInventory),
            };
            self.object(path.as_str(), ordinal)?;
            #[cfg(test)]
            tests::probe(tests::Event::Object, self.canceled.flag);
            stack.try_push((ordinal, path))?;
        }
        Ok(())
    }
}
