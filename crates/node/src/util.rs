//! Small helpers: hex, random bytes, canonical JSON.

use serde_json::Value;

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xf) as usize] as char);
    }
    out
}

pub fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let text = text.as_bytes();
    if text.len() != N * 2 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = digit(text[i * 2])? << 4 | digit(text[i * 2 + 1])?;
    }
    Some(out)
}

/// Bytes from the system RNG. If it is unavailable (practically never), fall
/// back to hashing the clock — good enough for nonces and file names.
pub fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    if getrandom::fill(&mut out).is_err() {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut xof = blake3::Hasher::new();
        xof.update(&seed.to_le_bytes());
        xof.update(&(out.as_ptr() as usize).to_le_bytes());
        xof.finalize_xof().fill(&mut out);
    }
    out
}

/// JSON with object keys sorted at every level, so two builds that order map
/// keys differently still produce the same bytes (signatures depend on it).
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let bytes = [0u8, 1, 0xab, 0xff];
        assert_eq!(hex(&bytes), "0001abff");
        assert_eq!(unhex::<4>("0001ABff"), Some(bytes));
        assert_eq!(unhex::<4>("0001ab"), None);
        assert_eq!(unhex::<2>("zz00"), None);
    }

    #[test]
    fn canonical_json_sorts_keys() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"y":[1,{"d":2,"c":3}],"x":"é"}}"#).unwrap();
        assert_eq!(canonical_json(&a), r#"{"a":{"x":"é","y":[1,{"c":3,"d":2}]},"b":1}"#);
    }

    #[test]
    fn random_is_random() {
        assert_ne!(random::<16>(), random::<16>());
    }
}
