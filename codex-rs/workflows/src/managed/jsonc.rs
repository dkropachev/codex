use std::fmt;

use anyhow::Context;
use anyhow::bail;
use serde::Deserialize;
use serde::de::Error as _;
use serde::de::MapAccess;
use serde::de::SeqAccess;
use serde::de::Visitor;

/// Parses caller-bounded JSONC while rejecting duplicate object keys at every depth.
#[allow(dead_code, reason = "used by managed Bun lock validation")]
pub(super) fn parse_unique_jsonc(contents: &str) -> anyhow::Result<serde_json::Value> {
    let normalized = normalize_jsonc(contents)?;
    serde_json::from_slice::<UniqueKeys>(&normalized)
        .context("JSONC must contain valid JSON without duplicate keys")?;
    serde_json::from_slice(&normalized).context("JSONC must contain valid JSON")
}

fn normalize_jsonc(contents: &str) -> anyhow::Result<Vec<u8>> {
    let bytes = contents.as_bytes();
    let mut without_comments = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            without_comments.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            without_comments.push(byte);
            index += 1;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            without_comments.push(b' ');
            index += 2;
            while index < bytes.len() && !matches!(bytes[index], b'\r' | b'\n') {
                index += 1;
            }
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            without_comments.push(b' ');
            index += 2;
            let mut closed = false;
            while index < bytes.len() {
                if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    index += 2;
                    closed = true;
                    break;
                }
                if matches!(bytes[index], b'\r' | b'\n') {
                    without_comments.push(bytes[index]);
                }
                index += 1;
            }
            if !closed {
                bail!("JSONC contains an unterminated block comment");
            }
            continue;
        }
        without_comments.push(byte);
        index += 1;
    }

    let mut normalized = Vec::with_capacity(without_comments.len());
    index = 0;
    in_string = false;
    escaped = false;
    while index < without_comments.len() {
        let byte = without_comments[index];
        if in_string {
            normalized.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            normalized.push(byte);
            index += 1;
            continue;
        }
        if byte == b',' {
            let mut following = index + 1;
            while without_comments
                .get(following)
                .is_some_and(u8::is_ascii_whitespace)
            {
                following += 1;
            }
            let follows_closing = matches!(without_comments.get(following), Some(b'}' | b']'));
            let follows_value = normalized
                .iter()
                .rev()
                .find(|byte| !byte.is_ascii_whitespace())
                .is_some_and(|byte| !matches!(*byte, b'{' | b'[' | b',' | b':'));
            if follows_closing && follows_value {
                index += 1;
                continue;
            }
        }
        normalized.push(byte);
        index += 1;
    }
    Ok(normalized)
}

struct UniqueKeys;

impl<'de> Deserialize<'de> for UniqueKeys {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer
            .deserialize_any(UniqueKeysVisitor)
            .map(|()| UniqueKeys)
    }
}

struct UniqueKeysVisitor;

impl<'de> Visitor<'de> for UniqueKeysVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<UniqueKeys>()?.is_some() {}
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = std::collections::HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(A::Error::custom(format!("duplicate object key `{key}`")));
            }
            map.next_value::<UniqueKeys>()?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "jsonc_tests.rs"]
mod tests;
