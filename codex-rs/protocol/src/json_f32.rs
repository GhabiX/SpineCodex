//! Decode typed JSON f32 values after Serde's tagged/flatten buffering.

use serde::Deserialize;
use serde::Deserializer;
use serde::de::Error;
use serde_json::Number;

pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    let number = Number::deserialize(deserializer)?;
    serde_json::from_str(&number.to_string()).map_err(D::Error::custom)
}
