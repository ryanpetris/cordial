//! Protobuf JSON enum names with numeric preservation for unknown error details.
use serde::{Deserialize, Serialize};

pub(crate) fn is_zero(value: &i32) -> bool {
    *value == 0
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Value<E> {
    Number(i32),
    Known(E),
}

fn serialize<E: TryFrom<i32> + Serialize, S: serde::Serializer>(
    value: &i32,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match E::try_from(*value) {
        Ok(value) => value.serialize(serializer),
        Err(_) => value.serialize(serializer),
    }
}

fn deserialize<'de, E: Deserialize<'de> + Into<i32>, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<i32, D::Error> {
    Ok(match Option::<Value<E>>::deserialize(deserializer)? {
        Some(Value::Number(value)) => value,
        Some(Value::Known(value)) => value.into(),
        None => 0,
    })
}

macro_rules! field {
    ($name:ident, $enum:ty) => {
        pub(crate) mod $name {
            pub(crate) fn serialize<S: serde::Serializer>(
                value: &i32,
                serializer: S,
            ) -> Result<S::Ok, S::Error> {
                super::serialize::<$enum, S>(value, serializer)
            }
            pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> Result<i32, D::Error> {
                super::deserialize::<$enum, D>(deserializer)
            }
        }
    };
}
field!(code, crate::ErrorCode);
field!(reason, crate::ErrorReason);
field!(outcome, crate::ErrorOutcome);
