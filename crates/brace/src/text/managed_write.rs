use super::Doc;
use crate::Node;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};

struct Frame<'a> {
    items: &'a [Node],
    next: usize,
}

pub fn write_doc_budgeted(
    doc: &Doc,
    budget: &MemoryBudget,
    category: Category,
) -> Result<Charged<Vec<u8>>, BudgetError> {
    let mut stack = BudgetedVec::new(budget, category)?;
    let mut length = 0usize;
    visit(doc, &mut stack, |bytes| {
        length = length
            .checked_add(bytes.len())
            .ok_or(BudgetError::Overflow)?;
        Ok(())
    })?;
    let mut output = BudgetedVec::try_with_capacity(budget, category, length)?;
    visit(doc, &mut stack, |bytes| output.try_extend_from_slice(bytes))?;
    Ok(output.into_charged_vec())
}

fn visit<'a>(
    doc: &'a Doc,
    stack: &mut BudgetedVec<Frame<'a>>,
    mut emit: impl FnMut(&[u8]) -> Result<(), BudgetError>,
) -> Result<(), BudgetError> {
    if doc.bom {
        emit(b"\xef\xbb\xbf")?;
    }
    emit(doc.lead.as_bytes())?;
    let mut node = Some(&doc.root);
    let mut first = true;
    loop {
        if let Some(value) = node.take() {
            match value {
                Node::List(items) => {
                    if !first {
                        emit(b"\r\n")?;
                    }
                    first = false;
                    emit(b"{")?;
                    stack.try_push(Frame { items, next: 0 })?;
                }
                Node::Str(text) => {
                    first = false;
                    emit(b"\"")?;
                    let mut parts = text.split('"');
                    if let Some(part) = parts.next() {
                        emit(part.as_bytes())?;
                    }
                    for part in parts {
                        emit(b"\"\"")?;
                        emit(part.as_bytes())?;
                    }
                    emit(b"\"")?;
                }
                Node::Atom(text) => {
                    first = false;
                    emit(text.as_bytes())?;
                }
            }
        }
        let Some(frame) = stack.last_mut() else {
            break;
        };
        if let Some(child) = frame.items.get(frame.next) {
            if frame.next != 0 {
                emit(b",")?;
            }
            frame.next += 1;
            node = Some(child);
        } else {
            if frame.items.last().is_some_and(Node::is_list) {
                emit(b"\r\n")?;
            }
            emit(b"}")?;
            stack.pop();
        }
    }
    emit(doc.trailer.as_bytes())
}
