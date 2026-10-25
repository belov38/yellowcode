use std::ops::Range;

pub fn writer_text(text: &[u16], file_line_sep: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(text.len());
    for &c in text {
        if c == 0x0A {
            out.extend_from_slice(file_line_sep);
        } else {
            out.push(c);
        }
    }
    out
}

fn split(text: &[u16], lone_cr: bool) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < text.len() {
        let c = text[i];
        if c == 0x0A {
            out.push(start..i);
            i += 1;
            start = i;
        } else if c == 0x0D && text.get(i + 1) == Some(&0x0A) {
            out.push(start..i);
            i += 2;
            start = i;
        } else if c == 0x0D && lone_cr {
            out.push(start..i);
            i += 1;
            start = i;
        } else {
            i += 1;
        }
    }
    if start < text.len() {
        out.push(start..text.len());
    }
    out
}

pub fn reader_lines(text: &[u16]) -> Vec<Range<usize>> {
    split(text, false)
}

pub fn reader_all(text: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i] == 0x0D && text.get(i + 1) == Some(&0x0A) {
            i += 1;
        }
        out.push(text[i]);
        i += 1;
    }
    out
}

pub fn document_lines(text: &[u16]) -> Vec<Range<usize>> {
    split(text, true)
}
