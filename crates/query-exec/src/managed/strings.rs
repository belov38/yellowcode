use super::{
    collation::{CollationError, ManagedCollation},
    map::ManagedMap,
};
use bsl_prim::Strength;
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, MemoryBudget};
use query_ir::{
    QType,
    managed_value::{ManagedValue, NumberValueError, ScalarValue, ValueRef, row::ScopedRow},
};
use query_syntax::Func;
use std::{
    borrow::Borrow,
    collections::hash_map::RandomState,
    fmt::{self, Write},
    hash::BuildHasher,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringEvalError {
    Memory(BudgetError),
    Icu(icu46_sys::IcuStatus),
    Argument(&'static str),
}
impl From<BudgetError> for StringEvalError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl From<CollationError> for StringEvalError {
    fn from(error: CollationError) -> Self {
        match error {
            CollationError::Budget(e) => Self::Memory(e),
            CollationError::Icu(e) => Self::Icu(e),
        }
    }
}
impl From<NumberValueError> for StringEvalError {
    fn from(error: NumberValueError) -> Self {
        match error {
            NumberValueError::Budget(e) => Self::Memory(e),
            NumberValueError::DivByZero => Self::Argument("invalid string length"),
        }
    }
}
impl fmt::Display for StringEvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(e) => e.fmt(f),
            Self::Icu(e) => e.fmt(f),
            Self::Argument(e) => f.write_str(e),
        }
    }
}
impl std::error::Error for StringEvalError {}
fn arg(values: &[ManagedValue], index: usize) -> Result<ValueRef<'_>, StringEvalError> {
    values
        .get(index)
        .map(ManagedValue::view)
        .ok_or(StringEvalError::Argument("too few arguments"))
}
fn text(value: ValueRef<'_>) -> Result<&[u16], StringEvalError> {
    match value {
        ValueRef::String(s) => Ok(s),
        _ => Err(StringEvalError::Argument("a string argument is expected")),
    }
}
fn integer(value: ValueRef<'_>) -> Result<i64, StringEvalError> {
    match value {
        ValueRef::Number(n) => Ok(n.trunc_to_i64_saturating()),
        _ => Err(StringEvalError::Argument("a number argument is expected")),
    }
}
fn length(value: usize, budget: &MemoryBudget) -> Result<ManagedValue, StringEvalError> {
    let mut n = u64::try_from(value).map_err(|_| BudgetError::Overflow)?;
    let mut digits = [0; 20];
    let mut start = digits.len();
    loop {
        start = start.checked_sub(1).ok_or(BudgetError::Overflow)?;
        *digits.get_mut(start).ok_or(BudgetError::Overflow)? = (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    ManagedValue::try_number_from_decimal_digits(
        false,
        digits.get(start..).ok_or(BudgetError::Overflow)?,
        0,
        budget,
    )?
    .ok_or(StringEvalError::Memory(BudgetError::Overflow))
}
fn substring(text: &[u16], start: i64, len: i64) -> &[u16] {
    if len <= 0 {
        return &[];
    }
    let start = usize::try_from(start.max(1) - 1)
        .unwrap_or(usize::MAX)
        .min(text.len());
    let end = start
        .saturating_add(usize::try_from(len).unwrap_or(usize::MAX))
        .min(text.len());
    text.get(start..end).unwrap_or(&[])
}
fn points(text: &[u16]) -> impl Iterator<Item = u32> + '_ {
    char::decode_utf16(text.iter().copied()).map(|v| {
        v.map(u32::from)
            .unwrap_or_else(|e| u32::from(e.unpaired_surrogate()))
    })
}
fn units(cp: u32, buffer: &mut [u16; 2]) -> &[u16] {
    match char::from_u32(cp) {
        Some(c) => c.encode_utf16(buffer),
        None => {
            buffer[0] = cp as u16;
            &buffer[..1]
        }
    }
}
struct Output {
    units: BudgetedVec<u16>,
    error: Option<BudgetError>,
}
impl Output {
    fn new(budget: &MemoryBudget, ty: &QType) -> Result<Self, BudgetError> {
        let capacity = ty.string.filter(|q| q.fixed).map_or(Ok(0), |q| {
            usize::try_from(q.len).map_err(|_| BudgetError::Overflow)
        })?;
        Ok(Self {
            units: BudgetedVec::try_with_capacity(budget, Category::Execution, capacity)?,
            error: None,
        })
    }
    fn extend(&mut self, text: &[u16]) -> Result<(), BudgetError> {
        self.units.try_extend_from_slice(text)
    }
    fn finish(
        mut self,
        ty: &QType,
        budget: &MemoryBudget,
    ) -> Result<ManagedValue, StringEvalError> {
        if let Some(error) = self.error {
            return Err(error.into());
        }
        if let Some(q) = ty.string
            && q.fixed
        {
            let len = usize::try_from(q.len).map_err(|_| BudgetError::Overflow)?;
            if self.units.len() < len {
                self.units.try_resize(len, 0x20)?;
            }
        }
        Ok(ManagedValue::try_utf16(&self.units, budget)?)
    }
    fn formatted(&self, result: fmt::Result) -> Result<(), StringEvalError> {
        if let Some(error) = self.error {
            return Err(error.into());
        }
        result.map_err(|_| StringEvalError::Memory(BudgetError::Overflow))
    }
}
impl Write for Output {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.error.is_some() {
            return Err(fmt::Error);
        }
        for unit in text.encode_utf16() {
            if let Err(error) = self.units.try_push(unit) {
                self.error = Some(error);
                return Err(fmt::Error);
            }
        }
        Ok(())
    }
}
fn present(
    value: ValueRef<'_>,
    out: &mut Output,
    budget: &MemoryBudget,
) -> Result<(), StringEvalError> {
    match value {
        ValueRef::String(s) => out.extend(s)?,
        ValueRef::Bool(value) => {
            let result = out.write_str(if value { "Да" } else { "Нет" });
            out.formatted(result)?;
        }
        ValueRef::Date(date) => {
            let p = date.parts();
            let result = write!(
                out,
                "{:02}.{:02}.{:04} {}:{:02}:{:02}",
                p.day, p.month, p.year, p.hour, p.minute, p.second
            );
            out.formatted(result)?;
        }
        ValueRef::Number(number) => {
            let cap = budget.scope_snapshot().ok_or(BudgetError::Overflow)?.limit;
            let mut xml = BudgetedText::new(budget, Category::Execution, cap)?;
            let result = number.write_xml(&mut xml);
            if let Some(error) = xml.error() {
                return Err((*error).into());
            }
            result.map_err(|_| BudgetError::Overflow)?;
            let xml = xml.as_str();
            let (negative, xml) = xml.strip_prefix('-').map_or((false, xml), |v| (true, v));
            let (integer, frac) = xml.split_once('.').unwrap_or((xml, ""));
            if negative {
                out.extend(&[u16::from(b'-')])?;
            }
            for (i, byte) in integer.bytes().enumerate() {
                if i > 0 && (integer.len() - i).is_multiple_of(3) {
                    out.extend(&[0xa0])?;
                }
                out.extend(&[u16::from(byte)])?;
            }
            if !frac.is_empty() {
                out.extend(&[u16::from(b',')])?;
                for byte in frac.bytes() {
                    out.extend(&[u16::from(byte)])?;
                }
            }
        }
        _ => {
            return Err(StringEvalError::Argument(
                "СТРОКА of this value is not supported",
            ));
        }
    }
    Ok(())
}
#[derive(Debug)]
struct Key(BudgetedVec<u8>);
impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_slice() == other.0.as_slice()
    }
}
impl Eq for Key {}
impl Borrow<[u8]> for Key {
    fn borrow(&self) -> &[u8] {
        self.0.as_slice()
    }
}
struct Classes<'a> {
    coll: &'a ManagedCollation,
    budget: &'a MemoryBudget,
    hashes: RandomState,
    cp: ManagedMap<u32, u32>,
    key: ManagedMap<Key, u32>,
    _charge: memory_budget::Reservation,
}
impl<'a> Classes<'a> {
    fn new(coll: &'a ManagedCollation, budget: &'a MemoryBudget) -> Result<Self, StringEvalError> {
        coll.check_budget(budget)?;
        let charge = budget
            .try_reserve(Category::Execution, std::mem::size_of::<Self>() as u64)
            .map_err(BudgetError::from)?;
        Ok(Self {
            coll,
            budget,
            hashes: RandomState::new(),
            cp: ManagedMap::new(budget)?,
            key: ManagedMap::new(budget)?,
            _charge: charge,
        })
    }
    fn class(&mut self, cp: u32) -> Result<u32, StringEvalError> {
        let hash = self.hashes.hash_one(cp);
        if let Some(id) = self.cp.get(hash, &cp) {
            return Ok(*id);
        }
        let mut buffer = [0; 2];
        let key =
            self.coll
                .try_sort_key(Strength::Secondary, units(cp, &mut buffer), self.budget)?;
        let key_hash = self.hashes.hash_one(key.as_slice());
        let id = if let Some(id) = self.key.get(key_hash, key.as_slice()) {
            *id
        } else {
            let id = u32::try_from(self.key.len()).map_err(|_| BudgetError::Overflow)?;
            self.key
                .try_insert(key_hash, Key(key), id)
                .map_err(|e| e.error)?;
            id
        };
        self.cp.try_insert(hash, cp, id).map_err(|e| e.error)?;
        Ok(id)
    }
    fn text(
        &mut self,
        text: &[u16],
    ) -> Result<(BudgetedVec<u32>, BudgetedVec<usize>), StringEvalError> {
        let mut ids = BudgetedVec::new(self.budget, Category::Execution)?;
        let mut offsets = BudgetedVec::new(self.budget, Category::Execution)?;
        let mut offset = 0usize;
        for cp in points(text) {
            offsets.try_push(offset)?;
            ids.try_push(self.class(cp)?)?;
            offset = offset
                .checked_add(if cp > 0xffff { 2 } else { 1 })
                .ok_or(BudgetError::Overflow)?;
        }
        offsets.try_push(offset)?;
        Ok((ids, offsets))
    }
}
fn occurrences(
    hay: &[u32],
    needle: &[u32],
    budget: &MemoryBudget,
) -> Result<BudgetedVec<usize>, BudgetError> {
    let mut matches = BudgetedVec::new(budget, Category::Execution)?;
    if needle.is_empty() || hay.len() < needle.len() {
        return Ok(matches);
    }
    let mut fail = BudgetedVec::try_with_capacity(budget, Category::Execution, needle.len())?;
    fail.try_resize(needle.len(), 0usize)?;
    let mut k = 0;
    for i in 1..needle.len() {
        while k > 0 && needle.get(i) != needle.get(k) {
            k = fail.get(k - 1).copied().unwrap_or(0);
        }
        if needle.get(i) == needle.get(k) {
            k += 1;
        }
        *fail.get_mut(i).ok_or(BudgetError::Overflow)? = k;
    }
    k = 0;
    for (i, h) in hay.iter().enumerate() {
        while k > 0 && needle.get(k) != Some(h) {
            k = fail.get(k - 1).copied().unwrap_or(0);
        }
        if needle.get(k) == Some(h) {
            k += 1;
        }
        if k == needle.len() {
            matches.try_push(i + 1 - k)?;
            k = 0;
        }
    }
    Ok(matches)
}

pub fn try_call(
    f: Func,
    args: &ScopedRow,
    ty: &QType,
    coll: &ManagedCollation,
) -> Result<ManagedValue, StringEvalError> {
    let budget = args.budget();
    coll.check_budget(budget)?;
    let values = args.values();
    if f == Func::IsNull {
        let [a, b] = values else {
            return Err(StringEvalError::Argument("ЕСТЬNULL takes two arguments"));
        };
        return Ok(if matches!(a.view(), ValueRef::Null) {
            b.clone()
        } else {
            a.clone()
        });
    }
    if values.iter().any(|a| matches!(a.view(), ValueRef::Null)) {
        return Ok(ManagedValue::scalar(ScalarValue::Null));
    }
    if f == Func::StringLength {
        return length(text(arg(values, 0)?)?.len(), budget);
    }
    let mut out = Output::new(budget, ty)?;
    match f {
        Func::String => present(arg(values, 0)?, &mut out, budget)?,
        Func::TrimL | Func::TrimR | Func::TrimAll => {
            let mut s = text(arg(values, 0)?)?;
            if f != Func::TrimR {
                s = s
                    .get(s.iter().take_while(|&&u| u == 0x20).count()..)
                    .unwrap_or(&[]);
            }
            if f != Func::TrimL {
                s = s
                    .get(..s.iter().rposition(|&u| u != 0x20).map_or(0, |i| i + 1))
                    .unwrap_or(&[]);
            }
            out.extend(s)?;
        }
        Func::Upper | Func::Lower => {
            for cp in points(text(arg(values, 0)?)?) {
                let cp = if let Some(ch) = char::from_u32(cp) {
                    if f == Func::Lower && ch == '\u{130}' {
                        u32::from('i')
                    } else if f == Func::Upper {
                        let mut it = ch.to_uppercase();
                        match (it.next(), it.next()) {
                            (Some(m), None) => u32::from(m),
                            _ => cp,
                        }
                    } else {
                        let mut it = ch.to_lowercase();
                        match (it.next(), it.next()) {
                            (Some(m), None) => u32::from(m),
                            _ => cp,
                        }
                    }
                } else {
                    cp
                };
                out.extend(units(cp, &mut [0; 2]))?;
            }
        }
        Func::Substring | Func::Left | Func::Right => {
            let t = text(arg(values, 0)?)?;
            let n = integer(arg(values, 1)?)?;
            let selected = match f {
                Func::Substring => substring(t, n, integer(arg(values, 2)?)?),
                Func::Left => substring(t, 1, n),
                _ if n <= 0 => &[],
                _ => t
                    .get(
                        t.len()
                            .saturating_sub(usize::try_from(n).unwrap_or(usize::MAX))..,
                    )
                    .unwrap_or(&[]),
            };
            out.extend(selected)?;
        }
        Func::StrFind | Func::StrReplace => {
            let hay = text(arg(values, 0)?)?;
            let needle = text(arg(values, 1)?)?;
            if needle.is_empty() {
                if f == Func::StrFind {
                    return length(1, budget);
                }
                text(arg(values, 2)?)?;
                out.extend(hay)?;
            } else {
                let mut classes = Classes::new(coll, budget)?;
                let (h, offsets) = classes.text(hay)?;
                let (n, _needle_offsets) = classes.text(needle)?;
                let matches = occurrences(&h, &n, budget)?;
                if f == Func::StrFind {
                    let pos = matches
                        .first()
                        .and_then(|&i| offsets.get(i))
                        .map_or(0, |&i| i + 1);
                    return length(pos, budget);
                }
                let replacement = text(arg(values, 2)?)?;
                let mut done = 0;
                for &index in matches.iter() {
                    let begin = *offsets.get(index).ok_or(BudgetError::Overflow)?;
                    let end = *offsets.get(index + n.len()).ok_or(BudgetError::Overflow)?;
                    out.extend(hay.get(done..begin).ok_or(BudgetError::Overflow)?)?;
                    out.extend(replacement)?;
                    done = end;
                }
                out.extend(hay.get(done..).ok_or(BudgetError::Overflow)?)?;
            }
        }
        _ => return Err(StringEvalError::Argument("not a string function")),
    }
    out.finish(ty, budget)
}
