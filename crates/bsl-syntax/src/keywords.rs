#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Kw {
    If,
    Then,
    ElsIf,
    Else,
    EndIf,
    For,
    Each,
    In,
    To,
    While,
    Do,
    EndDo,
    Procedure,
    Function,
    EndProcedure,
    EndFunction,
    Var,
    Goto,
    Return,
    Continue,
    Break,
    And,
    Or,
    Not,
    Try,
    Except,
    Raise,
    EndTry,
    New,
    Execute,
    Val,
    Export,
    True,
    False,
    Undefined,
    Null,
    AddHandler,
    RemoveHandler,
}

const KEYWORDS: [(&str, Kw); 75] = [
    ("если", Kw::If),
    ("if", Kw::If),
    ("тогда", Kw::Then),
    ("then", Kw::Then),
    ("иначеесли", Kw::ElsIf),
    ("elsif", Kw::ElsIf),
    ("иначе", Kw::Else),
    ("else", Kw::Else),
    ("конецесли", Kw::EndIf),
    ("endif", Kw::EndIf),
    ("для", Kw::For),
    ("for", Kw::For),
    ("каждого", Kw::Each),
    ("each", Kw::Each),
    ("из", Kw::In),
    ("in", Kw::In),
    ("по", Kw::To),
    ("to", Kw::To),
    ("пока", Kw::While),
    ("while", Kw::While),
    ("цикл", Kw::Do),
    ("do", Kw::Do),
    ("конеццикла", Kw::EndDo),
    ("enddo", Kw::EndDo),
    ("процедура", Kw::Procedure),
    ("procedure", Kw::Procedure),
    ("функция", Kw::Function),
    ("function", Kw::Function),
    ("конецпроцедуры", Kw::EndProcedure),
    ("endprocedure", Kw::EndProcedure),
    ("конецфункции", Kw::EndFunction),
    ("endfunction", Kw::EndFunction),
    ("перем", Kw::Var),
    ("var", Kw::Var),
    ("перейти", Kw::Goto),
    ("goto", Kw::Goto),
    ("возврат", Kw::Return),
    ("return", Kw::Return),
    ("продолжить", Kw::Continue),
    ("continue", Kw::Continue),
    ("прервать", Kw::Break),
    ("break", Kw::Break),
    ("и", Kw::And),
    ("and", Kw::And),
    ("или", Kw::Or),
    ("or", Kw::Or),
    ("не", Kw::Not),
    ("not", Kw::Not),
    ("попытка", Kw::Try),
    ("try", Kw::Try),
    ("исключение", Kw::Except),
    ("except", Kw::Except),
    ("вызватьисключение", Kw::Raise),
    ("raise", Kw::Raise),
    ("конецпопытки", Kw::EndTry),
    ("endtry", Kw::EndTry),
    ("новый", Kw::New),
    ("new", Kw::New),
    ("выполнить", Kw::Execute),
    ("execute", Kw::Execute),
    ("знач", Kw::Val),
    ("val", Kw::Val),
    ("экспорт", Kw::Export),
    ("export", Kw::Export),
    ("истина", Kw::True),
    ("true", Kw::True),
    ("ложь", Kw::False),
    ("false", Kw::False),
    ("неопределено", Kw::Undefined),
    ("undefined", Kw::Undefined),
    ("null", Kw::Null),
    ("добавитьобработчик", Kw::AddHandler),
    ("addhandler", Kw::AddHandler),
    ("удалитьобработчик", Kw::RemoveHandler),
    ("removehandler", Kw::RemoveHandler),
];

const fn units_len(s: &str) -> usize {
    let b = s.as_bytes();
    let mut i = 0;
    let mut n = 0;
    while i < b.len() {
        assert!(b[i] < 0xE0, "keyword outside BMP 2-byte range");
        if b[i] & 0xC0 != 0x80 {
            n += 1;
        }
        i += 1;
    }
    n
}

const fn max_len() -> usize {
    let mut max = 0;
    let mut i = 0;
    while i < KEYWORDS.len() {
        let n = units_len(KEYWORDS[i].0);
        if n > max {
            max = n;
        }
        i += 1;
    }
    max
}

const MAX_LEN: usize = max_len();

#[derive(Clone, Copy)]
struct Entry {
    len: usize,
    units: [u16; MAX_LEN],
    kw: Kw,
}

const fn build() -> [Entry; KEYWORDS.len()] {
    let mut out = [const {
        Entry {
            len: 0,
            units: [0; MAX_LEN],
            kw: Kw::If,
        }
    }; KEYWORDS.len()];
    let mut k = 0;
    while k < KEYWORDS.len() {
        let b = KEYWORDS[k].0.as_bytes();
        let mut i = 0;
        let mut n = 0;
        while i < b.len() {
            if b[i] < 0x80 {
                out[k].units[n] = b[i] as u16;
            } else {
                out[k].units[n] = (((b[i] & 0x1F) as u16) << 6) | (b[i + 1] & 0x3F) as u16;
                i += 1;
            }
            n += 1;
            i += 1;
        }
        out[k].len = n;
        out[k].kw = KEYWORDS[k].1;
        k += 1;
    }
    let mut i = 1;
    while i < out.len() {
        let mut j = i;
        while j > 0 && out[j - 1].len > out[j].len {
            let t = out[j - 1];
            out[j - 1] = out[j];
            out[j] = t;
            j -= 1;
        }
        i += 1;
    }
    out
}

const SORTED: [Entry; KEYWORDS.len()] = build();

const fn by_len() -> [(usize, usize); MAX_LEN + 1] {
    let mut r = [(0, 0); MAX_LEN + 1];
    let mut n = 0;
    while n <= MAX_LEN {
        let mut a = 0;
        while a < SORTED.len() && SORTED[a].len < n {
            a += 1;
        }
        let mut b = a;
        while b < SORTED.len() && SORTED[b].len == n {
            b += 1;
        }
        r[n] = (a, b);
        n += 1;
    }
    r
}

static TABLE: [Entry; KEYWORDS.len()] = SORTED;
static BY_LEN: [(usize, usize); MAX_LEN + 1] = by_len();

fn fold_unit(c: u16) -> Option<u16> {
    match c {
        0x41..=0x5A => return Some(c + 0x20),
        0..=0x7F | 0x0430..=0x045F => return Some(c),
        0x0400..=0x040F => return Some(c + 0x50),
        0x0410..=0x042F => return Some(c + 0x20),
        _ => {}
    }
    let ch = char::from_u32(u32::from(c))?;
    let mut it = ch.to_lowercase();
    match (it.next(), it.next()) {
        (Some(l), None) => u16::try_from(u32::from(l)).ok(),
        _ => Some(c),
    }
}

pub(crate) fn ident_is(ident: &[u16], lower: &str) -> bool {
    let mut it = lower.encode_utf16();
    ident
        .iter()
        .all(|&c| it.next().is_some_and(|l| fold_unit(c) == Some(l)))
        && it.next().is_none()
}

pub(crate) fn lookup(ident: &[u16]) -> Option<Kw> {
    let n = ident.len();
    let &(a, b) = BY_LEN.get(n)?;
    let candidates = TABLE.get(a..b).filter(|c| !c.is_empty())?;
    let mut buf = [0u16; MAX_LEN];
    for (d, &c) in buf.iter_mut().zip(ident) {
        *d = fold_unit(c)?;
    }
    let folded = buf.get(..n)?;
    candidates
        .iter()
        .find(|e| e.units.get(..n) == Some(folded))
        .map(|e| e.kw)
}
