//! Preserve malformed evidence: serde_json::Value alone silently replaces duplicate keys.
use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

/// Share the existing recorder's 64 MiB memory reservation between source and decoded data.
/// Eight Value slots plus four pointers per node conservatively cover BTree occupancy, Vec
/// spare capacity, retained/typed/temporary copies. Strings reserve eight byte copies, covering
/// Header/typed-init validation round trips. Source bytes are charged three times: the exact
/// read buffer plus up to twice its size for geometric growth of JSON escape scratch.
/// Maps also reserve 32 key/value slots for initial BTree nodes and temporary copies; frames
/// reserve four struct slots for Vec's minimum/growth capacity. Ordinals never index storage.
pub(super) struct Budget(std::cell::Cell<usize>);
impl Budget {
    pub(super) fn new(bytes: usize) -> Self {
        Self(std::cell::Cell::new(bytes))
    }
    pub(super) fn charge(&self, bytes: usize) -> Result<(), &'static str> {
        let left = self
            .0
            .get()
            .checked_sub(bytes)
            .ok_or("decoded recording budget exceeded")?;
        self.0.set(left);
        Ok(())
    }
    fn string(&self, bytes: usize) -> Result<(), &'static str> {
        self.charge(
            bytes
                .checked_mul(8)
                .ok_or("decoded recording budget exceeded")?,
        )
    }
}
struct Checked<'a>(&'a Budget);
struct Key<'a>(&'a Budget);
impl<'de> DeserializeSeed<'de> for Key<'_> {
    type Value = String;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<String, D::Error> {
        deserializer.deserialize_str(self)
    }
}
impl<'de> Visitor<'de> for Key<'_> {
    type Value = String;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded JSON key")
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<String, E> {
        self.0.string(value.len()).map_err(E::custom)?;
        Ok(value.to_owned())
    }
}
impl<'de> DeserializeSeed<'de> for Checked<'_> {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        self.0
            .charge(8 * std::mem::size_of::<Value>() + 4 * std::mem::size_of::<usize>())
            .map_err(serde::de::Error::custom)?;
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Checked<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unique-key JSON")
    }
    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Value, E> {
        Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("nonfinite JSON number"))
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Value, E> {
        self.0.string(v.len()).map_err(E::custom)?;
        Ok(Value::String(v.into()))
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Value, E> {
        self.0.string(v.len()).map_err(E::custom)?;
        Ok(Value::String(v))
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(Checked(self.0))? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        self.0
            .charge(32 * std::mem::size_of::<(String, Value)>())
            .map_err(serde::de::Error::custom)?;
        let mut values = Map::new();
        while let Some(key) = map.next_key_seed(Key(self.0))? {
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom("duplicate JSON key"));
            }
            let value = map.next_value_seed(Checked(self.0))?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}
pub(super) fn decode_with(bytes: &[u8], budget: &Budget) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = Checked(budget).deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}
pub(super) fn decode(bytes: &[u8]) -> Result<Value, serde_json::Error> {
    let budget = Budget::new(super::CAP_BYTES);
    budget
        .charge(bytes.len().saturating_mul(3))
        .map_err(<serde_json::Error as serde::de::Error>::custom)?;
    decode_with(bytes, &budget)
}
