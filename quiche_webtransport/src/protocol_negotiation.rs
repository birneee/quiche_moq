//! Minimal RFC 8941 Structured-Field encode/decode for `WT-Available-Protocols`
//! (a List of Strings) and `WT-Protocol` (a String Item), draft-ietf-webtrans-http3
//! §3.3. Only handles the plain-ASCII quoted-string subset — values here
//! (ALPN-style tokens like "moqt-18") never need escaping.

pub(crate) fn encode_string_list(items: &[String]) -> Vec<u8> {
    items
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ")
        .into_bytes()
}

pub(crate) fn parse_string_list(value: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(value)
        .split(',')
        .filter_map(|item| parse_quoted(item.trim()))
        .collect()
}

pub(crate) fn encode_string_item(item: &str) -> Vec<u8> {
    format!("\"{item}\"").into_bytes()
}

pub(crate) fn parse_string_item(value: &[u8]) -> Option<String> {
    parse_quoted(String::from_utf8_lossy(value).trim())
}

fn parse_quoted(s: &str) -> Option<String> {
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_list() {
        let items = vec!["moqt-18".to_string(), "moqt-17".to_string()];
        let encoded = encode_string_list(&items);
        assert_eq!(encoded, b"\"moqt-18\", \"moqt-17\"");
        assert_eq!(parse_string_list(&encoded), items);
    }

    #[test]
    fn round_trips_item() {
        let encoded = encode_string_item("moqt-18");
        assert_eq!(encoded, b"\"moqt-18\"");
        assert_eq!(parse_string_item(&encoded), Some("moqt-18".to_string()));
    }

    #[test]
    fn parse_list_ignores_malformed_entries() {
        assert_eq!(parse_string_list(b"\"moqt-18\", not-quoted, \"moqt-16\""), vec!["moqt-18", "moqt-16"]);
    }
}
