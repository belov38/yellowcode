use crate::error::{LibError, msg};
use bsl_prim::Number;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineBreak {
    Auto,
    Windows,
    Unix,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escape {
    None,
    NonAscii,
    NonBmp,
}

#[derive(Clone, Debug)]
pub struct WriteOptions {
    pub line_break: LineBreak,
    pub indent: Vec<u16>,
    pub double_quotes: bool,
    pub escape: Escape,
    pub escape_angle: bool,
    pub escape_line_separators: bool,
    pub escape_ampersand: bool,
    pub escape_single_quotes: bool,
    pub escape_slash: bool,
    pub auto_is_crlf: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            line_break: LineBreak::Auto,
            indent: Vec::new(),
            double_quotes: true,
            escape: Escape::None,
            escape_angle: false,
            escape_line_separators: false,
            escape_ampersand: false,
            escape_single_quotes: false,
            escape_slash: false,
            auto_is_crlf: true,
        }
    }
}

struct Ctx {
    is_obj: bool,
    count: usize,
    name_pending: bool,
}

pub struct Writer {
    o: WriteOptions,
    out: Vec<u16>,
    stack: Vec<Ctx>,
    top_done: bool,
}

fn bad_order() -> LibError {
    LibError::new(msg::BAD_ORDER)
}

impl Writer {
    pub fn new(o: WriteOptions) -> Writer {
        Writer {
            o,
            out: Vec::new(),
            stack: Vec::new(),
            top_done: false,
        }
    }

    pub fn len(&self) -> usize {
        self.out.len()
    }

    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    fn pretty(&self) -> bool {
        self.o.line_break != LineBreak::None
    }

    fn push_str(&mut self, s: &str) {
        self.out.extend(s.encode_utf16());
    }

    fn newline(&mut self, depth: usize) {
        match self.o.line_break {
            LineBreak::None => return,
            LineBreak::Unix => self.out.push(0x0A),
            LineBreak::Windows => self.out.extend([0x0D, 0x0A]),
            LineBreak::Auto => {
                if self.o.auto_is_crlf {
                    self.out.push(0x0D);
                }
                self.out.push(0x0A);
            }
        }
        for _ in 0..depth {
            self.out.extend_from_slice(&self.o.indent);
        }
    }

    fn begin_value(&mut self) -> Result<(), LibError> {
        let depth = self.stack.len();
        match self.stack.last_mut() {
            None => {
                if self.top_done {
                    return Err(bad_order());
                }
            }
            Some(c) if c.is_obj => {
                if !c.name_pending {
                    return Err(bad_order());
                }
                c.name_pending = false;
            }
            Some(c) => {
                let comma = c.count > 0;
                c.count += 1;
                if comma {
                    self.out.push(u16::from(b','));
                }
                self.newline(depth);
            }
        }
        Ok(())
    }

    fn end_value(&mut self) {
        if self.stack.is_empty() {
            self.top_done = true;
        }
    }

    fn begin_container(&mut self, is_obj: bool) -> Result<(), LibError> {
        self.begin_value()?;
        self.push_str(if is_obj { "{" } else { "[" });
        self.stack.push(Ctx {
            is_obj,
            count: 0,
            name_pending: false,
        });
        Ok(())
    }

    fn end_container(&mut self, is_obj: bool) -> Result<(), LibError> {
        match self.stack.last() {
            Some(c) if c.is_obj == is_obj && !c.name_pending => {}
            _ => return Err(bad_order()),
        }
        let c = self.stack.pop().ok_or_else(bad_order)?;
        if c.count > 0 {
            self.newline(self.stack.len());
        }
        self.push_str(if is_obj { "}" } else { "]" });
        self.end_value();
        Ok(())
    }

    pub fn start_object(&mut self) -> Result<(), LibError> {
        self.begin_container(true)
    }

    pub fn end_object(&mut self) -> Result<(), LibError> {
        self.end_container(true)
    }

    pub fn start_array(&mut self) -> Result<(), LibError> {
        self.begin_container(false)
    }

    pub fn end_array(&mut self) -> Result<(), LibError> {
        self.end_container(false)
    }

    pub fn property_name(&mut self, name: &[u16]) -> Result<(), LibError> {
        let depth = self.stack.len();
        let pretty = self.pretty();
        let comma = match self.stack.last_mut() {
            Some(c) if c.is_obj && !c.name_pending => {
                let comma = c.count > 0;
                c.count += 1;
                c.name_pending = true;
                comma
            }
            _ => return Err(bad_order()),
        };
        if comma {
            self.out.push(u16::from(b','));
        }
        self.newline(depth);
        self.quoted(name);
        self.push_str(if pretty { ": " } else { ":" });
        Ok(())
    }

    pub fn null(&mut self) -> Result<(), LibError> {
        self.scalar("null")
    }

    pub fn bool(&mut self, v: bool) -> Result<(), LibError> {
        self.scalar(if v { "true" } else { "false" })
    }

    pub fn number(&mut self, n: &Number) -> Result<(), LibError> {
        self.scalar(&n.to_xml_string())
    }

    fn scalar(&mut self, s: &str) -> Result<(), LibError> {
        self.begin_value()?;
        self.push_str(s);
        self.end_value();
        Ok(())
    }

    pub fn string(&mut self, s: &[u16]) -> Result<(), LibError> {
        self.begin_value()?;
        self.quoted(s);
        self.end_value();
        Ok(())
    }

    pub fn raw(&mut self, text: &[u16]) -> Result<(), LibError> {
        if let Some(c) = self.stack.last_mut() {
            c.name_pending = false;
        }
        self.out.extend_from_slice(text);
        Ok(())
    }

    pub fn finish(self) -> Result<Vec<u16>, LibError> {
        Ok(self.out)
    }

    fn quoted(&mut self, s: &[u16]) {
        let q = if self.o.double_quotes { b'"' } else { b'\'' };
        self.out.push(u16::from(q));
        let mut i = 0;
        while let Some(&u) = s.get(i) {
            i += 1;
            if (0xD800..0xDC00).contains(&u) {
                match s.get(i) {
                    Some(&lo) if (0xDC00..0xE000).contains(&lo) => {
                        i += 1;
                        if self.o.escape == Escape::None {
                            self.out.extend([u, lo]);
                        } else {
                            self.uescape(u);
                            self.uescape(lo);
                        }
                    }
                    _ => self.out.push(u16::from(b'?')),
                }
                continue;
            }
            if (0xDC00..0xE000).contains(&u) {
                self.out.push(u16::from(b'?'));
                continue;
            }
            self.unit(u);
        }
        self.out.push(u16::from(q));
    }

    fn unit(&mut self, u: u16) {
        let o = &self.o;
        let esc = match u {
            0x22 => Some("\\\""),
            0x5C => Some("\\\\"),
            0x0A => Some("\\n"),
            0x0D => Some("\\r"),
            0x27 if !o.double_quotes || o.escape_single_quotes => Some("\\u0027"),
            0x3C if o.escape_angle => Some("\\u003C"),
            0x3E if o.escape_angle => Some("\\u003E"),
            0x26 if o.escape_ampersand => Some("\\u0026"),
            0x2F if o.escape_slash => Some("\\/"),
            _ => None,
        };
        if let Some(e) = esc {
            self.push_str(e);
        } else if u < 0x20
            || u == 0x2028
            || u == 0x2029
            || (u > 0x7F && o.escape == Escape::NonAscii)
        {
            self.uescape(u);
        } else {
            self.out.push(u);
        }
    }

    fn uescape(&mut self, u: u16) {
        self.push_str(&format!("\\u{u:04X}"));
    }
}
