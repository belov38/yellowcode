use super::super::{Cx, MemBudget, OwnedCharge};
use crate::model::{invalid_size, size_add, size_block, size_vec};
use crate::source::Row;
use query_ir::{ColumnInfo, ExecError, QValue};
use std::alloc::{Layout, alloc};
use std::mem::{size_of, take};
use std::ops::{Deref, DerefMut};

pub(super) struct PaidVec<T> {
    values: Vec<T>,
    charge: OwnedCharge,
}

impl<T> PaidVec<T> {
    pub(super) fn new(capacity: usize, mem: &MemBudget<'_>) -> Result<Self, ExecError> {
        let layout = Layout::array::<T>(capacity).map_err(|_| invalid_size())?;
        let charge = mem.hold_owned(size_vec(capacity, size_of::<T>())?)?;
        let values = if layout.size() == 0 {
            Vec::new()
        } else {
            let pointer = unsafe { alloc(layout) }.cast::<T>();
            if pointer.is_null() {
                return Err(super::fatal("accounting buffer allocation failed"));
            }
            unsafe { Vec::from_raw_parts(pointer, 0, capacity) }
        };
        Ok(Self { values, charge })
    }

    pub(super) fn reserve_one(&mut self, mem: &MemBudget<'_>) -> Result<(), ExecError> {
        if self.values.len() == self.values.capacity() {
            let capacity = self
                .values
                .capacity()
                .checked_mul(2)
                .and_then(|n| n.checked_add(1))
                .ok_or_else(invalid_size)?;
            let mut next = Self::new(capacity, mem)?;
            next.values.append(&mut self.values);
            std::mem::swap(self, &mut next);
            drop(next);
        }
        Ok(())
    }

    pub(super) fn push(&mut self, value: T, mem: &MemBudget<'_>) -> Result<(), ExecError> {
        self.reserve_one(mem)?;
        self.values.push(value);
        Ok(())
    }

    pub(super) fn into_parts(self) -> (Vec<T>, OwnedCharge) {
        (self.values, self.charge)
    }
}
impl<T> Deref for PaidVec<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        &self.values
    }
}
impl<T> DerefMut for PaidVec<T> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        &mut self.values
    }
}

enum Frame<'a> {
    Value(&'a QValue),
    Values(std::slice::Iter<'a, QValue>),
    Rows(std::slice::Iter<'a, Row>),
    Columns(std::slice::Iter<'a, ColumnInfo>),
}

fn type_backing(ty: &query_ir::QType) -> Result<u64, ExecError> {
    size_add(
        size_vec(ty.refs.capacity(), size_of::<query_ir::RefAtom>())?,
        size_vec(ty.others.capacity(), size_of::<query_ir::Other>())?,
    )
}

pub(super) fn row_backing(row: &Row, cx: &Cx<'_>) -> Result<u64, ExecError> {
    let mut bytes = size_vec(row.capacity(), size_of::<QValue>())?;
    let mut pending = PaidVec::new(1, &cx.mem)?;
    let mut seen = PaidVec::<(u8, usize)>::new(0, &cx.mem)?;
    pending.push(Frame::Values(row.iter()), &cx.mem)?;
    while let Some(frame) = pending.pop() {
        super::super::accounting::check_cancel(cx.env)?;
        match frame {
            Frame::Values(mut values) => {
                if let Some(value) = values.next() {
                    pending.push(Frame::Values(values), &cx.mem)?;
                    pending.push(Frame::Value(value), &cx.mem)?;
                }
            }
            Frame::Rows(mut rows) => {
                if let Some(row) = rows.next() {
                    bytes = size_add(bytes, size_vec(row.capacity(), size_of::<QValue>())?)?;
                    pending.push(Frame::Rows(rows), &cx.mem)?;
                    pending.push(Frame::Values(row.iter()), &cx.mem)?;
                }
            }
            Frame::Columns(mut columns) => {
                if let Some(column) = columns.next() {
                    bytes = size_add(bytes, size_block(column.name.capacity() as u64)?)?;
                    bytes = size_add(bytes, type_backing(&column.ty)?)?;
                    pending.push(Frame::Columns(columns), &cx.mem)?;
                    if let Some(nested) = &column.nested {
                        bytes =
                            size_add(bytes, size_vec(nested.capacity(), size_of::<ColumnInfo>())?)?;
                        pending.push(Frame::Columns(nested.iter()), &cx.mem)?;
                    }
                }
            }
            Frame::Value(value) => {
                let shared = match value {
                    QValue::String(s) => Some((0, s.0.backing_layout())),
                    QValue::Binary(b) | QValue::Storage(b) => Some((1, b.backing_layout())),
                    QValue::Nested(n) => Some((2, n.backing_layout())),
                    _ => None,
                };
                if let Some((kind, backing)) = shared {
                    let backing = backing.map_err(|_| invalid_size())?;
                    let identity = (kind, backing.identity);
                    if seen.contains(&identity) {
                        continue;
                    }
                    seen.push(identity, &cx.mem)?;
                    bytes = size_add(bytes, size_block(backing.node.size() as u64)?)?;
                    if let Some(buffer) = backing.buffer {
                        bytes = size_add(bytes, size_block(buffer.size() as u64)?)?;
                    }
                    if let QValue::Nested(nested) = value {
                        bytes = size_add(
                            bytes,
                            size_vec(nested.columns.capacity(), size_of::<ColumnInfo>())?,
                        )?;
                        bytes =
                            size_add(bytes, size_vec(nested.rows.capacity(), size_of::<Row>())?)?;
                        pending.push(Frame::Columns(nested.columns.iter()), &cx.mem)?;
                        pending.push(Frame::Rows(nested.rows.iter()), &cx.mem)?;
                    }
                } else {
                    bytes = size_add(
                        bytes,
                        match value {
                            QValue::Number(n) => {
                                size_vec(n.backing_capacity_limbs(), size_of::<u32>())?
                            }
                            QValue::Type(t) => type_backing(t)?,
                            QValue::PointInTime(_) => {
                                size_block(size_of::<query_ir::value::Moment>() as u64)?
                            }
                            QValue::Boundary(b) => {
                                pending.push(Frame::Value(&b.point), &cx.mem)?;
                                size_block(size_of::<query_ir::value::Boundary>() as u64)?
                            }
                            _ => 0,
                        },
                    )?;
                }
                cx.mem.check_value(bytes)?;
            }
        }
    }
    cx.mem.check_value(bytes)?;
    Ok(bytes)
}

pub(super) struct PaidRow {
    pub(super) row: Row,
    _charge: OwnedCharge,
}
impl PaidRow {
    pub(super) fn import(row: Row, cx: &Cx<'_>) -> Result<Self, ExecError> {
        let bytes = row_backing(&row, cx)?;
        let charge = cx.mem.hold_owned(bytes)?;
        Ok(Self {
            row,
            _charge: charge,
        })
    }
}

pub(super) fn output(mut row: PaidVec<QValue>, cx: &Cx<'_>) -> Result<Row, ExecError> {
    let bytes = row_backing(&row.values, cx)?;
    cx.mem.grow_scratch(bytes)?;
    let values = take(&mut row.values);
    drop(row);
    Ok(values)
}
