use super::repr_in_live;
use bsl_vm::{Exception, Execution, ObjRef, Str, Value};

const HANDLE_BYTES: u64 = 64;

struct Text {
    value: String,
    base: u64,
}
impl Text {
    fn new(base: u64) -> Self {
        Self {
            value: String::new(),
            base,
        }
    }
    fn live(&self) -> u64 {
        self.base.saturating_add(self.value.capacity() as u64)
    }
    fn push(&mut self, ex: &mut Execution, part: &str, extra: u64) -> Result<(), Exception> {
        ex.charge(part.len() as u64 / 64 + 1)?;
        let needed = self
            .value
            .len()
            .checked_add(part.len())
            .ok_or_else(|| ex.out_of_memory())?;
        let capacity = self.value.capacity();
        if needed > capacity {
            let next = needed.max(capacity.saturating_mul(2)).max(32);
            ex.reserve(
                self.live()
                    .saturating_add(extra)
                    .saturating_add(part.len() as u64)
                    .saturating_add(next as u64),
            )?;
            ex.charge(capacity as u64 / 64 + 1)?;
            self.value
                .try_reserve_exact(next.saturating_sub(self.value.len()))
                .map_err(|_| ex.out_of_memory())?;
        } else {
            ex.reserve(
                self.live()
                    .saturating_add(extra)
                    .saturating_add(part.len() as u64),
            )?;
        }
        self.value.push_str(part);
        Ok(())
    }
}

fn backing(value: &Value) -> u64 {
    match value {
        Value::Number(n) => (n.backing_capacity_limbs() as u64).saturating_mul(4),
        Value::String(s) => (s.backing_capacity_units() as u64).saturating_mul(2),
        _ => 0,
    }
}
pub(super) fn number(
    ex: &mut Execution,
    n: &bsl_prim::Number,
    live: u64,
) -> Result<String, Exception> {
    let groups = n.size_hint_limbs();
    let capacity = groups.saturating_mul(9).saturating_add(3);
    ex.charge(groups / 64 + 1)?;
    ex.reserve(
        live.saturating_add((n.backing_capacity_limbs() as u64).saturating_mul(4))
            .saturating_add(capacity),
    )?;
    let mut text = String::new();
    text.try_reserve_exact(usize::try_from(capacity).map_err(|_| ex.out_of_memory())?)
        .map_err(|_| ex.out_of_memory())?;
    struct Output<'a>(&'a mut String);
    impl std::fmt::Write for Output<'_> {
        fn write_str(&mut self, part: &str) -> std::fmt::Result {
            if part.len() > self.0.capacity().saturating_sub(self.0.len()) {
                return Err(std::fmt::Error);
            }
            self.0.push_str(part);
            Ok(())
        }
    }
    n.write_xml(&mut Output(&mut text))
        .map_err(|_| ex.out_of_memory())?;
    Ok(text)
}

pub(super) fn string(ex: &mut Execution, s: &Str, live: u64) -> Result<String, Exception> {
    utf8(ex, s, live, true)
}
fn utf8(ex: &mut Execution, s: &Str, live: u64, quoted: bool) -> Result<String, Exception> {
    let units = s.len() as u64;
    let capacity = units.saturating_mul(3).saturating_add(2);
    ex.charge(units / 64 + 1)?;
    ex.reserve(
        live.saturating_add((s.backing_capacity_units() as u64).saturating_mul(2))
            .saturating_add(capacity),
    )?;
    let mut text = String::new();
    text.try_reserve_exact(usize::try_from(capacity).map_err(|_| ex.out_of_memory())?)
        .map_err(|_| ex.out_of_memory())?;
    if quoted {
        text.push('"');
    }
    for ch in char::decode_utf16(s.units().iter().copied()) {
        let ch = ch.unwrap_or(char::REPLACEMENT_CHARACTER);
        text.push(ch);
        if quoted && ch == '"' {
            text.push('"');
        }
    }
    if quoted {
        text.push('"');
    }
    Ok(text)
}

struct Frame {
    rows: Value,
    next: usize,
    depth: u32,
}
struct Tree {
    text: Text,
    frames: Vec<Frame>,
    names: Vec<Value>,
    name_bytes: u64,
}
impl Tree {
    fn extra(&self) -> u64 {
        (self.frames.capacity() as u64)
            .saturating_mul(std::mem::size_of::<Frame>() as u64)
            .saturating_add((self.frames.len() as u64).saturating_mul(HANDLE_BYTES))
            .saturating_add(
                (self.names.capacity() as u64).saturating_mul(std::mem::size_of::<Value>() as u64),
            )
            .saturating_add(self.name_bytes)
    }
    fn live(&self) -> u64 {
        self.text.live().saturating_add(self.extra())
    }
    fn push(&mut self, ex: &mut Execution, part: &str) -> Result<(), Exception> {
        self.text.push(ex, part, self.extra())
    }
    fn owned(&mut self, ex: &mut Execution, part: &String) -> Result<(), Exception> {
        self.text.push(
            ex,
            part,
            self.extra()
                .saturating_add((part.capacity().saturating_sub(part.len())) as u64),
        )
    }
    fn cell(&mut self, ex: &mut Execution, text: &String, cell: &Value) -> Result<(), Exception> {
        self.text.push(
            ex,
            text,
            self.extra()
                .saturating_add(backing(cell))
                .saturating_add(spare(text)),
        )
    }
    fn frame(&mut self, ex: &mut Execution, rows: Value, depth: u32) -> Result<(), Exception> {
        ex.charge(1)?;
        let cap = self.frames.capacity();
        if self.frames.len() == cap {
            let next = cap.saturating_mul(2).max(4);
            ex.reserve(
                self.live()
                    .saturating_add(
                        (next as u64).saturating_mul(std::mem::size_of::<Frame>() as u64),
                    )
                    .saturating_add(HANDLE_BYTES),
            )?;
            ex.charge(cap as u64 / 64 + 1)?;
            self.frames
                .try_reserve_exact(next.saturating_sub(self.frames.len()))
                .map_err(|_| ex.out_of_memory())?;
        } else {
            ex.reserve(self.live().saturating_add(HANDLE_BYTES))?;
        }
        self.frames.push(Frame {
            rows,
            next: 0,
            depth,
        });
        Ok(())
    }
}

pub(super) fn repr(
    ex: &mut Execution,
    value: &Value,
    depth: u32,
    live: u64,
) -> Result<String, Exception> {
    let mut out = Tree {
        text: Text::new(live.saturating_add(HANDLE_BYTES.saturating_mul(3))),
        frames: Vec::new(),
        names: Vec::new(),
        name_bytes: 0,
    };
    schema(ex, value, &mut out)?;
    out.push(ex, "ДеревоЗначений<")?;
    for i in 0..out.names.len() {
        if i != 0 {
            out.push(ex, ", ")?;
        }
        if let Some(Value::String(name)) = out.names.get(i) {
            let text = utf8(ex, name, out.live(), false)?;
            out.owned(ex, &text)?;
        }
    }
    out.push(ex, ">[")?;
    ex.reserve(out.live().saturating_add(HANDLE_BYTES))?;
    let roots = ex.get_member(value, "Строки")?;
    out.frame(ex, roots, depth)?;
    while let Some(frame) = out.frames.last_mut() {
        ex.charge(1)?;
        let Some(rows) = frame.rows.as_object() else {
            return Err(ex.out_of_memory());
        };
        let len = rows.iter_len(ex).unwrap_or(0);
        if frame.next >= len {
            out.frames.pop();
            out.push(ex, "]")?;
            continue;
        }
        let i = frame.next;
        let row_depth = frame.depth;
        frame.next = frame.next.saturating_add(1);
        let rows = rows.clone();
        ex.reserve(out.live().saturating_add(HANDLE_BYTES))?;
        let row = rows.iter_get(ex, i).map_err(|e| ex.call_error(e))?;
        if i != 0 {
            out.push(ex, ", ")?;
        }
        out.push(ex, "(")?;
        for j in 0..out.names.len() {
            if j != 0 {
                out.push(ex, ", ")?;
            }
            let Some(name) = out.names.get(j) else {
                continue;
            };
            let cell = ex.index_get(&row, name)?;
            let cell_live = out.live().saturating_add(HANDLE_BYTES.saturating_mul(2));
            let text = repr_in_live(ex, &cell, row_depth.saturating_add(1), cell_live)?;
            out.cell(ex, &text, &cell)?;
        }
        out.push(ex, ")")?;
        ex.reserve(out.live().saturating_add(HANDLE_BYTES.saturating_mul(2)))?;
        let children = ex.get_member(&row, "Строки")?;
        if children
            .as_object()
            .and_then(|o| o.iter_len(ex))
            .unwrap_or(0)
            > 0
        {
            out.push(ex, "[")?;
            out.frame(ex, children, row_depth.saturating_add(1))?;
        }
    }
    Ok(out.text.value)
}

pub(super) fn container(
    ex: &mut Execution,
    value: &Value,
    object: &ObjRef,
    name: &str,
    depth: u32,
    live: u64,
) -> Option<Result<String, Exception>> {
    match name {
        "Массив"
        | "ФиксированныйМассив"
        | "Структура"
        | "ФиксированнаяСтруктура"
        | "Соответствие"
        | "ФиксированноеСоответствие"
        | "СписокЗначений" => {}
        "ТаблицаЗначений" | "СтрокаТаблицыЗначений" => {
            return Some(table(ex, value, object, name, depth, live));
        }
        _ => return None,
    }
    Some((|| {
        let structure = matches!(name, "Структура" | "ФиксированнаяСтруктура");
        let map = matches!(name, "Соответствие" | "ФиксированноеСоответствие");
        let mut text = Text::new(live.saturating_add(HANDLE_BYTES.saturating_mul(5)));
        text.push(ex, name, 0)?;
        text.push(ex, if structure || map { "{" } else { "[" }, 0)?;
        let len = object.iter_len(ex).unwrap_or(0);
        for i in 0..len {
            ex.charge(1)?;
            if i != 0 {
                text.push(ex, ", ", 0)?;
            }
            let item = object.iter_get(ex, i).map_err(|e| ex.call_error(e))?;
            if structure || map {
                let key = ex.get_member(&item, "Ключ")?;
                let cell = ex.get_member(&item, "Значение")?;
                let retained = backing(&key).saturating_add(backing(&cell));
                ex.reserve(text.live().saturating_add(retained.saturating_mul(2)))?;
                drop(item);
                let key_live = text.live().saturating_add(backing(&cell));
                let key_text = if map {
                    repr_in_live(ex, &key, depth, key_live)?
                } else {
                    match &key {
                        Value::String(s) => utf8(ex, s, key_live, false)?,
                        _ => String::new(),
                    }
                };
                text.push(ex, &key_text, retained.saturating_add(spare(&key_text)))?;
                drop(key_text);
                drop(key);
                text.push(ex, if map { " => " } else { ": " }, backing(&cell))?;
                let cell_text = repr_in_live(ex, &cell, depth, text.live())?;
                text.push(
                    ex,
                    &cell_text,
                    backing(&cell).saturating_add(spare(&cell_text)),
                )?;
            } else if name == "СписокЗначений" {
                let cell = ex.get_member(&item, "Значение")?;
                let presentation = ex.get_member(&item, "Представление")?;
                let check = ex.get_member(&item, "Пометка")?;
                drop(item);
                let other = backing(&presentation).saturating_add(backing(&check));
                let cell_text = repr_in_live(ex, &cell, depth, text.live().saturating_add(other))?;
                text.push(
                    ex,
                    &cell_text,
                    other
                        .saturating_add(backing(&cell))
                        .saturating_add(spare(&cell_text)),
                )?;
                drop(cell_text);
                drop(cell);
                let label = match &presentation {
                    Value::String(s) => {
                        utf8(ex, s, text.live().saturating_add(backing(&check)), false)?
                    }
                    _ => ex.present(&presentation)?.to_string_lossy(),
                };
                let extra = other.saturating_add(label.capacity() as u64);
                if !label.is_empty() {
                    text.push(ex, " '", extra)?;
                    text.push(ex, &label, other.saturating_add(spare(&label)))?;
                    text.push(ex, "'", extra)?;
                }
                if matches!(check, Value::Bool(true)) {
                    text.push(ex, " +", extra)?;
                }
            } else {
                let part = repr_in_live(ex, &item, depth, text.live())?;
                text.push(ex, &part, backing(&item).saturating_add(spare(&part)))?;
            }
        }
        text.push(ex, if structure || map { "}" } else { "]" }, 0)?;
        Ok(text.value)
    })())
}
fn spare(text: &String) -> u64 {
    text.capacity().saturating_sub(text.len()) as u64
}

fn table(
    ex: &mut Execution,
    value: &Value,
    object: &ObjRef,
    name: &str,
    depth: u32,
    live: u64,
) -> Result<String, Exception> {
    let is_row = name == "СтрокаТаблицыЗначений";
    let owner = if is_row {
        ex.call_method(value, "Владелец", &mut [], true)?
    } else {
        value.clone()
    };
    let mut out = Tree {
        text: Text::new(live.saturating_add(HANDLE_BYTES.saturating_mul(4))),
        frames: Vec::new(),
        names: Vec::new(),
        name_bytes: 0,
    };
    schema(ex, &owner, &mut out)?;
    out.push(ex, name)?;
    out.push(ex, if is_row { "(" } else { "<" })?;
    if !is_row {
        for i in 0..out.names.len() {
            if i != 0 {
                out.push(ex, ", ")?;
            }
            let name = match out.names.get(i) {
                Some(Value::String(s)) => utf8(ex, s, out.live(), false)?,
                _ => String::new(),
            };
            out.owned(ex, &name)?;
        }
        out.push(ex, ">[")?;
    }
    let len = if is_row {
        1
    } else {
        object.iter_len(ex).unwrap_or(0)
    };
    for i in 0..len {
        ex.charge(1)?;
        if i != 0 {
            out.push(ex, ", ")?;
        }
        let row = if is_row {
            value.clone()
        } else {
            object.iter_get(ex, i).map_err(|e| ex.call_error(e))?
        };
        if !is_row {
            out.push(ex, "(")?;
        }
        for j in 0..out.names.len() {
            if j != 0 {
                out.push(ex, ", ")?;
            }
            if is_row {
                let name = match out.names.get(j) {
                    Some(Value::String(s)) => utf8(ex, s, out.live(), false)?,
                    _ => String::new(),
                };
                out.owned(ex, &name)?;
                drop(name);
                out.push(ex, ": ")?;
            }
            let Some(key) = out.names.get(j) else {
                continue;
            };
            let cell = ex.index_get(&row, key)?;
            let part = repr_in_live(ex, &cell, depth, out.live())?;
            out.cell(ex, &part, &cell)?;
        }
        if !is_row {
            out.push(ex, ")")?;
        }
    }
    out.push(ex, if is_row { ")" } else { "]" })?;
    Ok(out.text.value)
}
fn schema(ex: &mut Execution, owner: &Value, out: &mut Tree) -> Result<(), Exception> {
    let columns = ex.get_member(owner, "Колонки")?;
    let object = columns.as_object().ok_or_else(|| ex.out_of_memory())?;
    let len = object.iter_len(ex).unwrap_or(0);
    ex.charge(len as u64)?;
    ex.reserve(
        out.live()
            .saturating_add((len as u64).saturating_mul(std::mem::size_of::<Value>() as u64)),
    )?;
    out.names
        .try_reserve_exact(len)
        .map_err(|_| ex.out_of_memory())?;
    for i in 0..len {
        let column = object.iter_get(ex, i).map_err(|e| ex.call_error(e))?;
        let name = ex.get_member(&column, "Имя")?;
        let bytes = match &name {
            Value::String(s) => (s.backing_capacity_units() as u64).saturating_mul(2),
            _ => 0,
        };
        ex.reserve(out.live().saturating_add(bytes))?;
        out.name_bytes = out.name_bytes.saturating_add(bytes);
        out.names.push(name);
    }
    Ok(())
}
