use crate::{Error, data, layout::Layout, user::UserData};
use brace::Node;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Role {
    pub id: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Authentication {
    pub standard: bool,
    pub os: bool,
    pub openid: bool,
    pub openid_connect: bool,
    pub access_token: bool,
    pub qr_code: bool,
    pub email: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Separation {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Card {
    pub id: String,
    pub name: String,
    pub full_name: String,
    pub roles: Vec<Role>,
    pub default_interface: String,
    pub language: String,
    pub authentication: Authentication,
    pub os_user: String,
    pub show_in_list: bool,
    pub password_set: bool,
    pub cannot_change_password: bool,
    pub cannot_recover_password: bool,
    pub unsafe_operation_protection: bool,
    pub second_factor_processing: u32,
    pub run_mode: u32,
    pub role_set: u32,
    pub changed_local: String,
    pub data_separation: Vec<Separation>,
    pub email: String,
    pub password_policy: String,
    pub password_changed_utc: String,
    pub password_hash_algorithm: u32,
    pub password_does_not_satisfy_requirements: bool,
}

impl Card {
    pub fn from_blob(blob: &[u8]) -> Result<Self, Error> {
        UserData::parse(&data::decode(blob)?.text)?.card()
    }

    pub fn is_service(&self) -> bool {
        self.id == crate::user::SERVICE_ROW_ID
    }
}

impl UserData {
    pub fn card(&self) -> Result<Card, Error> {
        let fields = self.tree.as_list().ok_or(Error::NotList)?;
        let layout = Layout::read(fields)?;
        let at = |i| layout.at(fields, i);
        let text = |i| at(i).and_then(Node::text).unwrap_or_default().to_owned();
        let flag = |i, default| at(i).and_then(Node::as_bool).unwrap_or(default);
        let num = |i, default| at(i).and_then(Node::as_u32).unwrap_or(default);
        let mut data_separation = Vec::new();
        if let Some(count_node) = at(17) {
            let count = num(17, 0) as usize;
            let start = fields
                .iter()
                .position(|n| std::ptr::eq(n, count_node))
                .ok_or(Error::BadField(17))?
                + 1;
            for pair in fields
                .get(start..start + count)
                .ok_or(Error::BadField(17))?
            {
                data_separation.push(Separation {
                    name: pair
                        .at(0)
                        .and_then(Node::as_str)
                        .ok_or(Error::BadField(17))?
                        .to_owned(),
                    value: pair
                        .at(1)
                        .and_then(Node::as_str)
                        .ok_or(Error::BadField(17))?
                        .to_owned(),
                });
            }
        }
        let os_hex = text(9);
        let bytes: Result<Vec<u8>, _> = os_hex
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| {
                std::str::from_utf8(p)
                    .ok()
                    .and_then(|p| u8::from_str_radix(p, 16).ok())
                    .ok_or(Error::BadField(9))
            })
            .collect();
        let os_user = String::from_utf8(bytes?).map_err(|_| Error::BadField(9))?;
        Ok(Card {
            id: self.id().ok_or(Error::BadField(0))?.to_string(),
            name: text(1),
            full_name: text(3),
            roles: self
                .roles()
                .iter()
                .map(|id| Role {
                    id: id.to_string(),
                    name: None,
                })
                .collect(),
            default_interface: text(4),
            language: text(6),
            authentication: Authentication {
                standard: flag(7, false),
                os: flag(10, false),
                openid: flag(18, false),
                openid_connect: flag(23, true),
                access_token: flag(26, false),
                qr_code: flag(34, false),
                email: flag(36, false),
            },
            os_user,
            show_in_list: flag(8, false),
            password_set: self.password_set(),
            cannot_change_password: flag(11, false),
            cannot_recover_password: flag(24, true),
            unsafe_operation_protection: flag(20, true),
            second_factor_processing: num(22, 0),
            run_mode: num(14, 0),
            role_set: num(15, 0),
            changed_local: text(16),
            data_separation,
            email: text(25),
            password_policy: text(28),
            password_changed_utc: text(29),
            password_hash_algorithm: num(32, 0),
            password_does_not_satisfy_requirements: flag(35, false),
        })
    }
}
