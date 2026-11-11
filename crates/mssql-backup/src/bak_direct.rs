use std::{
    cell::RefCell,
    fmt,
    fs::{self, File},
    io::{self, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::{
    bak_catalog::Catalog,
    bak_config_source::{self, Summary},
    bak_consistency::Proof,
    bak_logscan::page_lsn,
    bak_pagemap::{self, Limits, OpenOptions, PageChecks, PageMap},
    bak_read::{self, IntegrityReport, PageReader, ReadSet},
    config_cf::{self, CfOptions},
    dynamic_update,
    media::{self, BackupHeader, HeaderChecks, LogRegion, Lsn},
    sql_config_spool::{self, Spool},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IoStep {
    ReadBackup,
    CheckOutput,
    ProbeOutputDirectory,
    StageConfig,
    CreateStaging,
    WriteCf,
    SyncCf,
    Publish,
}

impl IoStep {
    pub fn is_output(self) -> bool {
        match self {
            IoStep::ReadBackup => false,
            IoStep::CheckOutput
            | IoStep::ProbeOutputDirectory
            | IoStep::StageConfig
            | IoStep::CreateStaging
            | IoStep::WriteCf
            | IoStep::SyncCf
            | IoStep::Publish => true,
        }
    }
}

impl fmt::Display for IoStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            IoStep::ReadBackup => "reading the backup failed",
            IoStep::CheckOutput => "cannot look at the output path",
            IoStep::ProbeOutputDirectory => "cannot create a file in the directory of the output",
            IoStep::StageConfig => "cannot write the temporary file of the configuration",
            IoStep::CreateStaging => "cannot create the file the CF is written to",
            IoStep::WriteCf => "cannot write the CF",
            IoStep::SyncCf => "cannot make the CF durable",
            IoStep::Publish => "cannot put the CF at the output path",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Scope {
    CompressedOrEncrypted,
    ServerVersion {
        found: Option<(u8, u16)>,
        expected: u8,
    },
    ServerTooOld {
        found: Option<(u8, u16)>,
        oldest: u8,
    },
    Adr,
    NoLogBlockChecksums,
    LogWindowNotIdle {
        what: String,
    },
    LogNotProven {
        what: String,
    },
}

impl Scope {
    pub fn code(&self) -> &'static str {
        match self {
            Scope::CompressedOrEncrypted => "compressed_or_encrypted",
            Scope::ServerVersion { .. } | Scope::ServerTooOld { .. } => "sql_server_version",
            Scope::Adr => "adr",
            Scope::NoLogBlockChecksums => "no_log_block_checksums",
            Scope::LogWindowNotIdle { .. } => "log_window_not_idle",
            Scope::LogNotProven { .. } => "log_not_proven",
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::CompressedOrEncrypted => f.write_str(
                "the backup is compressed or encrypted (the file starts with MSSQLBAK); SQL Server reads a compressed backup, and an encrypted one if it has the certificate",
            ),
            Scope::ServerVersion {
                found: Some((major, build)),
                expected,
            } => write!(
                f,
                "the backup was made by SQL Server of major version {major} (build {build}), and the direct reader is verified with major version {expected} only"
            ),
            Scope::ServerVersion {
                found: None,
                expected,
            } => write!(
                f,
                "the backup does not say which SQL Server made it in the form that was checked, and the direct reader is verified with major version {expected} only"
            ),
            Scope::ServerTooOld {
                found: Some((major, build)),
                oldest,
            } => write!(
                f,
                "the backup was made by SQL Server of major version {major} (build {build}), older than the oldest the direct reader reads ({oldest})"
            ),
            Scope::ServerTooOld {
                found: None,
                oldest,
            } => write!(
                f,
                "the backup does not say which SQL Server made it in the form that was checked, and the direct reader reads only backups of major version {oldest} or later"
            ),
            Scope::Adr => f.write_str(
                "the database has accelerated database recovery (ADR) on, and the pages of such a backup can hold rows of a transaction that was rolled back, which only RESTORE removes",
            ),
            Scope::NoLogBlockChecksums => f.write_str(
                "the transaction log of the backup carries no block checksums (the page verification of the database is not CHECKSUM), and the direct reader takes only a log it can verify",
            ),
            Scope::LogWindowNotIdle { what } => write!(
                f,
                "the database was written to while the backup ran (the log window holds more than the records of a checkpoint), and the direct reader takes only backups of a database nobody wrote to: {what}"
            ),
            Scope::LogNotProven { what } => write!(
                f,
                "the transaction log of the backup does not show that the pages are what RESTORE would produce: {what}"
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(
        "the output already exists: {}; the conversion does not replace a file, so choose another path or remove it",
        .0.display()
    )]
    OutputExists(PathBuf),
    #[error("the output path {} does not name a file; give the path of the CF to write", .0.display())]
    OutputPath(PathBuf),
    #[error("cannot read the backup {}: {source}", .path.display())]
    Source { path: PathBuf, source: io::Error },
    #[error("{during}: {source}")]
    Io { during: IoStep, source: io::Error },
    #[error(
        "the direct reader does not read this backup: {reason}; use --server to let SQL Server restore it"
    )]
    Unsupported { reason: String },
    #[error(
        "this backup cannot be converted, by the direct reader or by SQL Server (it must be a full backup of one database in one file, with one data file): {reason}; make such a backup and convert that"
    )]
    NotConvertible { reason: String },
    #[error(
        "the backup is damaged, or is not a whole full backup in one file (a file cut short, say): {what}; get an intact copy of the whole backup, or make a new full backup in one file"
    )]
    Damaged { what: String },
    #[error(
        "page {page} of the backup fails its integrity check: {what}; the file is damaged, or the database uses transparent data encryption (its pages fail this check); get an intact copy of the backup"
    )]
    Corrupt { page: u32, what: String },
    #[error(
        "the direct reader cannot prove that this backup holds what RESTORE would produce (it does not replay the transaction log): {what}; use --server to let SQL Server restore it"
    )]
    NotProven { what: String },
    #[error("the backup is not the database of a 1C infobase: it has no table dbo.{table}")]
    NotAConfigurationStore { table: &'static str },
    #[error("a limit of the direct reader: {what}; use --server to let SQL Server restore it")]
    Limit { what: String },
    #[error("the configuration in the backup cannot be written as a CF: {0}")]
    Config(bak_config_source::Error),
    #[error(
        "the backup was refused by the consistency check of the direct reader: {0}; use --server to let SQL Server restore it"
    )]
    Gate(GateRefusal),
    #[error(
        "the direct reader does not take this backup [{code}]: {0}; use --server to let SQL Server restore it",
        code = .0.code()
    )]
    Scope(Scope),
}

impl Error {
    pub fn suggests_server(&self) -> bool {
        match self {
            Error::Unsupported { .. }
            | Error::NotProven { .. }
            | Error::Limit { .. }
            | Error::Gate(_)
            | Error::Scope(_) => true,
            Error::OutputExists(_)
            | Error::OutputPath(_)
            | Error::Source { .. }
            | Error::Io { .. }
            | Error::NotConvertible { .. }
            | Error::Damaged { .. }
            | Error::Corrupt { .. }
            | Error::NotAConfigurationStore { .. }
            | Error::Config(_) => false,
        }
    }

    fn about(self, context: &str) -> Error {
        match self {
            Error::Unsupported { reason } => Error::Unsupported {
                reason: format!("{context}: {reason}"),
            },
            Error::NotConvertible { reason } => Error::NotConvertible {
                reason: format!("{context}: {reason}"),
            },
            Error::Damaged { what } => Error::Damaged {
                what: format!("{context}: {what}"),
            },
            Error::Corrupt { page, what } => Error::Corrupt {
                page,
                what: format!("{context}: {what}"),
            },
            Error::NotProven { what } => Error::NotProven {
                what: format!("{context}: {what}"),
            },
            Error::Limit { what } => Error::Limit {
                what: format!("{context}: {what}"),
            },
            Error::OutputExists(_)
            | Error::OutputPath(_)
            | Error::Source { .. }
            | Error::Io { .. }
            | Error::NotAConfigurationStore { .. }
            | Error::Config(_)
            | Error::Gate(_)
            | Error::Scope(_) => self,
        }
    }

    fn read_failed(source: io::Error) -> Error {
        Error::Io {
            during: IoStep::ReadBackup,
            source,
        }
    }
}

impl From<media::Error> for Error {
    fn from(error: media::Error) -> Error {
        match error {
            media::Error::Io(source) => Error::read_failed(source),
            media::Error::Invalid(what) => Error::Damaged {
                what: format!("the container of the backup is invalid: {what}"),
            },
            media::Error::Checksum(what) => Error::Damaged {
                what: format!("a checksum of the container does not match: {what}"),
            },
            media::Error::Unsupported(what) => Error::NotConvertible {
                reason: format!("the container of the backup is not one that is read: {what}"),
            },
            media::Error::ResourceLimit(what) => Error::Limit {
                what: format!(
                    "the container of the backup needs more than the reader keeps: {what}"
                ),
            },
        }
    }
}

impl From<bak_pagemap::Error> for Error {
    fn from(error: bak_pagemap::Error) -> Error {
        use bak_pagemap::Error as Map;
        match error {
            Map::Io(source) => Error::read_failed(source),
            Map::Unsupported(reason) => Error::NotConvertible {
                reason: reason.to_owned(),
            },
            Map::TornPage(reason) => Error::Unsupported {
                reason: reason.to_owned(),
            },
            Map::Inconsistent(what) => Error::Damaged {
                what: what.to_owned(),
            },
            Map::Limit(what) => Error::Limit {
                what: format!("{what} (of the page map)"),
            },
            Map::Corrupt { page, what } => Error::Corrupt {
                page,
                what: format!(
                    "a {what} page that the page map reads for itself does not match its checksum or torn-page bits"
                ),
            },
            Map::UnprovenFrame { extent } => Error::NotProven {
                what: format!(
                    "the backup was made while the database was being written to, and extent {extent} was copied a second time; only its page headers say where it belongs"
                ),
            },
            Map::UncopiedExtent { extent } => Error::NotProven {
                what: format!(
                    "the backup lacks pages that the database had allocated (extent {extent}); it was made during an online rebuild of an index, or the file is cut short"
                ),
            },
        }
    }
}

impl From<bak_read::Error> for Error {
    fn from(error: bak_read::Error) -> Error {
        use bak_read::Error as Read;
        match error {
            Read::PageMap(error) => Error::from(error),
            Read::Corrupt { page } => Error::Corrupt {
                page,
                what: "its checksum or torn-page bits do not match its bytes (that check was verified on pages of SQL Server 2014 and 2022 only, so a good page of another version may fail it)".to_owned(),
            },
            Read::WrongId {
                page,
                header_page,
                header_file,
            } => Error::Damaged {
                what: format!("page {page} says it is page {header_file}:{header_page}"),
            },
            Read::WrongType { page, found, .. } => Error::Damaged {
                what: format!("page {page} has type {found}, which is not expected there"),
            },
            Read::Unsupported(reason) => Error::Unsupported {
                reason: reason.to_owned(),
            },
            Read::UnsupportedType { column, type_name } => Error::Unsupported {
                reason: format!("column {column} has the type {type_name}"),
            },
            Read::UnsupportedColumn { column, what } => Error::Unsupported {
                reason: format!("column {column} is not read: {what}"),
            },
            Read::UnsupportedValue { column, what } => Error::Unsupported {
                reason: format!("column {column}: {what}"),
            },
            Read::UnsupportedLobRecord {
                page,
                slot,
                record_type,
            } => Error::Unsupported {
                reason: format!(
                    "record {slot} of page {page} is a large-object record of type {record_type}"
                ),
            },
            Read::Inconsistent(what) => Error::Damaged {
                what: what.to_owned(),
            },
            Read::BadValue { column, what } => Error::Damaged {
                what: format!("column {column}: {what}"),
            },
            Read::Malformed { page, what } => Error::Damaged {
                what: format!("page {page} is malformed: {what}"),
            },
            Read::LeafChain { page, what } => Error::NotProven {
                what: format!(
                    "the leaf pages of a clustered index are not one chain (page {page}): {what}"
                ),
            },
            Read::PageAccounting { page, what } => Error::NotProven {
                what: format!("the figures of page {page} are not consistent: {what}"),
            },
            Read::Limit(what) => Error::Limit {
                what: what.to_owned(),
            },
            Read::Sink(source) => Error::Io {
                during: IoStep::StageConfig,
                source,
            },
            Read::NotFound { schema, name } => Error::Damaged {
                what: format!("the catalog does not list the table {schema}.{name}"),
            },
        }
    }
}

fn spool_io(error: sql_config_spool::Error) -> Result<io::Error, sql_config_spool::Error> {
    use sql_config_spool::Error as Spool;
    match error {
        Spool::Io(source) => Ok(source),
        Spool::Cf(config_cf::Error::Container(v8cont::Error::Io(source))) => Ok(source),
        Spool::DynamicUpdate(dynamic_update::Error::Io(source)) => Ok(source),
        other @ (Spool::Invalid(_)
        | Spool::Part { .. }
        | Spool::Rows(_)
        | Spool::Cf(_)
        | Spool::DynamicUpdate(_)) => Err(other),
    }
}

impl From<bak_config_source::Error> for Error {
    fn from(error: bak_config_source::Error) -> Error {
        use bak_config_source::Error as Source;
        match error {
            Source::Read(error) => Error::from(error),
            Source::MissingTable { table } => Error::NotAConfigurationStore { table },
            Source::ColumnType {
                table,
                column,
                found,
                expected,
            } => Error::Unsupported {
                reason: format!(
                    "column {column} of dbo.{table} is {found}, and the direct reader reads {expected}"
                ),
            },
            Source::AmbiguousNames { .. } => Error::Unsupported {
                reason: "two file names of dbo.Config differ only in trailing spaces, and SQL Server puts such names in an order that the direct reader cannot reproduce".to_owned(),
            },
            Source::TablesDifferOnlyInCase { first, second } => Error::Unsupported {
                reason: format!(
                    "the database has the tables dbo.{first} and dbo.{second}, which differ only in case, and the direct reader cannot tell which of them SQL Server takes for the table of 1C"
                ),
            },
            Source::ColumnsDifferOnlyInCase {
                table,
                first,
                second,
            } => Error::Unsupported {
                reason: format!(
                    "table dbo.{table} has the columns {first} and {second}, which differ only in case, and the direct reader cannot tell which of them SQL Server takes"
                ),
            },
            Source::OtherObject { table, what } => Error::Unsupported {
                reason: format!(
                    "the database has {what}, which SQL Server takes for dbo.{table}, and the direct reader reads only the user table of that name"
                ),
            },
            Source::SecurityPolicy => Error::Unsupported {
                reason: "the database has a security policy (row-level security), which SQL Server applies to the rows it exports and the direct reader does not".to_owned(),
            },
            Source::Limit(what) => Error::Limit {
                what: what.to_owned(),
            },
            Source::Staging(source) => Error::Io {
                during: IoStep::StageConfig,
                source,
            },
            Source::SharedValue {
                part_no,
                other_part_no,
                ..
            } => Error::Unsupported {
                reason: format!(
                    "part {part_no} of a file and part {other_part_no} of another file of dbo.Config point at the same large value, which a real table never has"
                ),
            },
            Source::LobRecords { limit, part_no, .. } => Error::Limit {
                what: format!(
                    "the large values of dbo.Config lead to more than {limit} records in all (reading part {part_no} of a file passed the limit)"
                ),
            },
            Source::Part {
                part_no, source, ..
            } => Error::from(source).about(&format!("part {part_no} of a file")),
            Source::Spool(error) => match spool_io(error) {
                Ok(source) => Error::Io {
                    during: IoStep::StageConfig,
                    source,
                },
                Err(other) => Error::Config(Source::Spool(other)),
            },
            Source::PartRefused {
                file,
                part_no,
                source,
            } => match spool_io(source) {
                Ok(source) => Error::Io {
                    during: IoStep::StageConfig,
                    source,
                },
                Err(source) => Error::Config(Source::PartRefused {
                    file,
                    part_no,
                    source,
                }),
            },
            other @ (Source::MissingColumn { .. }
            | Source::YearOffset(_)
            | Source::Null { .. }
            | Source::NonAsciiName { .. }
            | Source::NameNotText
            | Source::Row { .. }
            | Source::PartTooLarge { .. }
            | Source::PartLength { .. }
            | Source::FileSize { .. }) => Error::Config(other),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct GateRefusal {
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum LayoutCheck {
    Never,
    #[default]
    WhenUnprotected,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LayoutChecked {
    pub extents: u64,
    pub elapsed: Duration,
}

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct DirectOptions {
    pub map_limits: Limits,
    pub open: OpenOptions,
    pub config: bak_config_source::Options,
    pub layout_check: LayoutCheck,
    pub cf: CfOptions,
    pub server_major: Option<u8>,
    pub refuse_adr: bool,
    pub min_server_major: Option<u8>,
    pub idle_window_first: bool,
}

impl Default for DirectOptions {
    fn default() -> Self {
        DirectOptions {
            map_limits: Limits::DEFAULT,
            open: OpenOptions::default(),
            config: bak_config_source::Options::default(),
            layout_check: LayoutCheck::default(),
            cf: CfOptions::default(),
            server_major: None,
            refuse_adr: false,
            min_server_major: None,
            idle_window_first: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MapFacts {
    pub file_pages: u64,
    pub header_size_pages: u32,
    pub region2_extents: Vec<u32>,
    pub unmarked_region2_extents: Vec<u32>,
    pub uncopied_extents: Vec<u32>,
    pub structural_page_checks: PageChecks,
    pub header_checks: HeaderChecks,
    pub layout_checked: Option<LayoutChecked>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BackupFacts {
    pub file_bytes: u64,
    pub log_bytes: u64,
    pub log_read: bool,
    pub checksum_streams: Option<u64>,
    pub set_attributes: Option<u32>,
    pub data_streams: usize,
    pub server_version: Option<(u8, u16)>,
    pub database_collation: Option<u32>,
    pub log_regions: Vec<LogRegion>,
    pub log_regions_complete: bool,
    pub header: Option<BackupHeader>,
    pub accelerated_recovery: bool,
}

pub trait ReadSeek: Read + Seek {}

impl<T: Read + Seek + ?Sized> ReadSeek for T {}

#[derive(Debug, Default)]
pub struct GateOutcome(RefCell<Option<Proof>>);

impl GateOutcome {
    pub fn record(&self, proof: Proof) {
        *self.0.borrow_mut() = Some(proof);
    }

    fn take(&self) -> Option<Proof> {
        self.0.borrow_mut().take()
    }
}

#[non_exhaustive]
pub struct GateFacts<'a> {
    pub read_set: &'a ReadSet,
    pub integrity: IntegrityReport,
    pub map: &'a MapFacts,
    pub backup: &'a BackupFacts,
    pub container: &'a RefCell<dyn ReadSeek + 'a>,
    pub outcome: &'a GateOutcome,
    pub page_lsn: &'a dyn Fn(u32) -> Result<Option<Lsn>, String>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DirectReport {
    pub rows: usize,
    pub config: Summary,
    pub backup: BackupFacts,
    pub map: MapFacts,
    pub integrity: IntegrityReport,
    pub read_set: ReadSet,
    pub dynamic_update: Option<dynamic_update::Summary>,
    pub gate: Option<Proof>,
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn open_source(path: &Path) -> Result<(File, u64), Error> {
    let source_error = |source: io::Error| Error::Source {
        path: path.to_path_buf(),
        source,
    };
    let not_a_file = || {
        source_error(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ))
    };
    if !fs::metadata(path).map_err(source_error)?.is_file() {
        return Err(not_a_file());
    }
    let file = File::open(path).map_err(source_error)?;
    let metadata = file.metadata().map_err(source_error)?;
    if !metadata.is_file() {
        return Err(not_a_file());
    }
    Ok((file, metadata.len()))
}

fn bytes_at<const N: usize>(
    reader: &mut (impl Read + Seek),
    position: u64,
    size: u64,
) -> Option<[u8; N]> {
    if position.checked_add(u64::try_from(N).ok()?)? > size {
        return None;
    }
    reader.seek(SeekFrom::Start(position)).ok()?;
    let mut bytes = [0_u8; N];
    reader.read_exact(&mut bytes).ok()?;
    Some(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContainerWalk {
    checksum_streams: u64,
    set_attributes: Option<u32>,
    collation: Option<u32>,
    server_version: Option<(u8, u16)>,
}

const SET_DIFFERENTIAL: u32 = 0x08;

const CONFIG_MAGIC: &[u8; 4] = b"SCIN";
const CONFIG_LOCALE_AT: usize = 0x74;
const CONFIG_COLLATION_AT: usize = 0xD8;
const CONFIG_PREFIX: usize = CONFIG_COLLATION_AT + 4;

fn database_collation(prefix: &[u8; CONFIG_PREFIX]) -> Option<u32> {
    if prefix.get(..4)? != CONFIG_MAGIC {
        return None;
    }
    let word = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(prefix.get(at..at + 4)?.try_into().ok()?))
    };
    let (locale, id) = (word(CONFIG_LOCALE_AT)?, word(CONFIG_COLLATION_AT)?);
    (id & 0x000F_FFFF == locale).then_some(id)
}

const CONFIG_BUILD_AT: usize = 0x18;
const CONFIG_MAJOR_AT: usize = 0x1B;

fn server_version(prefix: &[u8; CONFIG_PREFIX]) -> Option<(u8, u16)> {
    if prefix.get(..4)? != CONFIG_MAGIC {
        return None;
    }
    let build = u16::from_le_bytes([
        *prefix.get(CONFIG_BUILD_AT)?,
        *prefix.get(CONFIG_BUILD_AT + 1)?,
    ]);
    let major = *prefix.get(CONFIG_MAJOR_AT)?;
    (major != 0).then_some((major, build))
}

const TAPE_FAMILY_COUNT_AT: u64 = 182;
const TAPE_FAMILY_MEMBER_AT: u64 = 186;

fn media_family(reader: &mut (impl Read + Seek), size: u64) -> Option<(u16, u32)> {
    if bytes_at::<4>(reader, 0, size)? != *b"TAPE" {
        return None;
    }
    let count = u16::from_le_bytes(bytes_at::<2>(reader, TAPE_FAMILY_COUNT_AT, size)?);
    let member = u32::from_le_bytes(bytes_at::<4>(reader, TAPE_FAMILY_MEMBER_AT, size)?);
    Some((count, member))
}

const COMPRESSED_MAGIC: &[u8; 8] = b"MSSQLBAK";

fn walk_container(reader: &mut (impl Read + Seek)) -> Option<ContainerWalk> {
    const BLOCK_HEADER: u64 = 52;
    const STREAM_HEADER: u64 = 22;
    const MAX_BLOCKS: usize = 1_000_000;
    const MAX_STREAMS: usize = 1_000_000;
    let size = reader.seek(SeekFrom::End(0)).ok()?;
    let mut position = 0_u64;
    let mut found = 0_u64;
    let mut set_attributes = None;
    let mut collation = None;
    let mut server = None;
    let mut config_seen = false;
    for _ in 0..MAX_BLOCKS {
        let header = bytes_at::<60>(reader, position, size)?;
        let tag: [u8; 4] = header.get(..4)?.try_into().ok()?;
        let first = u64::from(u16::from_le_bytes([*header.get(8)?, *header.get(9)?]));
        if first < BLOCK_HEADER {
            return None;
        }
        if &tag == b"SSET" && set_attributes.is_none() && first >= 56 {
            let word: [u8; 4] = header.get(52..56)?.try_into().ok()?;
            set_attributes = Some(u32::from_le_bytes(word));
        }
        if &tag == b"ESET" {
            return Some(ContainerWalk {
                checksum_streams: found,
                set_attributes,
                collation,
                server_version: server,
            });
        }
        if &tag == b"SFMB" {
            position = position.checked_add(first)?;
            if position >= size {
                return None;
            }
            continue;
        }
        let mut stream = position.checked_add(first)?;
        let mut next = None;
        for _ in 0..MAX_STREAMS {
            let header = bytes_at::<{ STREAM_HEADER as usize }>(reader, stream, size)?;
            let id: [u8; 4] = header.get(..4)?.try_into().ok()?;
            let length = u64::from_le_bytes(header.get(8..16)?.try_into().ok()?);
            if &id == b"CSUM" {
                found += 1;
            }
            let end = stream.checked_add(STREAM_HEADER)?.checked_add(length)?;
            if end > size {
                return None;
            }
            if &id == b"MQCI" && !config_seen {
                config_seen = true;
                if length >= CONFIG_PREFIX as u64
                    && let Some(prefix) =
                        bytes_at::<CONFIG_PREFIX>(reader, stream + STREAM_HEADER, size)
                {
                    collation = database_collation(&prefix);
                    server = server_version(&prefix);
                }
            }
            if &id == b"SPAD" {
                next = Some(end);
                break;
            }
            stream = end.checked_add(3)? & !3;
        }
        position = next?;
        if position >= size {
            return None;
        }
    }
    None
}

fn not_a_database_backup(data_streams: usize, walk: Option<&ContainerWalk>) -> Option<Error> {
    if walk
        .and_then(|walk| walk.set_attributes)
        .is_some_and(|attributes| attributes & SET_DIFFERENTIAL != 0)
    {
        return Some(Error::NotConvertible {
            reason: "this is a differential backup, which holds only the extents that changed since the last full backup".to_owned(),
        });
    }
    let two_passes = walk
        .and_then(|walk| walk.server_version)
        .is_some_and(|(major, _)| major >= FIRST_TWO_PASS_MAJOR);
    (data_streams == 1 && two_passes).then(|| Error::NotConvertible {
        reason: "this is a file, filegroup or partial backup (its data is copied once, where a backup of the whole database made by this version of SQL Server copies it in two passes), not a full backup of the database".to_owned(),
    })
}

const FIRST_TWO_PASS_MAJOR: u8 = 16;

fn open_error(
    error: bak_pagemap::Error,
    container: &mut (impl Read + Seek),
    walk: Option<&ContainerWalk>,
) -> Error {
    if matches!(error, bak_pagemap::Error::UncopiedExtent { .. }) {
        let streams = media::inspect(container)
            .map(|info| info.regions.len())
            .unwrap_or(0);
        if let Some(refusal) = not_a_database_backup(streams, walk) {
            return refusal;
        }
    }
    Error::from(error)
}

trait Staging: Sized {
    fn writer(&mut self) -> &mut dyn Write;
    fn sync(&mut self) -> io::Result<()>;
    fn publish(self, output: &Path) -> io::Result<()>;
}

impl Staging for tempfile::NamedTempFile {
    fn writer(&mut self) -> &mut dyn Write {
        self.as_file_mut()
    }

    fn sync(&mut self) -> io::Result<()> {
        self.as_file().sync_all()
    }

    fn publish(self, output: &Path) -> io::Result<()> {
        self.persist_noclobber(output)
            .map(drop)
            .map_err(|error| error.error)
    }
}

struct Recording<W> {
    inner: W,
    failure: Option<io::Error>,
}

impl<W> Recording<W> {
    fn new(inner: W) -> Recording<W> {
        Recording {
            inner,
            failure: None,
        }
    }

    fn remember(&mut self, error: io::Error) -> io::Error {
        let copy = io::Error::new(error.kind(), error.to_string());
        if error.kind() != io::ErrorKind::Interrupted && self.failure.is_none() {
            self.failure = Some(error);
        }
        copy
    }
}

impl<W: Write> Write for Recording<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.inner.write(data).map_err(|error| self.remember(error))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush().map_err(|error| self.remember(error))
    }
}

fn write_error(error: sql_config_spool::Error, failure: Option<io::Error>) -> Error {
    if let Some(source) = failure {
        return Error::Io {
            during: IoStep::WriteCf,
            source,
        };
    }
    match spool_io(error) {
        Ok(source) => Error::Io {
            during: IoStep::WriteCf,
            source,
        },
        Err(other) => Error::Config(bak_config_source::Error::Spool(other)),
    }
}

fn write_and_publish<S: Staging>(
    spool: &mut Spool,
    cf: &CfOptions,
    create: impl FnOnce() -> io::Result<S>,
    output: &Path,
) -> Result<Option<dynamic_update::Summary>, Error> {
    let mut staging = create().map_err(|source| Error::Io {
        during: IoStep::CreateStaging,
        source,
    })?;
    let dynamic_update;
    {
        let mut writer = BufWriter::new(Recording::new(staging.writer()));
        dynamic_update = match spool.write_cf_with(cf, &mut writer) {
            Ok(summary) => summary,
            Err(error) => return Err(write_error(error, writer.get_mut().failure.take())),
        };
        writer.flush().map_err(|source| Error::Io {
            during: IoStep::WriteCf,
            source,
        })?;
    }
    staging.sync().map_err(|source| Error::Io {
        during: IoStep::SyncCf,
        source,
    })?;
    staging
        .publish(output)
        .map_err(|source| match source.kind() {
            io::ErrorKind::AlreadyExists => Error::OutputExists(output.to_path_buf()),
            _ => Error::Io {
                during: IoStep::Publish,
                source,
            },
        })?;
    Ok(dynamic_update)
}

const STAGING_PREFIX: &str = ".yellowcode-";

fn staging_file(directory: &Path) -> io::Result<tempfile::NamedTempFile> {
    tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempfile_in(directory)
}

fn names_a_file(path: &Path) -> bool {
    path.file_name().is_some()
        && path
            .as_os_str()
            .to_string_lossy()
            .rsplit(std::path::is_separator)
            .next()
            .is_some_and(|last| !matches!(last, "" | "." | ".."))
}

fn check_output(output: &Path) -> Result<(), Error> {
    if !names_a_file(output) {
        return Err(Error::OutputPath(output.to_path_buf()));
    }
    match fs::symlink_metadata(output) {
        Ok(_) => Err(Error::OutputExists(output.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io {
            during: IoStep::CheckOutput,
            source,
        }),
    }
}

pub fn convert_cf_direct<G>(
    source: &Path,
    output: &Path,
    options: &DirectOptions,
    gate: G,
) -> Result<DirectReport, Error>
where
    G: FnOnce(&GateFacts<'_>) -> Result<(), GateRefusal>,
{
    check_output(output)?;
    let staging_dir = parent(output);
    drop(
        tempfile::tempfile_in(staging_dir).map_err(|source| Error::Io {
            during: IoStep::ProbeOutputDirectory,
            source,
        })?,
    );
    let (file, file_bytes) = open_source(source)?;
    let container = file.try_clone().map_err(|error| Error::Source {
        path: source.to_path_buf(),
        source: error,
    })?;
    convert_streams(
        file,
        container,
        file_bytes,
        (output, staging_dir),
        options,
        gate,
        || staging_file(staging_dir),
    )
}

fn convert_streams<R, G, S>(
    pages: R,
    mut container: R,
    file_bytes: u64,
    (output, staging_dir): (&Path, &Path),
    options: &DirectOptions,
    gate: G,
    create_staging: impl FnOnce() -> io::Result<S>,
) -> Result<DirectReport, Error>
where
    R: Read + Seek,
    G: FnOnce(&GateFacts<'_>) -> Result<(), GateRefusal>,
    S: Staging,
{
    if bytes_at::<8>(&mut container, 0, file_bytes).as_ref() == Some(COMPRESSED_MAGIC) {
        return Err(Error::Scope(Scope::CompressedOrEncrypted));
    }
    if let Some((count, member)) = media_family(&mut container, file_bytes)
        && count > 1
    {
        return Err(Error::NotConvertible {
            reason: format!(
                "the backup is striped across several files or devices [striped] (this file is member {member} of {count})"
            ),
        });
    }
    let walk = walk_container(&mut container);
    if let Some(expected) = options.server_major {
        let found = walk.and_then(|walk| walk.server_version);
        if found.map(|(major, _)| major) != Some(expected) {
            return Err(Error::Scope(Scope::ServerVersion { found, expected }));
        }
    }
    if let Some(oldest) = options.min_server_major {
        let found = walk.and_then(|walk| walk.server_version);
        if found.is_some_and(|(major, _)| major < oldest) {
            return Err(Error::Scope(Scope::ServerTooOld { found, oldest }));
        }
    }
    if options.idle_window_first
        && let Ok(early) = media::inspect_checked(&mut container)
    {
        if let Some(refusal) = not_a_database_backup(early.regions.len(), walk.as_ref()) {
            return Err(refusal);
        }
        crate::bak_idle::window_first(&mut container, &early).map_err(Error::Scope)?;
    }
    let map = PageMap::open_with(pages, options.map_limits, options.open)
        .map_err(|error| open_error(error, &mut container, walk.as_ref()))?;
    let media = media::inspect_checked(&mut container)?;
    if let Some(refusal) = not_a_database_backup(media.regions.len(), walk.as_ref()) {
        return Err(refusal);
    }
    if let (Some(oldest), Some(header)) = (options.min_server_major, media.header.as_ref())
        && header.software.major < oldest
    {
        return Err(Error::Scope(Scope::ServerTooOld {
            found: Some((header.software.major, header.software.build)),
            oldest,
        }));
    }
    let collation = walk.and_then(|walk| walk.collation);
    let names = collation.map_or(bak_config_source::Names::Unknown, |id| {
        bak_config_source::Names::from_collation_id(id)
    });
    let mut reader = PageReader::new(map);
    let accelerated_recovery = crate::bak_catalog::accelerated_recovery(&mut reader)?;
    if options.refuse_adr && accelerated_recovery {
        return Err(Error::Scope(Scope::Adr));
    }
    let catalog = Catalog::open(&mut reader)?;
    let tables = bak_config_source::locate_tables_in(&mut reader, &catalog, names)?;
    let mut staged =
        bak_config_source::stage_config(&mut reader, &tables, staging_dir, &options.config)?;
    let (mut page_map, integrity, read_set) = reader.into_parts();

    let backup = BackupFacts {
        file_bytes,
        log_bytes: media.log_bytes,
        log_read: false,
        checksum_streams: walk.map(|walk| walk.checksum_streams),
        set_attributes: walk.and_then(|walk| walk.set_attributes),
        data_streams: media.regions.len(),
        server_version: walk.and_then(|walk| walk.server_version),
        database_collation: collation,
        log_regions: media.log_regions,
        log_regions_complete: media.log_regions_complete,
        header: media.header,
        accelerated_recovery,
    };
    let mut map = MapFacts {
        file_pages: page_map.file_pages(),
        header_size_pages: page_map.header_size_pages(),
        region2_extents: page_map.region2_extents().to_vec(),
        unmarked_region2_extents: page_map.region2_unmarked_extents().to_vec(),
        uncopied_extents: page_map.uncopied_extents().to_vec(),
        structural_page_checks: page_map.structural_page_checks(),
        header_checks: page_map.mtf_header_checks(),
        layout_checked: None,
    };
    let check_layout = match options.layout_check {
        LayoutCheck::Never => false,
        LayoutCheck::WhenUnprotected => map.structural_page_checks.unprotected > 0,
        LayoutCheck::Always => true,
    };
    if check_layout {
        let started = Instant::now();
        let extents = page_map.verify_layout()?;
        map.layout_checked = Some(LayoutChecked {
            extents,
            elapsed: started.elapsed(),
        });
    }
    let pages = RefCell::new(page_map);
    let header_of = |page: u32| -> Result<Option<Lsn>, String> {
        let mut bytes = [0_u8; 8192];
        match pages.borrow_mut().read_page(page, &mut bytes) {
            Ok(Some(_)) => page_lsn(&bytes)
                .map(Some)
                .ok_or_else(|| "the page is cut off".to_owned()),
            Ok(None) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    };
    let container = RefCell::new(container);
    let outcome = GateOutcome::default();
    gate(&GateFacts {
        read_set: &read_set,
        integrity,
        map: &map,
        backup: &backup,
        container: &container,
        outcome: &outcome,
        page_lsn: &header_of,
    })
    .map_err(Error::Gate)?;

    let dynamic_update = write_and_publish(&mut staged.spool, &options.cf, create_staging, output)?;
    Ok(DirectReport {
        rows: staged.summary.parts,
        config: staged.summary,
        backup,
        map,
        integrity,
        read_set,
        dynamic_update,
        gate: outcome.take(),
    })
}
