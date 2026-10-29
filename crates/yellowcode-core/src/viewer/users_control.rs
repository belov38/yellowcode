use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::users::{self, UserWriteError};

pub const PERMISSION_LIFETIME: Duration = Duration::from_secs(15 * 60);
type Identity = (PathBuf, (u64, u64));

pub struct UsersControl {
    pub admission: Arc<RwLock<()>>,
    readers: Arc<super::consoles::ConsoleSlot>,
    code_readers: Arc<super::code_consoles::CodeSlot>,
    source: Option<Identity>,
    grant: Mutex<Grant>,
}
#[derive(Default)]
struct Grant {
    permission: Option<String>,
    issued: Option<Instant>,
    revision: Option<String>,
    closed: bool,
}
impl UsersControl {
    pub(super) fn new(source: &Path) -> Self {
        let readers = super::consoles::slot(source);
        let control = Self {
            admission: readers.admission.clone(),
            readers,
            code_readers: super::code_consoles::code_slot(source),
            source: users::source_identity(source).ok(),
            grant: Mutex::new(Grant::default()),
        };
        control.readers.register_session();
        control
    }
    fn same_source(&self, source: &Path) -> bool {
        self.source.is_some() && users::source_identity(source).ok() == self.source
    }
    pub fn closed(&self) -> bool {
        super::consoles::lock(&self.grant).closed
    }
    pub fn revoke(&self) {
        {
            let mut g = super::consoles::lock(&self.grant);
            if g.closed {
                return;
            }
            g.closed = true;
            g.permission = None;
            g.revision = None;
        }
        self.readers.unregister_session(&self.code_readers);
    }
    fn live(g: &Grant) -> bool {
        !g.closed && g.issued.is_some_and(|t| t.elapsed() < PERMISSION_LIFETIME)
    }
    pub fn permission(&self, source: &Path) -> Option<String> {
        if !self.same_source(source) {
            self.revoke();
            return None;
        }
        let mut g = super::consoles::lock(&self.grant);
        if g.closed || g.issued.is_some_and(|_| !Self::live(&g)) {
            return None;
        }
        if g.permission.is_none() {
            g.permission = Some(
                v8users::new_key()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect(),
            );
            g.issued = Some(Instant::now());
        }
        g.permission.clone()
    }
    pub fn authorize(&self, source: &Path, permission: Option<&str>) -> bool {
        let authorized = {
            let g = super::consoles::lock(&self.grant);
            Self::live(&g) && permission.is_some() && g.permission.as_deref() == permission
        };
        if !authorized {
            return false;
        }
        if !self.same_source(source) {
            self.revoke();
            return false;
        }
        true
    }
    pub fn remember_revision(&self, revision: &str) {
        super::consoles::lock(&self.grant).revision = Some(revision.to_owned());
    }
    pub fn consume_revision(&self, revision: &str) -> Result<(), UserWriteError> {
        let mut g = super::consoles::lock(&self.grant);
        if g.revision.as_deref() != Some(revision) {
            return Err(UserWriteError::StaleRevision);
        }
        g.revision = None;
        Ok(())
    }
}

impl Drop for UsersControl {
    fn drop(&mut self) {
        self.revoke();
    }
}
