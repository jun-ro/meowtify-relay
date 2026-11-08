//! The link a host shares: `meowtify://jam/<code>?relay=<url>`.
//!
//! The relay address is in the link so a guest needs nothing but the link: it works with any
//! relay, including one the host runs at home.

use crate::wire::CODE_LEN;

const SCHEME: &str = "meowtify://jam/";

/// A jam a guest can join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JamLink {
    /// Lower case letters and digits.
    pub code: String,
    /// The relay's WebSocket address, such as `wss://jam.example.com`.
    pub relay: String,
}

impl JamLink {
    pub fn to_url(&self) -> String {
        format!("{SCHEME}{}?relay={}", self.code, encode(&self.relay))
    }

    /// Reads a link. Anything that is not a well formed jam link gives `None`: the link comes
    /// from outside the app, so nothing else in it is trusted.
    pub fn parse(text: &str) -> Option<JamLink> {
        let rest = text.trim().strip_prefix(SCHEME)?;
        let (code, query) = rest.split_once('?')?;
        if !CODE_LEN.contains(&code.len()) || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let relay = query.split('&').find_map(|pair| pair.strip_prefix("relay="))?;
        let relay = decode(relay)?;
        let scheme_ok = relay.starts_with("ws://") || relay.starts_with("wss://");
        let host_ok = relay.len() > 6 && relay.len() <= 512 && relay.chars().all(|c| c.is_ascii_graphic());
        (scheme_ok && host_ok).then(|| JamLink { code: code.to_ascii_lowercase(), relay })
    }
}

/// Percent-encodes everything but unreserved characters.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_round_trips() {
        let link = JamLink { code: "abc123xy".into(), relay: "wss://jam.example.com:8443/base".into() };
        let url = link.to_url();
        assert_eq!(url, "meowtify://jam/abc123xy?relay=wss%3A%2F%2Fjam.example.com%3A8443%2Fbase");
        assert_eq!(JamLink::parse(&url), Some(link));
    }

    #[test]
    fn codes_ignore_case() {
        let link = JamLink::parse("meowtify://jam/AbC123?relay=ws%3A%2F%2F127.0.0.1%3A8787").unwrap();
        assert_eq!(link.code, "abc123");
        assert_eq!(link.relay, "ws://127.0.0.1:8787");
    }

    #[test]
    fn bad_links_are_refused() {
        for bad in [
            "",
            "https://jam/abc123?relay=wss%3A%2F%2Fx.example",
            "meowtify://jam/abc?relay=wss%3A%2F%2Fx.example",
            "meowtify://jam/abc123",
            "meowtify://jam/abc123?relay=http%3A%2F%2Fx.example",
            "meowtify://jam/abc123?relay=wss%3A%2F%2Fx.example%zz",
            "meowtify://jam/abc/../123?relay=wss%3A%2F%2Fx.example",
            "meowtify://jam/abc123?relay=wss%3A%2F%2Fa%20b.example",
        ] {
            assert_eq!(JamLink::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn surrounding_space_is_ignored() {
        assert!(JamLink::parse("  meowtify://jam/abc123?relay=wss%3A%2F%2Fx.example\n").is_some());
    }
}
