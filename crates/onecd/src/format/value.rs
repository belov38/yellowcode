use std::fmt;

use crate::format::bytes::{put_u16le, u16le};

pub type Row = Vec<Value>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Null,
    Numeric(Numeric),
    Char(String),
    VarChar(String),
    Binary(Vec<u8>),
    Text(BlobVal),
    NText(BlobVal),
    Image(BlobVal),
    Bool(bool),
    Date(RawDate),
    DateTime(DateTime),
    RowVersion([u32; 4]),
    Invalid(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobRef {
    pub start: u32,
    pub len: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobVal {
    Ref(BlobRef),
    Data(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawDate(pub [u8; 4]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    pub const EMPTY: DateTime = DateTime {
        year: 0,
        month: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
    };

    pub fn new(
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Option<DateTime> {
        let dt = DateTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        };
        if !dt.is_valid() {
            return None;
        }
        Some(if dt.is_empty_date() {
            DateTime::EMPTY
        } else {
            dt
        })
    }

    pub fn is_valid(&self) -> bool {
        *self == DateTime::EMPTY
            || ((1..=9999).contains(&self.year)
                && (1..=12).contains(&self.month)
                && (1..=days_in_month(self.year, self.month)).contains(&self.day)
                && self.hour < 24
                && self.minute < 60
                && self.second < 60)
    }

    fn is_empty_date(&self) -> bool {
        *self == DateTime::EMPTY
            || (
                self.year,
                self.month,
                self.day,
                self.hour,
                self.minute,
                self.second,
            ) == (1, 1, 1, 0, 0, 0)
    }

    pub fn from_bcd(b: &[u8; 7]) -> Option<DateTime> {
        if b.iter().all(|&x| x == 0) {
            return Some(DateTime::EMPTY);
        }
        let mut v = [0u8; 7];
        for (out, &x) in v.iter_mut().zip(b) {
            let (hi, lo) = (x >> 4, x & 0x0F);
            if hi > 9 || lo > 9 {
                return None;
            }
            *out = hi * 10 + lo;
        }
        let [century, year, month, day, hour, minute, second] = v;
        let dt = DateTime {
            year: u16::from(century) * 100 + u16::from(year),
            month,
            day,
            hour,
            minute,
            second,
        };
        dt.is_valid().then_some(dt)
    }

    pub fn to_bcd(&self) -> [u8; 7] {
        if self.is_empty_date() {
            return [0; 7];
        }
        let bcd = |v: u16| {
            let v = (v % 100) as u8;
            ((v / 10) << 4) | (v % 10)
        };
        [
            bcd(self.year / 100),
            bcd(self.year),
            bcd(u16::from(self.month)),
            bcd(u16::from(self.day)),
            bcd(u16::from(self.hour)),
            bcd(u16::from(self.minute)),
            bcd(u16::from(self.second)),
        ]
    }
}

fn days_in_month(year: u16, month: u8) -> u8 {
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Numeric {
    negative: bool,
    digits: Vec<u8>,
    scale: u32,
}

impl Numeric {
    pub fn parse(s: &str) -> Option<Numeric> {
        let (negative, body) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        let (int, frac) = body.split_once('.').unwrap_or((body, ""));
        if int.is_empty() && frac.is_empty() {
            return None;
        }
        let ascii_digits = |t: &str| -> Option<Vec<u8>> {
            t.bytes()
                .map(|c| c.is_ascii_digit().then(|| c - b'0'))
                .collect()
        };
        Numeric::from_parts(negative, &ascii_digits(int)?, &ascii_digits(frac)?)
    }

    pub fn from_i64(v: i64) -> Numeric {
        let mut magnitude = v.unsigned_abs();
        let mut digits = Vec::new();
        loop {
            digits.push((magnitude % 10) as u8);
            magnitude /= 10;
            if magnitude == 0 {
                break;
            }
        }
        digits.reverse();
        Numeric {
            negative: v < 0,
            digits,
            scale: 0,
        }
    }

    fn from_parts(negative: bool, int: &[u8], frac: &[u8]) -> Option<Numeric> {
        let scale = u32::try_from(frac.len()).ok()?;
        let int = trim_leading_zeros(int);
        let mut digits = Vec::with_capacity(int.len().max(1) + frac.len());
        if int.is_empty() {
            digits.push(0);
        } else {
            digits.extend_from_slice(int);
        }
        digits.extend_from_slice(frac);
        Some(Numeric {
            negative,
            digits,
            scale,
        })
    }

    fn split(&self) -> (&[u8], &[u8]) {
        let int_len = self.digits.len().saturating_sub(self.scale as usize);
        self.digits.split_at(int_len)
    }

    fn is_zero(&self) -> bool {
        self.digits.iter().all(|&d| d == 0)
    }

    pub fn is_negative(&self) -> bool {
        self.negative
    }

    pub fn scale(&self) -> u32 {
        self.scale
    }

    pub fn digits(&self) -> &[u8] {
        &self.digits
    }

    pub fn eq_value(&self, other: &Numeric) -> bool {
        let (a_zero, b_zero) = (self.is_zero(), other.is_zero());
        if a_zero || b_zero {
            return a_zero && b_zero;
        }
        let ((a_int, a_frac), (b_int, b_frac)) = (self.split(), other.split());
        self.negative == other.negative
            && trim_leading_zeros(a_int) == trim_leading_zeros(b_int)
            && trim_trailing_zeros(a_frac) == trim_trailing_zeros(b_frac)
    }

    pub(crate) fn from_bcd(b: &[u8], length: u32, precision: u32) -> Option<Numeric> {
        if precision > length || u64::from(length) + 1 > 2 * b.len() as u64 {
            return None;
        }
        let negative = nibble(b, 0)? == 0;
        let digits = (1..=length as usize)
            .map(|i| nibble(b, i).filter(|&d| d <= 9))
            .collect::<Option<Vec<u8>>>()?;
        let (int, frac) = digits.split_at(digits.len() - precision as usize);
        Numeric::from_parts(negative, int, frac)
    }

    pub(crate) fn bcd_capacity(
        b: &[u8],
        length: u32,
        precision: u32,
    ) -> Result<Option<usize>, memory_budget::BudgetError> {
        use memory_budget::BudgetError;
        if precision > length
            || u64::from(length) + 1 > 2 * b.len() as u64
            || nibble(b, 0).is_none()
        {
            return Ok(None);
        }
        let count = usize::try_from(length).map_err(|_| BudgetError::Overflow)?;
        let precision_usize = usize::try_from(precision).map_err(|_| BudgetError::Overflow)?;
        if !(1..=count).all(|i| nibble(b, i).is_some_and(|digit| digit <= 9)) {
            return Ok(None);
        }
        let integer = count - precision_usize;
        let first = (1..=integer).find(|&i| nibble(b, i) != Some(0));
        let integer_digits = first.map_or(1, |first| integer - first + 1);
        integer_digits
            .checked_add(precision_usize)
            .map(Some)
            .ok_or(BudgetError::Overflow)
    }

    pub(crate) fn from_bcd_into(
        b: &[u8],
        length: u32,
        precision: u32,
        mut digits: Vec<u8>,
    ) -> Result<Self, memory_budget::BudgetError> {
        use memory_budget::{BudgetError, extend_within};
        let count = usize::try_from(length).map_err(|_| BudgetError::Overflow)?;
        let precision_usize = usize::try_from(precision).map_err(|_| BudgetError::Overflow)?;
        let sign = nibble(b, 0).ok_or(BudgetError::Overflow)?;
        let integer = count
            .checked_sub(precision_usize)
            .ok_or(BudgetError::Overflow)?;
        let digit = |i: usize| nibble(b, i).ok_or(BudgetError::Overflow);
        match (1..=integer).find(|&i| nibble(b, i) != Some(0)) {
            Some(first) => {
                for i in first..=integer {
                    extend_within(&mut digits, &[digit(i)?])?;
                }
            }
            None => extend_within(&mut digits, &[0])?,
        }
        for i in integer + 1..=count {
            extend_within(&mut digits, &[digit(i)?])?;
        }
        Ok(Self {
            negative: sign == 0,
            digits,
            scale: precision,
        })
    }

    pub(crate) fn write_bcd(
        &self,
        length: u32,
        precision: u32,
        out: &mut [u8],
    ) -> Result<(), &'static str> {
        const BAD_SIZE: &str = "field size does not match its length and precision";
        let (len, prec) = (length as usize, precision as usize);
        let int_cap = len.checked_sub(prec).ok_or(BAD_SIZE)?;
        let (int, frac) = self.split();
        let (int, frac) = (trim_leading_zeros(int), trim_trailing_zeros(frac));
        if int.len() > int_cap {
            return Err("too many integer digits");
        }
        if frac.len() > prec {
            return Err("too many fraction digits");
        }
        let nibbles = len.checked_add(1).ok_or(BAD_SIZE)?;
        let out = out.get_mut(..nibbles.div_ceil(2)).ok_or(BAD_SIZE)?;
        out.fill(0);
        let sign = if self.negative && !self.is_zero() {
            0
        } else {
            1
        };
        let digits = int
            .iter()
            .enumerate()
            .map(|(k, &d)| (1 + int_cap - int.len() + k, d))
            .chain(frac.iter().enumerate().map(|(k, &d)| (1 + int_cap + k, d)));
        for (at, d) in std::iter::once((0, sign)).chain(digits) {
            set_nibble(out, at, d).ok_or(BAD_SIZE)?;
        }
        Ok(())
    }
}

impl fmt::Display for Numeric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (int, frac) = self.split();
        let mut s = String::with_capacity(self.digits.len() + 2);
        if self.negative {
            s.push('-');
        }
        if int.is_empty() {
            s.push('0');
        }
        push_digits(&mut s, int);
        if !frac.is_empty() {
            s.push('.');
            push_digits(&mut s, frac);
        }
        f.write_str(&s)
    }
}

fn push_digits(s: &mut String, digits: &[u8]) {
    s.extend(digits.iter().map(|&d| char::from(b'0' + d.min(9))));
}

fn trim_leading_zeros(d: &[u8]) -> &[u8] {
    let zeros = d.iter().take_while(|&&x| x == 0).count();
    d.get(zeros..).unwrap_or_default()
}

fn trim_trailing_zeros(d: &[u8]) -> &[u8] {
    let keep = d.len() - d.iter().rev().take_while(|&&x| x == 0).count();
    d.get(..keep).unwrap_or_default()
}

fn nibble(b: &[u8], i: usize) -> Option<u8> {
    let byte = *b.get(i / 2)?;
    Some(if i.is_multiple_of(2) {
        byte >> 4
    } else {
        byte & 0x0F
    })
}

fn set_nibble(b: &mut [u8], i: usize, v: u8) -> Option<()> {
    let byte = b.get_mut(i / 2)?;
    *byte = if i.is_multiple_of(2) {
        (*byte & 0x0F) | (v << 4)
    } else {
        (*byte & 0xF0) | (v & 0x0F)
    };
    Some(())
}

const CP1251_HIGH: [char; 128] = [
    '\u{0402}', '\u{0403}', '\u{201A}', '\u{0453}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{20AC}', '\u{2030}', '\u{0409}', '\u{2039}', '\u{040A}', '\u{040C}', '\u{040B}', '\u{040F}',
    '\u{0452}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{FFFD}', '\u{2122}', '\u{0459}', '\u{203A}', '\u{045A}', '\u{045C}', '\u{045B}', '\u{045F}',
    '\u{00A0}', '\u{040E}', '\u{045E}', '\u{0408}', '\u{00A4}', '\u{0490}', '\u{00A6}', '\u{00A7}',
    '\u{0401}', '\u{00A9}', '\u{0404}', '\u{00AB}', '\u{00AC}', '\u{00AD}', '\u{00AE}', '\u{0407}',
    '\u{00B0}', '\u{00B1}', '\u{0406}', '\u{0456}', '\u{0491}', '\u{00B5}', '\u{00B6}', '\u{00B7}',
    '\u{0451}', '\u{2116}', '\u{0454}', '\u{00BB}', '\u{0458}', '\u{0405}', '\u{0455}', '\u{0457}',
    '\u{0410}', '\u{0411}', '\u{0412}', '\u{0413}', '\u{0414}', '\u{0415}', '\u{0416}', '\u{0417}',
    '\u{0418}', '\u{0419}', '\u{041A}', '\u{041B}', '\u{041C}', '\u{041D}', '\u{041E}', '\u{041F}',
    '\u{0420}', '\u{0421}', '\u{0422}', '\u{0423}', '\u{0424}', '\u{0425}', '\u{0426}', '\u{0427}',
    '\u{0428}', '\u{0429}', '\u{042A}', '\u{042B}', '\u{042C}', '\u{042D}', '\u{042E}', '\u{042F}',
    '\u{0430}', '\u{0431}', '\u{0432}', '\u{0433}', '\u{0434}', '\u{0435}', '\u{0436}', '\u{0437}',
    '\u{0438}', '\u{0439}', '\u{043A}', '\u{043B}', '\u{043C}', '\u{043D}', '\u{043E}', '\u{043F}',
    '\u{0440}', '\u{0441}', '\u{0442}', '\u{0443}', '\u{0444}', '\u{0445}', '\u{0446}', '\u{0447}',
    '\u{0448}', '\u{0449}', '\u{044A}', '\u{044B}', '\u{044C}', '\u{044D}', '\u{044E}', '\u{044F}',
];

pub fn cp1251_decode(b: &[u8]) -> String {
    b.iter().copied().map(cp1251_char).collect()
}
pub(crate) fn cp1251_char(c: u8) -> char {
    match c.checked_sub(0x80) {
        None => char::from(c),
        Some(i) => CP1251_HIGH
            .get(usize::from(i))
            .copied()
            .unwrap_or(char::REPLACEMENT_CHARACTER),
    }
}

pub(crate) fn utf16le_decode(b: &[u8]) -> Option<String> {
    if !b.len().is_multiple_of(2) {
        return None;
    }
    let units = (0..b.len() / 2).filter_map(|i| u16le(b, 2 * i));
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()
}

pub(crate) fn write_utf16le_padded(s: &str, cap_units: usize, out: &mut [u8]) -> Option<usize> {
    let out = out.get_mut(..cap_units.checked_mul(2)?)?;
    let mut units = 0usize;
    for u in s.encode_utf16() {
        put_u16le(out, 2 * units, u)?;
        units += 1;
    }
    for i in units..cap_units {
        put_u16le(out, 2 * i, u16::from(b' '))?;
    }
    Some(units)
}
