#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
mod bak_bytes;
pub mod bak_catalog;
pub mod bak_checksum;
pub mod bak_config_source;
pub mod bak_consistency;
pub mod bak_direct;
pub mod bak_idle;
pub mod bak_lob;
pub mod bak_logscan;
pub mod bak_pagemap;
pub mod bak_read;
pub mod bak_record;
pub mod bak_table;
pub mod bak_values;
pub mod config_cf;
pub mod dynamic_update;
pub mod media;
pub mod page_source;
pub mod pages;
pub mod records;
pub mod sql_blob;
pub mod sql_catalog;
pub mod sql_config;
pub mod sql_config_spool;
pub mod sql_dt;
pub mod sql_folder;
pub mod sql_inventory;
pub mod sql_json_row;
pub mod sql_metadata;
pub mod sql_rows;
pub mod sql_schema_wire;
pub mod sql_server;
pub mod sql_support;
pub mod sql_users;
pub mod sql_values;
pub mod sqlcmd;
