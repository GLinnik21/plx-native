//! Preserve malformed evidence: serde_json::Value alone silently replaces duplicate keys.
//!
//! **Shared (`pub mod`).** The recorder reads [`Value`] through `decode`/`decode_with` (a duplicate
//! key is an error). `appkit::hud_format::parse` reads [`Ordered`] through [`decode_ordered`]: the
//! same visitor and the same budget charges, but an object keeps DOCUMENT ORDER and KEEPS DUPLICATES,
//! so the HUD package parser can name the duplicate key's own JSON pointer, and can honour "the first
//! condition in document order wins" for a state-keyed value. (`serde_json::Map` is a `BTreeMap` in
//! this build, so a `Value` loses both.) There is no second duplicate-key reader in the tree.
use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use std::marker::PhantomData;

/// Re-exported so a crate that cannot name `serde_json` (appkit has no such dependency) can hold
/// the types this module returns.
pub use serde_json::{Error, Map, Number, Value};

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
/// What the one visitor builds. `Obj` is the object under construction; `has_key` is the duplicate
/// check made BEFORE a key's value is read (`Value` refuses a repeat, [`Ordered`] never does).
trait Tree: Sized {
    type Obj;
    fn null() -> Self;
    fn boolean(v: bool) -> Self;
    fn number(v: Number) -> Self;
    fn string(v: String) -> Self;
    fn array(v: Vec<Self>) -> Self;
    fn new_obj() -> Self::Obj;
    fn has_key(obj: &Self::Obj, key: &str) -> bool;
    fn put(obj: &mut Self::Obj, key: String, value: Self);
    fn object(obj: Self::Obj) -> Self;
}
impl Tree for Value {
    type Obj = Map<String, Value>;
    fn null() -> Self {
        Value::Null
    }
    fn boolean(v: bool) -> Self {
        Value::Bool(v)
    }
    fn number(v: Number) -> Self {
        Value::Number(v)
    }
    fn string(v: String) -> Self {
        Value::String(v)
    }
    fn array(v: Vec<Self>) -> Self {
        Value::Array(v)
    }
    fn new_obj() -> Self::Obj {
        Map::new()
    }
    fn has_key(obj: &Self::Obj, key: &str) -> bool {
        obj.contains_key(key)
    }
    fn put(obj: &mut Self::Obj, key: String, value: Self) {
        obj.insert(key, value);
    }
    fn object(obj: Self::Obj) -> Self {
        Value::Object(obj)
    }
}

/// A JSON tree whose objects keep DOCUMENT ORDER and KEEP DUPLICATE KEYS (see the module doc).
#[derive(Clone, Debug, PartialEq)]
pub enum Ordered {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Ordered>),
    /// `(key, value)` pairs in the order the document wrote them; a repeated key appears repeatedly.
    Object(Vec<(String, Ordered)>),
}
impl Tree for Ordered {
    type Obj = Vec<(String, Ordered)>;
    fn null() -> Self {
        Ordered::Null
    }
    fn boolean(v: bool) -> Self {
        Ordered::Bool(v)
    }
    fn number(v: Number) -> Self {
        Ordered::Number(v)
    }
    fn string(v: String) -> Self {
        Ordered::String(v)
    }
    fn array(v: Vec<Self>) -> Self {
        Ordered::Array(v)
    }
    fn new_obj() -> Self::Obj {
        Vec::new()
    }
    fn has_key(_obj: &Self::Obj, _key: &str) -> bool {
        false
    }
    fn put(obj: &mut Self::Obj, key: String, value: Self) {
        obj.push((key, value));
    }
    fn object(obj: Self::Obj) -> Self {
        Ordered::Object(obj)
    }
}
impl Ordered {
    /// The FIRST value written under `key`, when this is an object (a later duplicate is not seen).
    pub fn get(&self, key: &str) -> Option<&Ordered> {
        match self {
            Ordered::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    /// The JSON kind, for a "wrong type" message: `null`, `boolean`, `number`, `string`, `array`, `object`.
    pub fn type_name(&self) -> &'static str {
        match self {
            Ordered::Null => "null",
            Ordered::Bool(_) => "boolean",
            Ordered::Number(_) => "number",
            Ordered::String(_) => "string",
            Ordered::Array(_) => "array",
            Ordered::Object(_) => "object",
        }
    }
    pub fn as_object(&self) -> Option<&[(String, Ordered)]> {
        match self {
            Ordered::Object(pairs) => Some(pairs),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Ordered]> {
        match self {
            Ordered::Array(items) => Some(items),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Ordered::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Ordered::Bool(b) => Some(*b),
            _ => None,
        }
    }
    /// An integer that fits `i64` (so `1.0` and `2^63` are not integers here, as in `Number::as_i64`).
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Ordered::Number(n) => n.as_i64(),
            _ => None,
        }
    }
    /// Any number as `f64` (an integer converts).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Ordered::Number(n) => n.as_f64(),
            _ => None,
        }
    }
}

struct Checked<'a, T>(&'a Budget, PhantomData<T>);
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
impl<'de, T: Tree> DeserializeSeed<'de> for Checked<'_, T> {
    type Value = T;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<T, D::Error> {
        self.0
            .charge(8 * std::mem::size_of::<T>() + 4 * std::mem::size_of::<usize>())
            .map_err(serde::de::Error::custom)?;
        deserializer.deserialize_any(self)
    }
}
impl<'de, T: Tree> Visitor<'de> for Checked<'_, T> {
    type Value = T;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unique-key JSON")
    }
    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<T, E> {
        Ok(T::boolean(v))
    }
    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<T, E> {
        Ok(T::number(v.into()))
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<T, E> {
        Ok(T::number(v.into()))
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<T, E> {
        Number::from_f64(v)
            .map(T::number)
            .ok_or_else(|| E::custom("nonfinite JSON number"))
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<T, E> {
        self.0.string(v.len()).map_err(E::custom)?;
        Ok(T::string(v.into()))
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<T, E> {
        self.0.string(v.len()).map_err(E::custom)?;
        Ok(T::string(v))
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<T, E> {
        Ok(T::null())
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<T, E> {
        Ok(T::null())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<T, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(Checked::<T>(self.0, PhantomData))? {
            values.push(value);
        }
        Ok(T::array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<T, A::Error> {
        self.0
            .charge(32 * std::mem::size_of::<(String, T)>())
            .map_err(serde::de::Error::custom)?;
        let mut values = T::new_obj();
        while let Some(key) = map.next_key_seed(Key(self.0))? {
            if T::has_key(&values, &key) {
                return Err(serde::de::Error::custom("duplicate JSON key"));
            }
            let value = map.next_value_seed(Checked::<T>(self.0, PhantomData))?;
            T::put(&mut values, key, value);
        }
        Ok(T::object(values))
    }
}
fn decode_tree<T: Tree>(bytes: &[u8], budget: &Budget) -> Result<T, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = Checked::<T>(budget, PhantomData).deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}
pub(super) fn decode_with(bytes: &[u8], budget: &Budget) -> Result<Value, serde_json::Error> {
    decode_tree::<Value>(bytes, budget)
}
pub(super) fn decode(bytes: &[u8]) -> Result<Value, serde_json::Error> {
    let budget = Budget::new(super::CAP_BYTES);
    budget
        .charge(bytes.len().saturating_mul(3))
        .map_err(<serde_json::Error as serde::de::Error>::custom)?;
    decode_with(bytes, &budget)
}
/// Decode one JSON document into an [`Ordered`] tree: objects in document order, duplicate keys kept.
/// `cap` is the decoded-memory budget in bytes (the recorder's is `CAP_BYTES`); the source is charged
/// three times first, exactly as `decode` does, and the whole input must be one value (trailing
/// bytes are an error). serde_json's own nesting limit (128) bounds the depth.
pub fn decode_ordered(bytes: &[u8], cap: usize) -> Result<Ordered, serde_json::Error> {
    let budget = Budget::new(cap);
    budget
        .charge(bytes.len().saturating_mul(3))
        .map_err(<serde_json::Error as serde::de::Error>::custom)?;
    decode_tree::<Ordered>(bytes, &budget)
}

#[cfg(test)]
mod freeze_tests {
    use super::*;

    #[test]
    fn ordered_keeps_document_order_and_duplicates() {
        let doc = br#"{"b":1,"a":{"x":true,"x":false},"b":2,"list":[null,"s",1.5]}"#;
        let tree = decode_ordered(doc, 1 << 20).unwrap();
        let keys: Vec<&str> = tree
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(keys, ["b", "a", "b", "list"]);
        assert_eq!(
            tree.get("b").and_then(Ordered::as_i64),
            Some(1),
            "get is the FIRST match"
        );
        let inner = tree.get("a").unwrap().as_object().unwrap();
        assert_eq!(
            inner.len(),
            2,
            "a duplicate inside a nested object is kept too"
        );
        let list = tree.get("list").unwrap().as_array().unwrap();
        assert_eq!(list[0], Ordered::Null);
        assert_eq!(list[1].as_str(), Some("s"));
        assert_eq!(list[2].as_f64(), Some(1.5));
        assert_eq!(list[2].as_i64(), None);
        assert_eq!(tree.type_name(), "object");
        assert_eq!(list[0].type_name(), "null");
    }

    #[test]
    fn value_still_refuses_a_duplicate_key_and_ordered_still_charges_its_budget() {
        let dup = br#"{"a":1,"a":2}"#;
        assert!(
            decode(dup).is_err(),
            "the recorder's Value path rejects a duplicate key"
        );
        assert!(decode_ordered(dup, 1 << 20).is_ok());
        assert!(decode(br#"{"a":1,"b":[true,false]}"#).is_ok());
        // The source is charged three times, then every node: a 64-byte budget cannot hold anything.
        assert!(decode_ordered(br#"{"a":1}"#, 64).is_err());
        // One value and nothing after it.
        assert!(decode_ordered(br#"{} x"#, 1 << 20).is_err());
        assert!(decode_ordered(b"", 1 << 20).is_err());
    }

    #[test]
    fn nesting_past_serde_jsons_recursion_limit_is_an_error_not_a_panic() {
        // The visitor never lifts serde_json's limit (128), so a package cannot recurse the stack away.
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        assert!(decode(deep.as_bytes()).is_err());
        assert!(decode_ordered(deep.as_bytes(), 1 << 24).is_err());
        let objects = format!("{}1{}", "{\"a\":".repeat(200), "}".repeat(200));
        assert!(decode(objects.as_bytes()).is_err());
        assert!(decode_ordered(objects.as_bytes(), 1 << 24).is_err());
        // Well inside the limit it decodes.
        let shallow = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(decode(shallow.as_bytes()).is_ok());
        assert!(decode_ordered(shallow.as_bytes(), 1 << 24).is_ok());
    }

    #[test]
    fn both_outputs_refuse_an_over_budget_document_with_the_same_text() {
        let doc = br#"{"a":[1,2,3],"b":{"c":"text","d":null}}"#;
        // Too small for the source charge (three times its length): refused before any node.
        let source_cap = doc.len() * 3 - 1;
        let by_source = decode_ordered(doc, source_cap).unwrap_err().to_string();
        let budget = Budget::new(source_cap);
        let value_charge = budget.charge(doc.len() * 3).unwrap_err();
        assert_eq!(by_source, value_charge);
        assert_eq!(by_source, "decoded recording budget exceeded");
        // Room for the source and not for the nodes: the one visitor charges both the same way.
        let cap = doc.len() * 3 + 64;
        let budget = Budget::new(cap);
        budget.charge(doc.len() * 3).unwrap();
        let by_value = decode_with(doc, &budget).unwrap_err().to_string();
        let by_ordered = decode_ordered(doc, cap).unwrap_err().to_string();
        assert_eq!(by_value, by_ordered);
        assert!(by_ordered.contains("budget exceeded"), "{by_ordered}");
        // And a generous budget decodes the same document through both.
        assert!(decode_ordered(doc, 1 << 20).is_ok());
        assert!(decode(doc).is_ok());
    }
}
