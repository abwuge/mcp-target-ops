use super::Params;
use crate::core::oauth::OAuthError;

pub(super) fn parse_query(url: &str) -> std::result::Result<Params, OAuthError> {
    parse_urlencoded(url.split_once('?').map(|(_, query)| query).unwrap_or(""))
}

pub(super) fn parse_urlencoded(input: &str) -> std::result::Result<Params, OAuthError> {
    let mut params = Params::new();
    if input.is_empty() {
        return Ok(params);
    }

    for pair in input.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(percent_decode(key)?, percent_decode(value)?);
    }

    Ok(params)
}

pub(super) fn required_param<'a>(
    params: &'a Params,
    name: &'static str,
) -> std::result::Result<&'a str, OAuthError> {
    params
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OAuthError::new("invalid_request", format!("missing {name}")))
}

fn percent_decode(value: &str) -> std::result::Result<String, OAuthError> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut iter = value.as_bytes().iter().copied();

    while let Some(byte) = iter.next() {
        match byte {
            b'+' => bytes.push(b' '),
            b'%' => {
                let hi = iter.next().ok_or_else(|| {
                    OAuthError::new("invalid_request", "incomplete percent encoding")
                })?;
                let lo = iter.next().ok_or_else(|| {
                    OAuthError::new("invalid_request", "incomplete percent encoding")
                })?;
                let decoded = hex_value(hi)
                    .zip(hex_value(lo))
                    .map(|(hi, lo)| (hi << 4) | lo)
                    .ok_or_else(|| {
                        OAuthError::new("invalid_request", "invalid percent encoding")
                    })?;
                bytes.push(decoded);
            }
            other => bytes.push(other),
        }
    }

    String::from_utf8(bytes)
        .map_err(|_| OAuthError::new("invalid_request", "urlencoded value is not UTF-8"))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(super) fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(*byte as char)
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

pub(super) fn quote_header_value(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::{parse_urlencoded, percent_decode, percent_encode};

    #[test]
    fn parses_urlencoded_values() {
        let params = parse_urlencoded("scope=mcp%3Atools+other&state=a%2Bb").unwrap();
        assert_eq!(params.get("scope").unwrap(), "mcp:tools other");
        assert_eq!(params.get("state").unwrap(), "a+b");
    }

    #[test]
    fn percent_codec_round_trips_reserved_values() {
        let encoded = percent_encode("https://example.com/cb?x=a b&y=1");
        assert_eq!(
            percent_decode(&encoded).unwrap(),
            "https://example.com/cb?x=a b&y=1"
        );
    }
}
