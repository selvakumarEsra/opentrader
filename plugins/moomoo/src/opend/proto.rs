/*
 * The OpenD messages the plugin uses (proto ids) and the few enum values it sends. Bodies are protobuf's JSON
 * mapping of OpenD's .proto files: camelCase field names, 64-bit integers as strings.
 */
use serde::{Deserialize, Deserializer, Serialize};

pub const INIT_CONNECT: u32 = 1001;
pub const KEEP_ALIVE: u32 = 1004;
pub const QOT_SUB: u32 = 3001;
pub const QOT_UPDATE_BASIC: u32 = 3005;
pub const QOT_UPDATE_KL: u32 = 3007;
pub const QOT_HISTORY_KL: u32 = 3103;
pub const QOT_STATIC_INFO: u32 = 3202;

pub const PACKET_ENC_NONE: i32 = -1;
pub const PACKET_ENC_AES_CBC: i32 = 2;
pub const SUB_BASIC: i32 = 1;
pub const SUB_KL_1MIN: i32 = 11;
pub const REHAB_FORWARD: i32 = 1;
pub const KL_1MIN: i32 = 1;

/// Messages OpenD sends unasked.
pub fn is_push(proto: u32) -> bool {
    matches!(proto, QOT_UPDATE_BASIC | QOT_UPDATE_KL)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Security {
    pub market: i32,
    pub code: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KLine {
    pub time: String,
    #[serde(default)]
    pub is_blank: bool,
    #[serde(default)]
    pub open_price: f64,
    #[serde(default)]
    pub high_price: f64,
    #[serde(default)]
    pub low_price: f64,
    #[serde(default)]
    pub close_price: f64,
    #[serde(default, deserialize_with = "number")]
    pub volume: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BasicQot {
    pub security: Security,
    pub open_price: f64,
    pub high_price: f64,
    pub low_price: f64,
    pub cur_price: f64,
    pub last_close_price: f64,
    #[serde(deserialize_with = "number")]
    pub volume: f64,
    #[serde(default)]
    pub update_timestamp: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticBasic {
    pub security: Security,
    pub name: String,
    pub sec_type: i32,
    #[serde(default)]
    pub delisting: bool,
}

/// A JSON number, or a 64-bit integer written as a string.
fn number<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum N {
        F(f64),
        S(String),
    }
    match N::deserialize(d)? {
        N::F(f) => Ok(f),
        N::S(s) => s.parse().map_err(serde::de::Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_64_bit_volumes_written_as_strings() {
        let k: KLine = serde_json::from_str(
            r#"{"time":"2025-05-09 09:31:00","isBlank":false,"openPrice":1,"highPrice":2,"lowPrice":0.5,"closePrice":1.5,"volume":"12345"}"#,
        )
        .unwrap();
        assert_eq!((k.volume, k.close_price), (12_345.0, 1.5));
        let k: KLine = serde_json::from_str(r#"{"time":"t","isBlank":true,"volume":7}"#).unwrap();
        assert!(k.is_blank);
        assert_eq!(k.volume, 7.0);
    }
}
