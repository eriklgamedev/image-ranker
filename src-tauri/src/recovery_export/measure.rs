//! Budget a borrowed serializable projection without constructing another JSON
//! tree. The scanner consumes only serde_json's generated valid JSON; it is not
//! an admission parser for untrusted input.
use crate::backup::{BudgetUsage, MAX_BYTES, MAX_DEPTH, MAX_NODES};
use serde::Serialize;
use std::collections::HashSet;
use std::io::{self, Write};

/// Exact canonical serde_json fragment dimensions. Construction is native-only
/// and checked; persisted/imported numeric metadata cannot manufacture one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fragment {
    bytes: u64,
    nodes: usize,
    depth: usize,
}
impl Fragment {
    fn checked(bytes: u64, nodes: usize, depth: usize) -> Result<Self, String> {
        if bytes > MAX_BYTES || nodes > MAX_NODES || depth > MAX_DEPTH {
            return Err("Recovery fragment exceeds supported byte or structure limits".into());
        }
        Ok(Self {
            bytes,
            nodes,
            depth,
        })
    }
    pub(crate) fn bytes(self) -> u64 {
        self.bytes
    }
    pub(crate) fn nodes(self) -> usize {
        self.nodes
    }
    pub(crate) fn depth(self) -> usize {
        self.depth
    }
    pub(crate) fn usage(self) -> BudgetUsage {
        BudgetUsage {
            bytes: self.bytes,
            nodes: self.nodes,
            depth: self.depth,
        }
    }
}

/// Array aggregation appends one already-measured member in constant work.
/// Empty containers have depth zero, matching the generated JSON scanner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ArrayUsage {
    fragment: Fragment,
    count: usize,
}
impl ArrayUsage {
    pub(crate) const fn empty() -> Self {
        Self {
            fragment: Fragment {
                bytes: 2,
                nodes: 1,
                depth: 0,
            },
            count: 0,
        }
    }
    pub(crate) fn len(self) -> usize {
        self.count
    }
    pub(crate) fn fragment(self) -> Fragment {
        self.fragment
    }
    pub(crate) fn push(self, member: Fragment) -> Result<Self, String> {
        let bytes = self
            .fragment
            .bytes
            .checked_add(member.bytes)
            .and_then(|bytes| bytes.checked_add(u64::from(self.count > 0)))
            .ok_or("Recovery size overflow")?;
        let nodes = self
            .fragment
            .nodes
            .checked_add(member.nodes)
            .ok_or("Recovery structure overflow")?;
        let depth = member
            .depth
            .checked_add(1)
            .ok_or("Recovery depth overflow")?
            .max(self.fragment.depth);
        Ok(Self {
            fragment: Fragment::checked(bytes, nodes, depth)?,
            count: self
                .count
                .checked_add(1)
                .ok_or("Recovery item count overflow")?,
        })
    }
}

/// Compose an exact object, including escaped UTF-8 keys and all punctuation.
/// Duplicate keys are rejected so metrics cannot depend on parser key collapse.
pub(crate) fn object(fields: &[(&str, Fragment)]) -> Result<Fragment, String> {
    let mut seen = HashSet::new();
    let mut bytes = 2u64;
    let mut nodes = 1usize;
    let mut depth = 0usize;
    for (index, (key, member)) in fields.iter().enumerate() {
        if key.len() > 1024 || !seen.insert(*key) {
            return Err("Recovery object has an oversized or duplicate key".into());
        }
        let key_bytes = serde_json::to_vec(key)
            .map_err(|error| error.to_string())?
            .len() as u64;
        bytes = bytes
            .checked_add(key_bytes)
            .and_then(|bytes| bytes.checked_add(1 + u64::from(index > 0)))
            .and_then(|bytes| bytes.checked_add(member.bytes))
            .ok_or("Recovery size overflow")?;
        nodes = nodes
            .checked_add(member.nodes)
            .ok_or("Recovery structure overflow")?;
        depth = depth.max(
            member
                .depth
                .checked_add(1)
                .ok_or("Recovery depth overflow")?,
        );
        // Reject during aggregation, not after an unbounded input allocation.
        Fragment::checked(bytes, nodes, depth)?;
    }
    Fragment::checked(bytes, nodes, depth)
}

pub(crate) fn fragment<T: Serialize + ?Sized>(value: &T) -> Result<Fragment, String> {
    let mut counter = Counter::new(MAX_BYTES, MAX_NODES, MAX_DEPTH);
    serde_json::to_writer(&mut counter, value).map_err(|error| error.to_string())?;
    if counter.string || !counter.stack.is_empty() {
        return Err("Invalid generated recovery JSON".into());
    }
    Fragment::checked(
        counter.usage.bytes,
        counter.usage.nodes,
        counter.usage.depth,
    )
}

pub(crate) fn wrap_envelope(payload: Fragment) -> Result<BudgetUsage, String> {
    Ok(object(&[
        ("formatVersion", fragment(&1u32)?),
        ("checksumSha256", fragment(&"0".repeat(64))?),
        ("payload", payload),
    ])?
    .usage())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Output<'a, T: ?Sized> {
    format_version: u32,
    checksum_sha256: &'static str,
    payload: &'a T,
}

#[derive(Clone, Copy)]
enum Frame {
    Object { key: bool },
    Array,
}
#[derive(Default)]
enum Escape {
    #[default]
    None,
    Slash,
    Unicode {
        digits: u8,
        value: u16,
    },
}
struct Counter {
    usage: BudgetUsage,
    stack: Vec<Frame>,
    string: bool,
    key: bool,
    string_bytes: usize,
    high_surrogate: bool,
    escape: Escape,
    primitive: bool,
    max_bytes: u64,
    max_nodes: usize,
    max_depth: usize,
}
impl Counter {
    fn new(max_bytes: u64, max_nodes: usize, max_depth: usize) -> Self {
        Self {
            usage: BudgetUsage {
                bytes: 0,
                nodes: 0,
                depth: 0,
            },
            stack: vec![],
            string: false,
            key: false,
            string_bytes: 0,
            high_surrogate: false,
            escape: Escape::None,
            primitive: false,
            max_bytes,
            max_nodes,
            max_depth,
        }
    }
    fn node(&mut self) -> io::Result<()> {
        self.usage.nodes += 1;
        self.usage.depth = self.usage.depth.max(self.stack.len());
        if self.usage.nodes > self.max_nodes || self.stack.len() > self.max_depth {
            return Err(io::Error::other(
                "Recovery copy exceeds supported structure limits",
            ));
        }
        Ok(())
    }
    fn string_length(&self) -> io::Result<()> {
        if self.string_bytes > if self.key { 1024 } else { 1024 * 1024 } {
            return Err(io::Error::other(
                "Recovery string or key exceeds supported limits",
            ));
        }
        Ok(())
    }
    fn byte(&mut self, byte: u8) -> io::Result<()> {
        if self.string {
            match self.escape {
                Escape::Slash => {
                    if byte == b'u' {
                        self.escape = Escape::Unicode {
                            digits: 0,
                            value: 0,
                        };
                        return Ok(());
                    }
                    self.string_bytes += 1;
                    self.escape = Escape::None;
                }
                Escape::Unicode { digits, value } => {
                    let digit = (byte as char)
                        .to_digit(16)
                        .ok_or_else(|| io::Error::other("Invalid generated JSON escape"))?
                        as u16;
                    let value = (value << 4) | digit;
                    if digits < 3 {
                        self.escape = Escape::Unicode {
                            digits: digits + 1,
                            value,
                        };
                        return Ok(());
                    }
                    if (0xd800..=0xdbff).contains(&value) {
                        self.high_surrogate = true;
                    } else if (0xdc00..=0xdfff).contains(&value) && self.high_surrogate {
                        self.string_bytes += 4;
                        self.high_surrogate = false;
                    } else {
                        self.string_bytes += if value < 0x80 {
                            1
                        } else if value < 0x800 {
                            2
                        } else {
                            3
                        };
                    }
                    self.escape = Escape::None;
                }
                Escape::None => match byte {
                    b'\\' => {
                        self.escape = Escape::Slash;
                        return Ok(());
                    }
                    b'"' => {
                        self.string = false;
                        return self.string_length();
                    }
                    _ => self.string_bytes += 1,
                },
            }
            return self.string_length();
        }
        if self.primitive {
            if !matches!(byte, b',' | b']' | b'}' | b' ' | b'\n' | b'\r' | b'\t') {
                return Ok(());
            }
            self.primitive = false;
        }
        match byte {
            b'{' => {
                self.node()?;
                self.stack.push(Frame::Object { key: true });
            }
            b'[' => {
                self.node()?;
                self.stack.push(Frame::Array);
            }
            b'}' | b']' => {
                self.stack
                    .pop()
                    .ok_or_else(|| io::Error::other("Invalid generated JSON framing"))?;
            }
            b':' => {
                if let Some(Frame::Object { key }) = self.stack.last_mut() {
                    *key = false;
                }
            }
            b',' => {
                if let Some(Frame::Object { key }) = self.stack.last_mut() {
                    *key = true;
                }
            }
            b'"' => {
                self.key = matches!(self.stack.last(), Some(Frame::Object { key: true }));
                if !self.key {
                    self.node()?;
                }
                self.string = true;
                self.string_bytes = 0;
                self.high_surrogate = false;
                self.escape = Escape::None;
            }
            b' ' | b'\n' | b'\r' | b'\t' => {}
            _ => {
                self.node()?;
                self.primitive = true;
            }
        }
        Ok(())
    }
}
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.usage.bytes = self
            .usage
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("Recovery size overflow"))?;
        if self.usage.bytes > self.max_bytes {
            return Err(io::Error::other(
                "Recovery copy exceeds supported byte limits",
            ));
        }
        #[cfg(test)]
        let mut scan = crate::capacity_tests::MeasurementScan::new();
        for byte in bytes {
            #[cfg(test)]
            scan.record_byte();
            self.byte(*byte)?;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn envelope<T: Serialize + ?Sized>(value: &T) -> Result<BudgetUsage, String> {
    bounded(value, MAX_BYTES, MAX_NODES, MAX_DEPTH)
}
fn bounded<T: Serialize + ?Sized>(
    value: &T,
    bytes: u64,
    nodes: usize,
    depth: usize,
) -> Result<BudgetUsage, String> {
    let mut counter = Counter::new(bytes, nodes, depth);
    let output = Output {
        format_version: 1,
        checksum_sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        payload: value,
    };
    serde_json::to_writer(&mut counter, &output).map_err(|e| e.to_string())?;
    if counter.string || !counter.stack.is_empty() {
        return Err("Invalid generated recovery JSON".into());
    }
    Ok(counter.usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    fn equivalent(value: &Value) {
        let expected = crate::backup::measure_envelope(value).unwrap();
        let actual = envelope(value).unwrap();
        assert_eq!(
            (actual.bytes, actual.nodes, actual.depth),
            (expected.bytes, expected.nodes, expected.depth)
        );
        assert!(bounded(value, actual.bytes - 1, MAX_NODES, MAX_DEPTH).is_err());
        assert!(bounded(value, MAX_BYTES, actual.nodes - 1, MAX_DEPTH).is_err());
        assert!(bounded(value, MAX_BYTES, MAX_NODES, actual.depth - 1).is_err());
        assert!(bounded(value, actual.bytes, actual.nodes, actual.depth).is_ok());
    }
    #[test]
    fn streaming_counter_matches_container_for_primitives_escapes_and_nested_shapes() {
        for value in [
            Value::Null,
            json!(true),
            json!(-10.125e30),
            json!("中文🦀\n\t\u{0000}\\\""),
            json!([]),
            json!({}),
            json!({"quoted\"\n中文":[null,false,42,{"x":[[],{},"😀"]}]}),
        ] {
            equivalent(&value);
        }
        for width in [0, 1, 17, 128] {
            equivalent(&json!((0..width)
                .map(|i| json!({"i":i,"children":[null,format!("entry-{i}")]}))
                .collect::<Vec<_>>()));
        }
    }
    #[test]
    fn streaming_counter_matches_string_key_and_depth_rejection() {
        for value in [
            json!({"x":"a".repeat(1024*1024)}),
            json!({"k".repeat(1024):0}),
        ] {
            equivalent(&value);
        }
        for value in [
            json!({"x":"a".repeat(1024*1024+1)}),
            json!({"k".repeat(1025):0}),
        ] {
            assert!(crate::backup::measure_envelope(&value).is_err());
            assert!(envelope(&value).is_err());
        }
        let mut value = Value::Null;
        for _ in 1..MAX_DEPTH {
            value = json!([value]);
        }
        equivalent(&value);
        value = json!([value]);
        assert!(crate::backup::measure_envelope(&value).is_err());
        assert!(envelope(&value).is_err());
    }
    #[test]
    fn borrowed_struct_projection_matches_materialized_value() {
        #[derive(Serialize)]
        struct Projection<'a> {
            history: &'a [Value],
            snapshot: &'a Value,
            pending: Option<&'a Value>,
        }
        let states = vec![json!({"a":[1,2]}), json!({"delta":"é"})];
        let projected = Projection {
            history: &states,
            snapshot: &states[0],
            pending: Some(&states[1]),
        };
        let expected =
            crate::backup::measure_envelope(&serde_json::to_value(&projected).unwrap()).unwrap();
        let actual = envelope(&projected).unwrap();
        assert_eq!(
            (actual.bytes, actual.nodes, actual.depth),
            (expected.bytes, expected.nodes, expected.depth)
        );
    }

    fn composed(value: &Value) -> Fragment {
        match value {
            Value::Array(items) => items
                .iter()
                .fold(ArrayUsage::empty(), |array, item| {
                    array.push(composed(item)).unwrap()
                })
                .fragment(),
            Value::Object(fields) => {
                let members: Vec<_> = fields
                    .iter()
                    .map(|(key, value)| (key.as_str(), composed(value)))
                    .collect();
                object(&members).unwrap()
            }
            _ => fragment(value).unwrap(),
        }
    }

    #[test]
    fn checked_composition_matches_real_json_bytes_nodes_depth_and_digit_boundaries() {
        for count in [0, 1, 9, 10, 99, 100, 999, 1_000] {
            let events: Vec<_> = (0..count).map(|index| json!({
                "sequence": index + 1,
                "key\"雪\n": [null, false, -0.0, -10.125e30, u64::MAX, "\u{0000}\\\"🦀", {}, []]
            })).collect();
            let value = json!({"snapshot":null,"checkpointRevision":0,"revision":count,"events":events,"recoveryNotes":[]});
            let joined = composed(&value);
            assert_eq!(joined, fragment(&value).unwrap());
            assert_eq!(
                joined.bytes(),
                serde_json::to_vec(&value).unwrap().len() as u64
            );
            let wrapped = wrap_envelope(joined).unwrap();
            let actual = crate::backup::measure_envelope(&value).unwrap();
            assert_eq!(
                (wrapped.bytes, wrapped.nodes, wrapped.depth),
                (actual.bytes, actual.nodes, actual.depth)
            );
        }
    }

    #[test]
    fn composition_rejects_oversized_keys_strings_duplicates_and_excess_depth() {
        let zero = fragment(&0).unwrap();
        assert!(object(&[("same", zero), ("same", zero)]).is_err());
        assert!(object(&[(&"雪".repeat(342), zero)]).is_err());
        assert!(object(&[(&"k".repeat(1024), zero)]).is_ok());
        assert!(fragment(&"x".repeat(1024 * 1024 + 1)).is_err());
        let mut actual = Value::Null;
        let mut aggregate = fragment(&actual).unwrap();
        for _ in 1..MAX_DEPTH {
            actual = json!([actual]);
            aggregate = ArrayUsage::empty().push(aggregate).unwrap().fragment();
        }
        assert_eq!(aggregate, fragment(&actual).unwrap());
        assert!(wrap_envelope(aggregate).is_ok());
        let over_envelope = ArrayUsage::empty().push(aggregate).unwrap().fragment();
        assert!(wrap_envelope(over_envelope).is_err());
        actual = json!([actual]);
        assert!(crate::backup::measure_envelope(&actual).is_err());
        assert!(ArrayUsage::empty().push(over_envelope).is_err());
    }

    #[test]
    fn composition_checks_arithmetic_limits_before_accepting_larger_container() {
        // Direct private construction isolates arithmetic at the actual caps;
        // public APIs cannot import these synthetic dimensions as authority.
        let maximum_bytes = Fragment::checked(MAX_BYTES - 2, 1, 0).unwrap();
        assert_eq!(
            ArrayUsage::empty()
                .push(maximum_bytes)
                .unwrap()
                .fragment()
                .bytes(),
            MAX_BYTES
        );
        assert!(ArrayUsage::empty()
            .push(Fragment::checked(MAX_BYTES - 1, 1, 0).unwrap())
            .is_err());
        let maximum_nodes = Fragment::checked(1, MAX_NODES - 1, 0).unwrap();
        assert_eq!(
            ArrayUsage::empty()
                .push(maximum_nodes)
                .unwrap()
                .fragment()
                .nodes(),
            MAX_NODES
        );
        assert!(ArrayUsage::empty()
            .push(Fragment::checked(1, MAX_NODES, 0).unwrap())
            .is_err());
        let invalid_overflow = Fragment {
            bytes: u64::MAX,
            nodes: usize::MAX,
            depth: usize::MAX,
        };
        assert!(ArrayUsage::empty().push(invalid_overflow).is_err());
        assert!(object(&[("member", invalid_overflow)]).is_err());
    }
}
