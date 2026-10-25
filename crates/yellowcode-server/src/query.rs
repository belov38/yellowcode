use query_console::Console;
use std::{io, io::Write, path::PathBuf, sync::Arc};
use yellowcode_core::service::{control, memory};

pub struct QueryConfig {
    pub platform_build: String,
    pub memory: memory::Settings,
    pub base: PathBuf,
}

pub struct QueryProcess {
    _refresh: control::RefreshOwner,
    console: Arc<Console>,
    memory: Arc<memory::Context>,
}

impl QueryProcess {
    pub fn start<L>(
        args: QueryConfig,
        err: &mut dyn Write,
        bind: impl FnOnce() -> io::Result<L>,
    ) -> yellowcode_core::Result<(Self, L)> {
        yellowcode_core::platform::select(&args.platform_build)?;
        let (memory, reserve) = args.memory.resolve()?;
        let memory = Arc::new(memory);
        memory.summary(err)?;
        let (console, refresh, listener) = control::start_service(
            || Ok(Arc::new(memory.open(&args.base)?)),
            control::trim_heap,
            |console| {
                control::RefreshOwner::start(Arc::clone(&memory), console.cache_evictor(), reserve)
            },
            bind,
        )?;
        Ok((
            Self {
                _refresh: refresh,
                console,
                memory,
            },
            listener,
        ))
    }

    pub fn console(&self) -> &Arc<Console> {
        &self.console
    }

    pub fn memory(&self) -> &Arc<memory::Context> {
        &self.memory
    }
}
