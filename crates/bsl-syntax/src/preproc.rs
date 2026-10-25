use crate::diag::{MsgId, msg};
use crate::fold::fold_ident;
use crate::lexer::{Lexed, Token, TokenKind, ends_word, is_ident_part, is_ident_start, is_space};
use crate::source::{SourceText, Span};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ExecContext {
    Server,
    ThinClient,
    WebClient,
    MobileClient,
    ThickClientManaged,
    ThickClientOrdinary,
    ExternalConnection,
    MobileAppClient,
    MobileAppServer,
    MobileStandaloneServer,
}

impl ExecContext {
    pub fn is_client(self) -> bool {
        use ExecContext::*;
        matches!(
            self,
            ThinClient
                | WebClient
                | MobileClient
                | ThickClientManaged
                | ThickClientOrdinary
                | MobileAppClient
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PreprocSymbol {
    Server,
    AtServer,
    Client,
    AtClient,
    ThinClient,
    WebClient,
    MobileClient,
    ExternalConnection,
    ThickClientManagedApplication,
    ThickClientOrdinaryApplication,
    MobileStandaloneServer,
    MobileAppClient,
    MobileAppServer,
}

impl PreprocSymbol {
    fn bit(self) -> u16 {
        1 << (self as u16)
    }

    fn from_folded(w: &str) -> Option<Self> {
        use PreprocSymbol::*;
        Some(match w {
            "сервер" | "server" => Server,
            "насервере" | "atserver" => AtServer,
            "клиент" | "client" => Client,
            "наклиенте" | "atclient" => AtClient,
            "тонкийклиент" | "thinclient" => ThinClient,
            "вебклиент" | "webclient" => WebClient,
            "мобильныйклиент" | "mobileclient" => MobileClient,
            "внешнеесоединение" | "externalconnection" => ExternalConnection,
            "толстыйклиентуправляемоеприложение" | "thickclientmanagedapplication" => {
                ThickClientManagedApplication
            }
            "толстыйклиентобычноеприложение" | "thickclientordinaryapplication" => {
                ThickClientOrdinaryApplication
            }
            "мобильныйавтономныйсервер" | "mobilestandaloneserver" => {
                MobileStandaloneServer
            }
            "мобильноеприложениеклиент" | "mobileappclient" => {
                MobileAppClient
            }
            "мобильноеприложениесервер" | "mobileappserver" => {
                MobileAppServer
            }
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PreprocSymbols(u16);

impl PreprocSymbols {
    pub fn for_context(c: ExecContext) -> Self {
        use ExecContext::*;
        use PreprocSymbol as S;
        let list: &[S] = match c {
            Server => &[S::Server, S::AtServer],
            ThinClient => &[S::Client, S::AtClient, S::ThinClient],
            WebClient => &[S::Client, S::AtClient, S::WebClient],
            MobileClient => &[S::Client, S::AtClient, S::MobileClient],
            ThickClientManaged => &[
                S::Client,
                S::AtClient,
                S::ThickClientManagedApplication,
                S::Server,
                S::AtServer,
            ],
            ThickClientOrdinary => &[
                S::Client,
                S::AtClient,
                S::ThickClientOrdinaryApplication,
                S::Server,
                S::AtServer,
            ],
            ExternalConnection => &[S::Server, S::AtServer, S::ExternalConnection],
            MobileAppClient => &[S::Client, S::AtClient, S::MobileAppClient],
            MobileAppServer => &[S::Server, S::AtServer, S::MobileAppServer],
            MobileStandaloneServer => &[S::Server, S::AtServer, S::MobileStandaloneServer],
        };
        PreprocSymbols(list.iter().fold(0, |m, s| m | s.bit()))
    }

    pub fn none() -> Self {
        PreprocSymbols(0)
    }

    pub fn defines(self, s: PreprocSymbol) -> bool {
        self.0 & s.bit() != 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    Normal,
    Inserted,
}

#[derive(Clone, Debug)]
pub struct Preprocessed {
    pub tokens: Vec<Token>,
    pub origin: Vec<Origin>,
    pub deleted: Vec<(u32, Vec<Token>)>,
    pub regions: Vec<(Span, Span)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    If,
    Region,
    Insert,
    Delete,
}

struct Frame {
    kind: Kind,
    outer: bool,
    taken: bool,
    active: bool,
    saw_else: bool,
    region: usize,
}

type Fail = (MsgId, u32);

struct Scan<'a> {
    u: &'a [u16],
    i: usize,
    last_end: usize,
}

enum Tk {
    Word(usize, usize),
    LParen(usize),
    RParen(usize),
    Other(usize),
    End(usize),
}

const MAX_EXPR_DEPTH: u32 = 64;

impl Scan<'_> {
    fn next(&mut self) -> Tk {
        while self.u.get(self.i).copied().is_some_and(is_space) {
            self.i += 1;
        }
        let start = self.i;
        let Some(&c) = self.u.get(start) else {
            return Tk::End(self.last_end);
        };
        let t = if c == 0x2F && self.u.get(start + 1) == Some(&0x2F) {
            self.i = self.u.len();
            return Tk::End(self.last_end);
        } else if c == 0x28 {
            self.i += 1;
            Tk::LParen(start)
        } else if c == 0x29 {
            self.i += 1;
            Tk::RParen(start)
        } else if is_ident_start(c) {
            let mut word_end = start + 1;
            while self.u.get(word_end).copied().is_some_and(is_ident_part) {
                word_end += 1;
            }
            let mut e = word_end;
            while self.u.get(e).copied().is_some_and(|c| !ends_word(c)) {
                e += 1;
            }
            self.i = e;
            if e == word_end {
                Tk::Word(start, e)
            } else {
                Tk::Other(start)
            }
        } else {
            self.i += 1;
            Tk::Other(start)
        };
        self.last_end = self.i;
        t
    }

    fn peek(&mut self) -> Tk {
        let (i, l) = (self.i, self.last_end);
        let t = self.next();
        self.i = i;
        self.last_end = l;
        t
    }

    fn folded(&self, s: usize, e: usize) -> String {
        fold_ident(&String::from_utf16_lossy(self.u.get(s..e).unwrap_or(&[])))
    }

    fn at(t: &Tk) -> usize {
        match *t {
            Tk::Word(s, _) | Tk::LParen(s) | Tk::RParen(s) | Tk::Other(s) | Tk::End(s) => s,
        }
    }

    fn expect_end(&mut self) -> Result<(), usize> {
        match self.next() {
            Tk::End(_) => Ok(()),
            t => Err(Self::at(&t)),
        }
    }

    fn or_expr(&mut self, sy: PreprocSymbols, depth: u32) -> Result<bool, usize> {
        let mut v = self.and_expr(sy, depth)?;
        while let Tk::Word(s, e) = self.peek() {
            let w = self.folded(s, e);
            if w != "или" && w != "or" {
                break;
            }
            self.next();
            v |= self.and_expr(sy, depth)?;
        }
        Ok(v)
    }

    fn and_expr(&mut self, sy: PreprocSymbols, depth: u32) -> Result<bool, usize> {
        let mut v = self.not_expr(sy, depth)?;
        while let Tk::Word(s, e) = self.peek() {
            let w = self.folded(s, e);
            if w != "и" && w != "and" {
                break;
            }
            self.next();
            v &= self.not_expr(sy, depth)?;
        }
        Ok(v)
    }

    fn not_expr(&mut self, sy: PreprocSymbols, depth: u32) -> Result<bool, usize> {
        let t = self.next();
        match t {
            Tk::Word(s, e) => {
                let w = self.folded(s, e);
                if w == "не" || w == "not" {
                    if depth >= MAX_EXPR_DEPTH {
                        return Err(s);
                    }
                    return Ok(!self.not_expr(sy, depth + 1)?);
                }
                PreprocSymbol::from_folded(&w)
                    .map(|sym| sy.defines(sym))
                    .ok_or(s)
            }
            Tk::LParen(p) => {
                if depth >= MAX_EXPR_DEPTH {
                    return Err(p);
                }
                let v = self.or_expr(sy, depth + 1)?;
                match self.next() {
                    Tk::RParen(_) => Ok(v),
                    t => Err(Self::at(&t)),
                }
            }
            t => Err(Self::at(&t)),
        }
    }

    fn condition(&mut self, sy: PreprocSymbols) -> Result<bool, usize> {
        let v = self.or_expr(sy, 0)?;
        match self.next() {
            Tk::Word(s, e) => {
                let w = self.folded(s, e);
                if w != "тогда" && w != "then" {
                    return Err(s);
                }
            }
            t => return Err(Self::at(&t)),
        }
        match self.next() {
            Tk::End(_) => Ok(v),
            t => Err(Self::at(&t)),
        }
    }
}

fn off(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

fn bad(kind: MsgId, at: u32) -> Token {
    Token {
        kind: TokenKind::Bad(kind),
        span: Span { start: at, end: at },
    }
}

fn fail(m: MsgId, at: usize, base: usize) -> Fail {
    (m, off(base + at))
}

pub fn preprocess(
    src: &SourceText,
    lexed: &Lexed,
    symbols: PreprocSymbols,
    extension: bool,
) -> Preprocessed {
    let units = src.units();
    let mut out = Preprocessed {
        tokens: Vec::with_capacity(lexed.tokens.len()),
        origin: Vec::with_capacity(lexed.tokens.len()),
        deleted: Vec::new(),
        regions: Vec::new(),
    };
    let mut stack: Vec<Frame> = Vec::new();
    let mut active = true;
    let mut origin = Origin::Normal;
    let mut inserting = false;
    let mut delete_buf: Option<(u32, Vec<Token>)> = None;
    let mut deleting = false;

    for &t in &lexed.tokens {
        match t.kind {
            TokenKind::Directive => {
                let r = directive(
                    units,
                    t.span,
                    symbols,
                    extension,
                    &mut stack,
                    &mut out,
                    &mut inserting,
                    &mut deleting,
                    &mut delete_buf,
                );
                match r {
                    Ok(()) => {
                        active = stack.last().is_none_or(|f| f.active);
                        origin = if inserting {
                            Origin::Inserted
                        } else {
                            Origin::Normal
                        };
                    }
                    Err((m, at)) => {
                        out.tokens.push(bad(m, at));
                        out.origin.push(Origin::Normal);
                        return out;
                    }
                }
            }
            TokenKind::Eof => break,
            _ if !active => {}
            _ => {
                if let Some((_, buf)) = delete_buf.as_mut() {
                    buf.push(t);
                } else {
                    out.tokens.push(t);
                    out.origin.push(origin);
                }
            }
        }
    }
    let end = off(units.len());
    if let Some(f) = stack.last() {
        let m = match f.kind {
            Kind::If => msg::EXP_PREPROCESSORENDIF,
            Kind::Region => msg::EXP_PREPROCESSORENDREGION,
            Kind::Insert => msg::EXP_PREPROCESSORENDINSERT,
            Kind::Delete => msg::EXP_PREPROCESSORENDDELETE,
        };
        out.tokens.push(bad(m, end));
    } else {
        out.tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span { start: end, end },
        });
    }
    out.origin.push(Origin::Normal);
    out
}

#[allow(clippy::too_many_arguments)]
fn directive(
    units: &[u16],
    span: Span,
    symbols: PreprocSymbols,
    extension: bool,
    stack: &mut Vec<Frame>,
    out: &mut Preprocessed,
    inserting: &mut bool,
    deleting: &mut bool,
    delete_buf: &mut Option<(u32, Vec<Token>)>,
) -> Result<(), Fail> {
    let base = span.start as usize;
    let end = (span.end as usize).min(units.len());
    let body = units.get(base..end).unwrap_or(&[]);
    let mut sc = Scan {
        u: body,
        i: 1,
        last_end: 1,
    };
    let (ws, we) = match sc.next() {
        Tk::Word(s, e) => (s, e),
        t => return Err(fail(msg::EXP_PREPROCESSORSTMT, Scan::at(&t), base)),
    };
    let word = sc.folded(ws, we);
    let wpos = off(base + ws);
    let outer = stack.last().is_none_or(|f| f.active);
    let generic = |sc: &mut Scan<'_>, m: MsgId| sc.expect_end().map_err(|at| fail(m, at, base));
    match word.as_str() {
        "если" | "if" => {
            let cond = sc
                .condition(symbols)
                .map_err(|at| fail(msg::EXP_PREPROCESSOREXPR, at, base))?;
            stack.push(Frame {
                kind: Kind::If,
                outer,
                taken: cond,
                active: outer && cond,
                saw_else: false,
                region: usize::MAX,
            });
        }
        "иначеесли" | "elsif" => {
            let f = open_if(stack, wpos)?;
            if f.saw_else {
                return Err((msg::PREPROCESSORERROR, wpos));
            }
            let cond = sc
                .condition(symbols)
                .map_err(|at| fail(msg::EXP_PREPROCESSOREXPR, at, base))?;
            f.active = f.outer && !f.taken && cond;
            f.taken |= cond;
        }
        "иначе" | "else" => {
            let f = open_if(stack, wpos)?;
            if f.saw_else {
                return Err((msg::PREPROCESSORERROR, wpos));
            }
            generic(&mut sc, msg::PREPROCESSORERROR)?;
            f.active = f.outer && !f.taken;
            f.taken = true;
            f.saw_else = true;
        }
        "конецесли" | "endif" => {
            open_if(stack, wpos)?;
            generic(&mut sc, msg::PREPROCESSORERROR)?;
            stack.pop();
        }
        "область" | "region" => {
            let (ns, ne) = match sc.next() {
                Tk::Word(s, e) => (s, e),
                t => return Err(fail(msg::EXP_PREPROCESSORIDENT, Scan::at(&t), base)),
            };
            generic(&mut sc, msg::EXP_PREPROCESSORIDENT)?;
            let region = if outer {
                let name = Span {
                    start: off(base + ns),
                    end: off(base + ne),
                };
                out.regions.push((
                    name,
                    Span {
                        start: span.start,
                        end: span.end,
                    },
                ));
                out.regions.len() - 1
            } else {
                usize::MAX
            };
            stack.push(Frame {
                kind: Kind::Region,
                outer,
                taken: true,
                active: outer,
                saw_else: false,
                region,
            });
        }
        "конецобласти" | "endregion" => {
            match stack.last() {
                None => return Err((msg::MIS_PREPROCESSORREGION, wpos)),
                Some(f) if f.kind != Kind::Region => return Err((msg::PREPROCESSORERROR, wpos)),
                Some(_) => {}
            }
            generic(&mut sc, msg::PREPROCESSORERROR)?;
            if let Some(f) = stack.pop()
                && let Some((_, r)) = out.regions.get_mut(f.region)
            {
                r.end = span.end;
            }
        }
        "вставка" | "insert" | "удаление" | "delete" => {
            if !extension {
                return Err((msg::EXP_PREPROCESSORSTMT, wpos));
            }
            if *inserting || *deleting {
                return Err((msg::PREPROCESSORERROR, wpos));
            }
            generic(&mut sc, msg::PREPROCESSORERROR)?;
            let is_ins = word == "вставка" || word == "insert";
            if is_ins {
                *inserting = true;
            } else {
                *deleting = true;
                if outer {
                    *delete_buf = Some((off(out.tokens.len()), Vec::new()));
                }
            }
            stack.push(Frame {
                kind: if is_ins { Kind::Insert } else { Kind::Delete },
                outer,
                taken: true,
                active: outer,
                saw_else: false,
                region: usize::MAX,
            });
        }
        "конецвставки" | "endinsert" | "конецудаления" | "enddelete" => {
            let is_ins = word == "конецвставки" || word == "endinsert";
            let want = if is_ins { Kind::Insert } else { Kind::Delete };
            if !extension {
                return Err((msg::EXP_PREPROCESSORSTMT, wpos));
            }
            let open = if is_ins { *inserting } else { *deleting };
            if !open {
                let m = if is_ins {
                    msg::EXP_PREPROCESSORINSERT
                } else {
                    msg::EXP_PREPROCESSORDELETE
                };
                return Err((m, wpos));
            }
            if stack.last().is_none_or(|f| f.kind != want) {
                return Err((msg::PREPROCESSORERROR, wpos));
            }
            generic(&mut sc, msg::PREPROCESSORERROR)?;
            stack.pop();
            if is_ins {
                *inserting = false;
            } else {
                *deleting = false;
                if let Some(d) = delete_buf.take() {
                    out.deleted.push(d);
                }
            }
        }
        _ => return Err((msg::EXP_PREPROCESSORSTMT, wpos)),
    }
    Ok(())
}

fn open_if(stack: &mut [Frame], wpos: u32) -> Result<&mut Frame, Fail> {
    match stack.last_mut() {
        None => Err((msg::MIS_PREPROCESSORIF, wpos)),
        Some(f) if f.kind != Kind::If => Err((msg::PREPROCESSORERROR, wpos)),
        Some(f) => Ok(f),
    }
}
