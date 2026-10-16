//! Reject expanded JSON structure while parsing, before constructing an unbounded Value tree.
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::fmt;

struct Budget {
    remaining: usize,
    max_depth: usize,
}
struct Seed<'a> {
    budget: &'a mut Budget,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;
    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        if self.depth > self.budget.max_depth || self.budget.remaining == 0 {
            return Err(de::Error::custom("Recovery JSON exceeds structure limits"));
        }
        self.budget.remaining -= 1;
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Value;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("bounded recovery JSON")
    }
    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("Invalid JSON number"))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        if value.len() > 1024 * 1024 {
            return Err(E::custom("Recovery string exceeds 1 MiB"));
        }
        Ok(Value::String(value.to_string()))
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
        if value.len() > 1024 * 1024 {
            return Err(E::custom("Recovery string exceeds 1 MiB"));
        }
        Ok(Value::String(value))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(Seed {
            budget: self.budget,
            depth: self.depth + 1,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if key.len() > 1024 || values.contains_key(&key) {
                return Err(de::Error::custom(
                    "Oversized or duplicate recovery object key",
                ));
            }
            let value = object.next_value_seed(Seed {
                budget: self.budget,
                depth: self.depth + 1,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

pub(super) fn parse(bytes: &[u8], max_nodes: usize, max_depth: usize) -> Result<Value, String> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = Seed {
        budget: &mut Budget {
            remaining: max_nodes,
            max_depth,
        },
        depth: 0,
    }
    .deserialize(&mut deserializer)
    .map_err(|e| format!("Damaged or oversized recovery copy: {e}"))?;
    deserializer
        .end()
        .map_err(|e| format!("Unexpected trailing recovery data: {e}"))?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn node_budget_is_enforced_during_deserialization() {
        assert!(parse(b"[1,2,3,4]", 4, 8).is_err());
        assert!(parse(b"[1,2,3,4]", 5, 8).is_ok());
    }
    #[test]
    fn duplicate_keys_and_trailing_values_are_rejected() {
        assert!(parse(br#"{"a":1,"a":2}"#, 20, 8).is_err());
        assert!(parse(b"{} {}", 20, 8).is_err());
    }
    #[test]
    fn nested_json_needs_explicit_budget() {
        assert!(parse(b"[[[0]]]", 20, 2).is_err());
        assert!(parse(b"[[[0]]]", 20, 3).is_ok());
    }
}
