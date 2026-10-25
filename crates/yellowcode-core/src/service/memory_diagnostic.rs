use memory_budget::{Category, Snapshot};
use onecd::ReaderStats;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub struct Record {
    pub id: u64,
    pub status: u16,
    pub reason: &'static str,
    pub transport: &'static str,
    pub wire_bytes: u64,
    pub elapsed_ms: u128,
    pub memory: Snapshot,
    pub reader: ReaderStats,
}
pub fn resource_reason(mut error: &(dyn std::error::Error + 'static)) -> Option<&'static str> {
    loop {
        if error.is::<memory_budget::ResponseLimit>() {
            return Some("memory_response");
        }
        if let Some(error) = error.downcast_ref::<memory_budget::BudgetError>() {
            return Some(match error {
                memory_budget::BudgetError::AllocationFailed => "allocation_failed",
                memory_budget::BudgetError::Overflow => "memory_overflow",
                memory_budget::BudgetError::Limit(limit)
                    if limit.reason == memory_budget::LimitReason::Global =>
                {
                    "memory_global"
                }
                memory_budget::BudgetError::Limit(limit) => match limit.category {
                    Category::Metadata | Category::ReaderWorking => "memory_global",
                    Category::Cache => "memory_cache",
                    Category::Execution => "memory_execution",
                    Category::Result => "memory_result",
                    Category::Row => "memory_row",
                    Category::Input => "memory_input",
                    Category::Response => "memory_response",
                },
            });
        }
        error = error.source()?;
    }
}

struct Line {
    bytes: [u8; 4096],
    len: usize,
}
impl Write for Line {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .len
            .checked_add(bytes.len())
            .filter(|end| *end <= self.bytes.len())
            .ok_or(io::ErrorKind::FileTooLarge)?;
        self.bytes
            .get_mut(self.len..end)
            .ok_or(io::ErrorKind::InvalidInput)?
            .copy_from_slice(bytes);
        self.len = end;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Record {
    pub fn write(&self, output: &mut (impl Write + ?Sized)) -> io::Result<()> {
        let mut line = Line {
            bytes: [0; 4096],
            len: 0,
        };
        write!(
            line,
            "memory_request id={} status={} reason={} transport={} wire_bytes={} elapsed_ms={} live={} peak={} cap={} over_budget={}",
            self.id,
            self.status,
            self.reason,
            self.transport,
            self.wire_bytes,
            self.elapsed_ms,
            self.memory.current,
            self.memory.peak,
            self.memory.admission,
            self.memory.over_budget
        )?;
        for (category, state) in Category::ALL.into_iter().zip(self.memory.categories) {
            write!(
                line,
                " {}_live={} {}_peak={} {}_cap={}",
                category.as_str(),
                state.current,
                category.as_str(),
                state.peak,
                category.as_str(),
                state.limit
            )?;
        }
        writeln!(
            line,
            " cache_hit={} cache_miss={} cache_evict={} cache_pages={} cache_bytes={} pinned_pages={} pinned_bytes={} reader_working_bytes={}",
            self.reader.hits,
            self.reader.misses,
            self.reader.evictions,
            self.reader.cache_pages,
            self.reader.cache_payload_bytes,
            self.reader.pinned_cache_pages,
            self.reader.pinned_cache_payload_bytes,
            self.reader.reader_working_bytes
        )?;
        output.write_all(
            line.bytes
                .get(..line.len)
                .ok_or(io::ErrorKind::InvalidInput)?,
        )
    }
}
