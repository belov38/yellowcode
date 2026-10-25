use std::collections::HashMap;

use serde_json::{Value, json};

use crate::reader::{LogData, LogRecord};
use crate::{BslDate, Level, Meta, TxStatus};

const NIL_UUID: &str = "00000000-0000-0000-0000-000000000000";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportMeta {
    Null,
    One(String),
    Items(Vec<String>),
}

impl ExportMeta {
    fn to_json(&self) -> Value {
        match self {
            ExportMeta::Null => Value::Null,
            ExportMeta::One(s) => json!(s),
            ExportMeta::Items(v) => match v.as_slice() {
                [one] => json!({ "Item": one }),
                _ => json!({ "Item": v }),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRow {
    pub date: String,
    pub transaction_status: &'static str,
    pub transaction_id: String,
    pub user: String,
    pub user_name: String,
    pub computer: String,
    pub application_name: String,
    pub connection: String,
    pub event: String,
    pub level: &'static str,
    pub comment: String,
    pub metadata: ExportMeta,
    pub metadata_presentation: ExportMeta,
    pub data_presentation: String,
    pub server_name: String,
    pub port: String,
    pub sync_port: String,
    pub session: String,
}

impl ExportRow {
    pub fn fields(&self) -> Vec<(&'static str, Value)> {
        let s = |v: &str| json!(v);
        vec![
            ("Date", s(&self.date)),
            ("TransactionStatus", s(self.transaction_status)),
            ("TransactionID", s(&self.transaction_id)),
            ("User", s(&self.user)),
            ("UserName", s(&self.user_name)),
            ("Computer", s(&self.computer)),
            ("ApplicationName", s(&self.application_name)),
            ("Connection", s(&self.connection)),
            ("Event", s(&self.event)),
            ("Level", s(self.level)),
            ("Comment", s(&self.comment)),
            ("DataPresentation", s(&self.data_presentation)),
            ("Session", s(&self.session)),
            ("Metadata", self.metadata.to_json()),
            ("MetadataPresentation", self.metadata_presentation.to_json()),
            ("ServerName", s(&self.server_name)),
            ("Port", s(&self.port)),
            ("SyncPort", s(&self.sync_port)),
        ]
    }
}

fn status_name(s: TxStatus) -> &'static str {
    match s {
        TxStatus::None => "NotApplicable",
        TxStatus::Unfinished => "Unfinished",
        TxStatus::Committed => "Committed",
        TxStatus::RolledBack => "RolledBack",
    }
}

fn level_name(l: Level) -> &'static str {
    match l {
        Level::Information => "Information",
        Level::Error => "Error",
        Level::Warning => "Warning",
        Level::Note => "Note",
    }
}

fn tx_id(tx: (i64, u64)) -> String {
    if tx == (0, 0) {
        return String::new();
    }
    let secs_ticks = tx.0 / BslDate::TICKS_PER_SECOND * BslDate::TICKS_PER_SECOND;
    let Some(d) = BslDate::from_ticks(secs_ticks) else {
        return String::new();
    };
    let p = d.parts();
    format!(
        "{:02}.{:02}.{} {}:{:02}:{:02} ({})",
        p.day, p.month, p.year, p.hour, p.minute, p.second, tx.1
    )
}

fn is_array_form(r: &LogRecord) -> bool {
    r.event.event == "_$Access$_.AccessDenied"
}

pub fn export_rows(log: &LogData) -> Vec<ExportRow> {
    let letters: HashMap<(i64, u64), TxStatus> = log
        .records
        .iter()
        .filter(|r| r.tx == (r.event.time.trunc_seconds().ticks(), r.offset))
        .map(|r| (r.tx, r.status))
        .collect();
    log.records
        .iter()
        .map(|r| {
            let e = &r.event;
            let status = match r.status {
                TxStatus::Unfinished => match letters.get(&r.tx) {
                    Some(s @ (TxStatus::Committed | TxStatus::RolledBack)) => *s,
                    _ => TxStatus::Unfinished,
                },
                s => s,
            };
            let p = e.time.parts();
            let (metadata, metadata_presentation) = match &e.metadata {
                Meta::One(m) => (
                    ExportMeta::One(m.uuid.to_string()),
                    ExportMeta::One(m.name.clone()),
                ),
                Meta::Many(v) if v.is_empty() => (ExportMeta::Null, ExportMeta::Null),
                Meta::Many(v) => {
                    let names: Vec<String> = v.iter().map(|m| m.name.clone()).collect();
                    (ExportMeta::Items(names.clone()), ExportMeta::Items(names))
                }
                Meta::None if is_array_form(r) => (ExportMeta::Null, ExportMeta::Null),
                Meta::None => (
                    ExportMeta::One(NIL_UUID.into()),
                    ExportMeta::One(format!("<Не определено {NIL_UUID}>")),
                ),
            };
            let (user, user_name) = match &e.user {
                Some(u) => (u.uuid.to_string(), u.name.clone()),
                None => (NIL_UUID.into(), String::new()),
            };
            ExportRow {
                date: format!(
                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                    p.year, p.month, p.day, p.hour, p.minute, p.second
                ),
                transaction_status: status_name(status),
                transaction_id: tx_id(r.tx),
                user,
                user_name,
                computer: e.computer.clone(),
                application_name: e.app.clone(),
                connection: e.connection.to_string(),
                event: e.event.clone(),
                level: level_name(e.level),
                comment: e.comment.clone(),
                metadata,
                metadata_presentation,
                data_presentation: e.data_presentation.clone(),
                server_name: e.server.clone(),
                port: e.main_port.to_string(),
                sync_port: e.aux_port.to_string(),
                session: e.session.to_string(),
            }
        })
        .collect()
}
