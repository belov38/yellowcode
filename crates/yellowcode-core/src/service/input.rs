use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};
use query_console::{Lang, Mode, ParamIn, RunRequest};
use std::io::Read;

#[derive(Debug)]
pub enum InputError {
    Malformed,
    Limit,
    Memory(BudgetError),
}
impl From<BudgetError> for InputError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
pub struct ParsedQuery {
    pub request: Charged<RunRequest>,
    pub timeout: f64,
    pub max_rows: usize,
}

type Result<T> = std::result::Result<T, InputError>;
struct Parser<'a> {
    source: &'a [u8],
    position: usize,
    budget: &'a MemoryBudget,
}
impl Parser<'_> {
    fn whitespace(&mut self) {
        while self
            .source
            .get(self.position)
            .is_some_and(|b| matches!(b, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.position += 1;
        }
    }
    fn take(&mut self, token: u8) -> Result<()> {
        self.whitespace();
        if self.source.get(self.position) != Some(&token) {
            return Err(InputError::Malformed);
        }
        self.position += 1;
        Ok(())
    }
    fn consume(&mut self, token: u8) -> bool {
        self.whitespace();
        if self.source.get(self.position) == Some(&token) {
            self.position += 1;
            true
        } else {
            false
        }
    }
    fn end(&mut self) -> Result<()> {
        self.whitespace();
        if self.position == self.source.len() {
            Ok(())
        } else {
            Err(InputError::Malformed)
        }
    }
    fn string(&mut self) -> Result<Charged<String>> {
        self.take(b'"')?;
        let mut bytes = BudgetedVec::<u8>::new(self.budget, Category::Input)?;
        loop {
            let start = self.position;
            while self
                .source
                .get(self.position)
                .is_some_and(|b| *b >= 0x20 && !matches!(b, b'"' | b'\\'))
            {
                self.position += 1;
            }
            bytes.try_extend_from_slice(
                self.source
                    .get(start..self.position)
                    .ok_or(InputError::Malformed)?,
            )?;
            let byte = *self
                .source
                .get(self.position)
                .ok_or(InputError::Malformed)?;
            self.position += 1;
            match byte {
                b'"' => {
                    return bytes
                        .into_charged_vec()
                        .try_map(String::from_utf8)
                        .map_err(|_| InputError::Malformed);
                }
                b'\\' => {
                    let escaped = *self
                        .source
                        .get(self.position)
                        .ok_or(InputError::Malformed)?;
                    self.position += 1;
                    let ch = match escaped {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let first = self.hex4()?;
                            let scalar = if (0xd800..=0xdbff).contains(&first) {
                                if self.source.get(self.position..self.position + 2) != Some(b"\\u")
                                {
                                    return Err(InputError::Malformed);
                                }
                                self.position += 2;
                                let second = self.hex4()?;
                                if !(0xdc00..=0xdfff).contains(&second) {
                                    return Err(InputError::Malformed);
                                }
                                0x10000
                                    + ((u32::from(first) - 0xd800) << 10)
                                    + (u32::from(second) - 0xdc00)
                            } else {
                                u32::from(first)
                            };
                            char::from_u32(scalar).ok_or(InputError::Malformed)?
                        }
                        _ => return Err(InputError::Malformed),
                    };
                    let mut encoded = [0; 4];
                    bytes.try_extend_from_slice(ch.encode_utf8(&mut encoded).as_bytes())?;
                }
                _ => return Err(InputError::Malformed),
            }
        }
    }
    fn hex4(&mut self) -> Result<u16> {
        let bytes = self
            .source
            .get(self.position..self.position + 4)
            .ok_or(InputError::Malformed)?;
        let mut value = 0u16;
        for byte in bytes {
            value = (value << 4)
                | match byte {
                    b'0'..=b'9' => u16::from(byte - b'0'),
                    b'a'..=b'f' => u16::from(byte - b'a' + 10),
                    b'A'..=b'F' => u16::from(byte - b'A' + 10),
                    _ => return Err(InputError::Malformed),
                };
        }
        self.position += 4;
        Ok(value)
    }
    fn number(&mut self) -> Result<&str> {
        self.whitespace();
        let start = self.position;
        if self.source.get(self.position) == Some(&b'-') {
            self.position += 1;
        }
        match self.source.get(self.position) {
            Some(b'0') => self.position += 1,
            Some(b'1'..=b'9') => {
                self.position += 1;
                while self
                    .source
                    .get(self.position)
                    .is_some_and(u8::is_ascii_digit)
                {
                    self.position += 1;
                }
            }
            _ => return Err(InputError::Malformed),
        }
        if self.source.get(self.position) == Some(&b'.') {
            self.position += 1;
            self.digits()?;
        }
        if self
            .source
            .get(self.position)
            .is_some_and(|b| matches!(b, b'e' | b'E'))
        {
            self.position += 1;
            if self
                .source
                .get(self.position)
                .is_some_and(|b| matches!(b, b'+' | b'-'))
            {
                self.position += 1;
            }
            self.digits()?;
        }
        std::str::from_utf8(
            self.source
                .get(start..self.position)
                .ok_or(InputError::Malformed)?,
        )
        .map_err(|_| InputError::Malformed)
    }
    fn digits(&mut self) -> Result<()> {
        let start = self.position;
        while self
            .source
            .get(self.position)
            .is_some_and(u8::is_ascii_digit)
        {
            self.position += 1;
        }
        if self.position == start {
            Err(InputError::Malformed)
        } else {
            Ok(())
        }
    }
    fn skip(&mut self, depth: usize) -> Result<()> {
        if depth > 128 {
            return Err(InputError::Limit);
        }
        self.whitespace();
        match self.source.get(self.position) {
            Some(b'"') => {
                self.skip_string()?;
            }
            Some(b'{') => {
                self.take(b'{')?;
                if !self.consume(b'}') {
                    loop {
                        self.skip_string()?;
                        self.take(b':')?;
                        self.skip(depth + 1)?;
                        if self.consume(b'}') {
                            break;
                        }
                        self.take(b',')?;
                    }
                }
            }
            Some(b'[') => {
                self.take(b'[')?;
                if !self.consume(b']') {
                    loop {
                        self.skip(depth + 1)?;
                        if self.consume(b']') {
                            break;
                        }
                        self.take(b',')?;
                    }
                }
            }
            Some(b't') => self.literal(b"true")?,
            Some(b'f') => self.literal(b"false")?,
            Some(b'n') => self.literal(b"null")?,
            _ => {
                self.number()?;
            }
        }
        Ok(())
    }
    fn skip_string(&mut self) -> Result<()> {
        self.take(b'"')?;
        loop {
            let byte = *self
                .source
                .get(self.position)
                .ok_or(InputError::Malformed)?;
            self.position += 1;
            match byte {
                b'"' => return Ok(()),
                b'\\' => {
                    let escape = *self
                        .source
                        .get(self.position)
                        .ok_or(InputError::Malformed)?;
                    self.position += 1;
                    match escape {
                        b'u' => {
                            self.hex4()?;
                        }
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                        _ => return Err(InputError::Malformed),
                    }
                }
                0..=31 => return Err(InputError::Malformed),
                _ => {}
            }
        }
    }
    fn literal(&mut self, text: &[u8]) -> Result<()> {
        if self.source.get(self.position..self.position + text.len()) != Some(text) {
            return Err(InputError::Malformed);
        }
        self.position += text.len();
        Ok(())
    }
    fn params(&mut self) -> Result<Charged<Vec<ParamIn>>> {
        self.take(b'[')?;
        let mut params =
            BudgetedVec::<ParamIn>::try_with_capacity(self.budget, Category::Input, 4096)?
                .into_charged_vec();
        if self.consume(b']') {
            return Ok(params);
        }
        loop {
            if params.len() == 4096 {
                return Err(InputError::Limit);
            }
            self.take(b'{')?;
            let (mut name, mut repr) = (None, None);
            if !self.consume(b'}') {
                loop {
                    let key = self.string()?;
                    self.take(b':')?;
                    match key.as_str() {
                        "name" if name.is_none() => name = Some(self.string()?),
                        "repr" if repr.is_none() => repr = Some(self.string()?),
                        "name" | "repr" => return Err(InputError::Malformed),
                        _ => self.skip(3)?,
                    }
                    if self.consume(b'}') {
                        break;
                    }
                    self.take(b',')?;
                }
            }
            let param = name
                .ok_or(InputError::Malformed)?
                .try_zip(repr.ok_or(InputError::Malformed)?)?
                .map(|(name, repr)| ParamIn { name, repr });
            params = params.try_zip(param)?.map(|(mut values, param)| {
                values.push(param);
                values
            });
            if self.consume(b']') {
                break;
            }
            self.take(b',')?;
        }
        Ok(params)
    }
}

pub fn parse_query(source: &[u8], budget: &MemoryBudget) -> Result<ParsedQuery> {
    std::str::from_utf8(source).map_err(|_| InputError::Malformed)?;
    let mut parser = Parser {
        source,
        position: 0,
        budget,
    };
    parser.take(b'{')?;
    let (mut text, mut params, mut timeout, mut max_rows) = (None, None, None, None);
    if !parser.consume(b'}') {
        loop {
            let key = parser.string()?;
            parser.take(b':')?;
            match key.as_str() {
                "text" if text.is_none() => text = Some(parser.string()?),
                "params" if params.is_none() => params = Some(parser.params()?),
                "timeout" if timeout.is_none() => {
                    let value = parser
                        .number()?
                        .parse::<f64>()
                        .map_err(|_| InputError::Malformed)?;
                    if !value.is_finite() {
                        return Err(InputError::Malformed);
                    }
                    timeout = Some(value);
                }
                "maxRows" if max_rows.is_none() => {
                    max_rows = Some(
                        parser
                            .number()?
                            .parse::<usize>()
                            .map_err(|_| InputError::Malformed)?,
                    )
                }
                _ => return Err(InputError::Malformed),
            }
            if parser.consume(b'}') {
                break;
            }
            parser.take(b',')?;
        }
    }
    parser.end()?;
    let text = text.ok_or(InputError::Malformed)?;
    let params = match params {
        Some(p) => p,
        None => BudgetedVec::new(budget, Category::Input)?.into_charged_vec(),
    };
    let request = text.try_zip(params)?.map(|(mut text, params)| {
        if text.starts_with('\u{feff}') {
            text.drain(..3);
        }
        RunRequest {
            text,
            params,
            mode: Mode::Batch,
            lang: Lang::En,
        }
    });
    Ok(ParsedQuery {
        request,
        timeout: timeout.unwrap_or(60.0),
        max_rows: max_rows.unwrap_or(10000),
    })
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed => f.write_str("invalid query/parameter JSON or UTF-8"),
            Self::Limit => f.write_str("MEMORY_LIMIT: input"),
            Self::Memory(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for InputError {}
pub fn parameters_json(source: &[u8], budget: &MemoryBudget) -> Result<Charged<Vec<ParamIn>>> {
    std::str::from_utf8(source).map_err(|_| InputError::Malformed)?;
    let mut parser = Parser {
        source,
        position: 0,
        budget,
    };
    let params = parser.params()?;
    parser.end()?;
    Ok(params)
}
pub fn copy_text(text: &str, budget: &MemoryBudget) -> Result<Charged<String>> {
    let mut bytes = BudgetedVec::try_with_capacity(budget, Category::Input, text.len())?;
    bytes.try_extend_from_slice(text.as_bytes())?;
    bytes
        .into_charged_vec()
        .try_map(String::from_utf8)
        .map_err(|_| InputError::Malformed)
}
pub fn append_parameter(
    params: Charged<Vec<ParamIn>>,
    name: &str,
    repr: &str,
    budget: &MemoryBudget,
) -> Result<Charged<Vec<ParamIn>>> {
    if params.len() >= 4096 {
        return Err(InputError::Limit);
    }
    let param = copy_text(name, budget)?
        .try_zip(copy_text(repr, budget)?)?
        .map(|(name, repr)| ParamIn { name, repr });
    Ok(params.try_zip(param)?.map(|(mut params, param)| {
        params.push(param);
        params
    }))
}
pub fn read_text(reader: &mut dyn Read, budget: &MemoryBudget) -> crate::Result<Charged<String>> {
    let mut bytes = BudgetedVec::<u8>::new(budget, Category::Input)?;
    let mut chunk = [0; 8192];
    loop {
        let count = chunk
            .len()
            .min(super::query::MAX_INPUT_BYTES - bytes.len() + 1);
        let slice = chunk.get_mut(..count).ok_or(InputError::Limit)?;
        let n = reader.read(slice)?;
        if n == 0 {
            break;
        }
        if n > super::query::MAX_INPUT_BYTES - bytes.len() {
            return Err(InputError::Limit.into());
        }
        bytes.try_extend_from_slice(slice.get(..n).ok_or(InputError::Malformed)?)?;
    }
    let text = bytes
        .into_charged_vec()
        .try_map(String::from_utf8)
        .map_err(|_| InputError::Malformed)?;
    Ok(text.map(|mut text| {
        if text.starts_with('\u{feff}') {
            text.drain(..3);
        }
        text
    }))
}

pub fn index_request(
    index: &str,
    eq: Option<&str>,
    range: Option<&str>,
    order: query_console::IndexOrder,
    budget: &MemoryBudget,
) -> Result<Charged<query_console::TableIndexRequest>> {
    let index = copy_text(index, budget)?;
    let eq = match eq {
        None => BudgetedVec::<String>::new(budget, Category::Input)?.into_charged_vec(),
        Some(source) => {
            let mut parser = Parser {
                source: source.as_bytes(),
                position: 0,
                budget,
            };
            parser.take(b'[')?;
            let capacity = source
                .bytes()
                .filter(|b| *b == b',')
                .count()
                .checked_add(1)
                .ok_or(InputError::Limit)?;
            let mut values =
                BudgetedVec::<String>::try_with_capacity(budget, Category::Input, capacity)?
                    .into_charged_vec();
            if !parser.consume(b']') {
                loop {
                    let value = parser.string()?;
                    values = values.try_zip(value)?.map(|(mut values, value)| {
                        values.push(value);
                        values
                    });
                    if parser.consume(b']') {
                        break;
                    }
                    parser.take(b',')?;
                }
            }
            parser.end()?;
            values
        }
    };
    let mut bounds = BudgetedVec::<u8>::new(budget, Category::Input)?
        .into_charged_vec()
        .map(|_| query_console::IndexRange::default());
    if let Some(source) = range {
        let mut parser = Parser {
            source: source.as_bytes(),
            position: 0,
            budget,
        };
        parser.take(b'{')?;
        let mut seen = 0u8;
        if !parser.consume(b'}') {
            loop {
                let key = parser.string()?;
                let bit = match key.as_str() {
                    "gt" => 1,
                    "gte" => 2,
                    "lt" => 4,
                    "lte" => 8,
                    _ => return Err(InputError::Malformed),
                };
                if seen & bit != 0 {
                    return Err(InputError::Malformed);
                }
                seen |= bit;
                parser.take(b':')?;
                parser.whitespace();
                if parser.source.get(parser.position) == Some(&b'n') {
                    parser.literal(b"null")?;
                } else {
                    let value = parser.string()?;
                    bounds = bounds.try_zip(value)?.map(|(mut bounds, value)| {
                        match bit {
                            1 => bounds.gt = Some(value),
                            2 => bounds.gte = Some(value),
                            4 => bounds.lt = Some(value),
                            _ => bounds.lte = Some(value),
                        }
                        bounds
                    });
                }
                if parser.consume(b'}') {
                    break;
                }
                parser.take(b',')?;
            }
        }
        parser.end()?;
    }
    Ok(index
        .try_zip(eq)?
        .try_zip(bounds)?
        .map(|((index, eq), range)| query_console::TableIndexRequest {
            index,
            eq,
            range,
            order,
        }))
}
