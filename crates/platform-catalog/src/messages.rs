use crate::model::DecodeError;

pub(crate) static MESSAGES_TSV: &str = include_str!("../data/messages.tsv");
pub(crate) static CATEGORIES_TSV: &str = include_str!("../data/categories.tsv");

fn embedded() -> &'static Messages {
    crate::resource::active().messages()
}

pub fn text(id: &str, lang: Lang) -> Option<&'static str> {
    embedded().text(id, lang)
}

pub fn render(id: &str, arg: Option<&str>, lang: Lang) -> Option<String> {
    embedded().render(id, arg, lang)
}

pub fn text_key(key: &str, lang: Lang) -> Option<&'static str> {
    embedded().text(bsl_id(key), lang)
}

pub fn loaded_text_key(key: &str, lang: Lang) -> Option<&'static str> {
    crate::resource::loaded_messages()?.text(bsl_id(key), lang)
}

pub fn render_key(key: &str, args: &[String], suffix: &str, lang: Lang) -> Option<String> {
    let mut text = fill(text_key(key, lang)?, args);
    text.push_str(suffix);
    Some(text)
}

pub fn fill(template: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut args = args.iter();
    let mut rest = template;
    while let Some(p) = rest.find('%') {
        out.push_str(&rest[..p]);
        let tail = &rest[p..];
        if (tail.starts_with("%d") || tail.starts_with("%s"))
            && let Some(a) = args.next()
        {
            out.push_str(a);
            rest = &tail[2..];
        } else {
            out.push('%');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

fn bsl_id(key: &str) -> &str {
    key.strip_prefix("bsl:").unwrap_or(key)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Wrapper {
    None,
    ContextCall,
    CtorCall,
    BuiltinArg,
    SetAttr,
    GetAttr,
    Compile,
}

impl Wrapper {
    pub const ALL: [Wrapper; 7] = [
        Wrapper::None,
        Wrapper::ContextCall,
        Wrapper::CtorCall,
        Wrapper::BuiltinArg,
        Wrapper::SetAttr,
        Wrapper::GetAttr,
        Wrapper::Compile,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Wrapper::None => "None",
            Wrapper::ContextCall => "ContextCall",
            Wrapper::CtorCall => "CtorCall",
            Wrapper::BuiltinArg => "BuiltinArg",
            Wrapper::SetAttr => "SetAttr",
            Wrapper::GetAttr => "GetAttr",
            Wrapper::Compile => "Compile",
        }
    }

    pub fn from_name(s: &str) -> Option<Wrapper> {
        Wrapper::ALL.into_iter().find(|w| w.name() == s)
    }

    pub fn key(self) -> Option<&'static str> {
        Some(match self {
            Wrapper::None => return None,
            Wrapper::ContextCall => "IDS_RTE_CONTEXTCALL",
            Wrapper::CtorCall => "IDS_RTE_CTORCALL",
            Wrapper::BuiltinArg => "IDS_RTE_BADARGVALUE",
            Wrapper::SetAttr => "IDS_RTE_CONTEXTSETPROPVAL",
            Wrapper::GetAttr => "IDS_RTE_CONTEXTGETPROPVAL",
            Wrapper::Compile => "IDS_RTE_COMPILE",
        })
    }
}

pub fn categories(wrapper: Wrapper, inner: &str) -> Option<u32> {
    embedded_categories().get(wrapper, bsl_id(inner))
}

fn embedded_categories() -> &'static CategoryTable {
    crate::resource::active().categories()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CategoryTable {
    map: Vec<(Wrapper, String, u32)>,
}

impl CategoryTable {
    pub fn parse_tsv(text: &str) -> Result<CategoryTable, DecodeError> {
        let mut map = Vec::new();
        map.try_reserve_exact(data_rows(text).count())
            .map_err(|_| DecodeError("allocation failed".into()))?;
        for (i, line) in text.lines().enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let bad = |what: &str| DecodeError(format!("line {}: {what}", i + 1));
            let mut parts = line.split('\t');
            let (Some(w), Some(id), Some(bits)) = (parts.next(), parts.next(), parts.next()) else {
                return Err(bad("expected wrapper, id and bits"));
            };
            let w = Wrapper::from_name(w).ok_or_else(|| bad("unknown wrapper"))?;
            if id.is_empty() {
                return Err(bad("empty id"));
            }
            let bits = bits
                .strip_prefix("0x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .ok_or_else(|| bad("bits are not 0x<hex>"))?;
            map.push((w, copy_text(id)?, bits));
        }
        map.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        if map
            .windows(2)
            .any(|pair| matches!(pair, [a,b] if a.0==b.0&&a.1==b.1))
        {
            return Err(DecodeError("repeated wrapper and id".into()));
        }
        Ok(CategoryTable { map })
    }

    pub fn get(&self, wrapper: Wrapper, id: &str) -> Option<u32> {
        let i = self
            .map
            .partition_point(|(w, key, _)| (*w, key.as_str()) < (wrapper, id));
        self.map
            .get(i)
            .filter(|(w, key, _)| *w == wrapper && key == id)
            .map(|(_, _, bits)| *bits)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn rows(&self) -> impl Iterator<Item = (Wrapper, &str, u32)> {
        self.map
            .iter()
            .map(|(w, id, bits)| (*w, id.as_str(), *bits))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    Ru,
    En,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Messages {
    map: Vec<(String, (String, String), usize)>,
}

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
}

fn data_rows(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}
fn copy_text(text: &str) -> Result<String, DecodeError> {
    let mut value = String::new();
    value
        .try_reserve_exact(text.len())
        .map_err(|_| DecodeError("allocation failed".into()))?;
    value.push_str(text);
    Ok(value)
}
fn unescape(s: &str, line: usize) -> Result<String, DecodeError> {
    let mut out = String::new();
    out.try_reserve_exact(s.len())
        .map_err(|_| DecodeError("allocation failed".into()))?;
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            _ => return Err(DecodeError(format!("line {line}: bad escape"))),
        }
    }
    Ok(out)
}

impl Messages {
    pub fn parse_tsv(text: &str) -> Result<Messages, DecodeError> {
        let mut map = Vec::new();
        map.try_reserve_exact(data_rows(text).count())
            .map_err(|_| DecodeError("allocation failed".into()))?;
        for (i, line) in text.lines().enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split('\t');
            let (Some(id), Some(ru), Some(en), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return Err(DecodeError(format!(
                    "line {}: expected 3 tab-separated fields",
                    i + 1
                )));
            };
            if id.is_empty() {
                return Err(DecodeError(format!("line {}: empty id", i + 1)));
            }
            map.push((
                copy_text(id)?,
                (unescape(ru, i + 1)?, unescape(en, i + 1)?),
                i,
            ));
        }
        Ok(Self::finish(map))
    }

    pub fn to_tsv(&self) -> String {
        let mut out = String::new();
        for (id, (ru, en), _) in &self.map {
            out.push_str(id);
            out.push('\t');
            escape(ru, &mut out);
            out.push('\t');
            escape(en, &mut out);
            out.push('\n');
        }
        out
    }

    pub fn from_rows(rows: Vec<(String, String, String)>) -> Messages {
        Self::finish(
            rows.into_iter()
                .enumerate()
                .map(|(i, (id, ru, en))| (id, (ru, en), i))
                .collect(),
        )
    }
    fn finish(mut map: Vec<(String, (String, String), usize)>) -> Self {
        map.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| b.2.cmp(&a.2)));
        map.dedup_by(|a, b| a.0 == b.0);
        for row in &mut map {
            row.2 = 0;
        }
        Self { map }
    }

    pub fn text(&self, id: &str, lang: Lang) -> Option<&str> {
        let i = self.map.partition_point(|(key, _, _)| key.as_str() < id);
        let (_, (ru, en), _) = self.map.get(i).filter(|(key, _, _)| key == id)?;
        Some(match lang {
            Lang::Ru => ru,
            Lang::En => en,
        })
    }

    pub fn render(&self, id: &str, arg: Option<&str>, lang: Lang) -> Option<String> {
        let t = self.text(id, lang)?;
        Some(match arg {
            Some(a) => format!("{t} ({a})"),
            None => t.to_string(),
        })
    }
}

const DIAGNOSTIC_BYTES: u64 = 256;
fn add(total: &mut u64, bytes: usize) -> Result<(), memory_budget::BudgetError> {
    *total = total
        .checked_add(bytes as u64)
        .ok_or(memory_budget::BudgetError::Overflow)?;
    Ok(())
}
fn table_bytes<T>(text: &str, category: bool) -> Result<u64, memory_budget::BudgetError> {
    let mut total = std::mem::size_of::<T>() as u64;
    for line in data_rows(text) {
        let mut fields = line.split('\t');
        if category {
            add(&mut total, std::mem::size_of::<(Wrapper, String, u32)>())?;
            let _ = fields.next();
            add(&mut total, fields.next().unwrap_or("").len())?;
        } else {
            add(
                &mut total,
                std::mem::size_of::<(String, (String, String), usize)>(),
            )?;
            for field in fields.take(3) {
                add(&mut total, field.len())?;
            }
        }
    }
    Ok(total)
}
pub(crate) fn managed_messages(
    text: &str,
    budget: &memory_budget::MemoryBudget,
) -> Result<crate::owned::Admitted<Messages>, crate::ManagedDecodeError> {
    let diagnostic =
        budget.try_reserve(memory_budget::Category::ReaderWorking, DIAGNOSTIC_BYTES)?;
    let charge = budget.try_reserve(
        memory_budget::Category::Metadata,
        table_bytes::<Messages>(text, false)?,
    )?;
    match Messages::parse_tsv(text) {
        Ok(value) => Ok(crate::owned::Admitted { value, charge }),
        Err(error) => Err(crate::ManagedDecodeError::data(error, diagnostic)),
    }
}
pub(crate) fn managed_categories(
    text: &str,
    budget: &memory_budget::MemoryBudget,
) -> Result<crate::owned::Admitted<CategoryTable>, crate::ManagedDecodeError> {
    let diagnostic =
        budget.try_reserve(memory_budget::Category::ReaderWorking, DIAGNOSTIC_BYTES)?;
    let charge = budget.try_reserve(
        memory_budget::Category::Metadata,
        table_bytes::<CategoryTable>(text, true)?,
    )?;
    match CategoryTable::parse_tsv(text) {
        Ok(value) => Ok(crate::owned::Admitted { value, charge }),
        Err(error) => Err(crate::ManagedDecodeError::data(error, diagnostic)),
    }
}
impl Messages {
    pub(crate) fn backing_bytes(&self) -> Result<u64, memory_budget::BudgetError> {
        let mut total = std::mem::size_of::<Self>() as u64;
        add(
            &mut total,
            self.map
                .capacity()
                .checked_mul(std::mem::size_of::<(String, (String, String), usize)>())
                .ok_or(memory_budget::BudgetError::Overflow)?,
        )?;
        for (id, (ru, en), _) in &self.map {
            add(&mut total, id.capacity())?;
            add(&mut total, ru.capacity())?;
            add(&mut total, en.capacity())?;
        }
        Ok(total)
    }
}
impl CategoryTable {
    pub(crate) fn backing_bytes(&self) -> Result<u64, memory_budget::BudgetError> {
        let mut total = std::mem::size_of::<Self>() as u64;
        add(
            &mut total,
            self.map
                .capacity()
                .checked_mul(std::mem::size_of::<(Wrapper, String, u32)>())
                .ok_or(memory_budget::BudgetError::Overflow)?,
        )?;
        for (_, id, _) in &self.map {
            add(&mut total, id.capacity())?;
        }
        Ok(total)
    }
}
