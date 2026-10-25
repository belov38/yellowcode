use super::index::text;
use crate::parts::{Picture, picture_extension};
use brace::Node;
use memory_budget::{BudgetError, Charged, MemoryBudget};
#[derive(Debug)]
pub struct ManagedPicture {
    pub picture: Option<Charged<Picture>>,
    pub miss: Option<&'static str>,
}
impl ManagedPicture {
    fn missing(why: &'static str) -> Self {
        Self {
            picture: None,
            miss: Some(why),
        }
    }
    pub fn decode(budget: &MemoryBudget, entry: &Node) -> Result<Self, BudgetError> {
        let Some(payload) = entry.at(2).and_then(|node| node.at(0)) else {
            return Ok(Self::missing("shape"));
        };
        let payload = payload.at(0).unwrap_or(payload);
        let Some(data) = payload
            .text()
            .map(|body| super::base64::picture_bytes(budget, body))
            .transpose()?
            .flatten()
        else {
            return Ok(Self::missing("base64"));
        };
        let Some(flags) = entry.at(1).and_then(Node::as_list) else {
            return Ok(Self::missing("flags"));
        };
        let item = |at| flags.get(at).and_then(Node::text);
        let Some(load) = item(0) else {
            return Ok(Self::missing("flags"));
        };
        let Some(x) = item(2) else {
            return Ok(Self::missing("flags"));
        };
        let transparent_pixel = if x != "-1" {
            let Some((x, y)) = x
                .parse::<i64>()
                .ok()
                .zip(item(3).and_then(|y| y.parse::<i64>().ok()))
            else {
                return Ok(Self::missing("flags"));
            };
            Some((x, y))
        } else {
            None
        };
        let ext = picture_extension(&data);
        let picture = data
            .try_zip(text(budget, &[ext])?.into_string()?)?
            .map(|(data, ext)| Picture {
                data,
                ext,
                load_transparent: load == "1",
                transparent_pixel,
            });
        Ok(Self {
            picture: Some(picture),
            miss: None,
        })
    }
}
