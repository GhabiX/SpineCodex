//! Decode typed JSON floats after Serde's tagged/flatten buffering.
//!
//! With `serde_json/arbitrary_precision`, buffered decimal tokens use a private
//! Number map. Read Number here rather than normalizing whole payloads: opaque
//! JSON values must retain their original precision and tagged-field validation.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::Deserializer;
use serde::de::Error;
use serde_json::Number;

pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    into_f64(Number::deserialize(deserializer)?)
}

pub(crate) fn deserialize_map<'de, D>(deserializer: D) -> Result<BTreeMap<String, f64>, D::Error>
where
    D: Deserializer<'de>,
{
    BTreeMap::<String, Number>::deserialize(deserializer)?
        .into_iter()
        .map(|(key, number)| into_f64(number).map(|value| (key, value)))
        .collect()
}

fn into_f64<E: Error>(number: Number) -> Result<f64, E> {
    number
        .as_f64()
        .ok_or_else(|| E::custom("JSON number is outside the finite f64 range"))
}
