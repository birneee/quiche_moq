use std::fmt;
use std::fmt::{Display, Formatter};
use std::str::FromStr;
use crate::{Namespace, Tuple};

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct NamespaceTrackname {
    pub(crate) namespace: Namespace,
    pub(crate) trackname: Vec<u8>,
}

impl NamespaceTrackname {
    pub fn new(namespace: Vec<Vec<u8>>, trackname: Vec<u8>) -> Self {
        Self {
            namespace: Namespace(Tuple(namespace)),
            trackname,
        }
    }

    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    pub fn trackname(&self) -> &[u8] {
        &self.trackname
    }

    #[allow(unused)]
    fn text_encoded_len(&self) -> usize {
        self.namespace.0.0.iter().map(|b| escape_len(b)).sum::<usize>()
            + self.namespace.0.0.len().saturating_sub(1) // The "-" separators
            + 2 // The "--" separator
            + escape_len(&self.trackname)
    }
}

// todo more optimized but conflicts with Display automatic implementation
// impl ToString for NamespaceTrackname {
//     fn to_string(&self) -> String {
//         let mut txt = String::with_capacity(self.text_encoded_len());
//         let _ = std::fmt::write(&mut txt, format_args!("{}", self));
//         txt
//     }
// }

impl FromStr for NamespaceTrackname {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.rsplitn(2, "--");
        let trackname_raw = parts.next().ok_or("Missing track name")?;
        let namespace_raw = parts.next().ok_or("Missing namespace delimiter '--'")?;

        let namespace: Vec<Vec<u8>> = namespace_raw
            .split('-')
            .map(unescape)
            .collect::<Result<Vec<_>, _>>()?;

        // 3. Unescape the track name
        let trackname = unescape(trackname_raw)?;

        Ok(NamespaceTrackname { namespace: Namespace(Tuple(namespace)), trackname })
    }
}

impl Display for NamespaceTrackname {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.namespace.fmt(f)?;
        f.write_str("--")?;
        write_escape(f, &self.trackname)
    }
}

fn unescape(s: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '.' {
            if i + 2 >= chars.len() {
                return Err("Truncated hex sequence".to_string());
            }
            let hex_str = &s[i + 1..i + 3];
            let byte = u8::from_str_radix(hex_str, 16)
                .map_err(|_| format!("Invalid hex sequence: .{}", hex_str))?;
            bytes.push(byte);
            i += 3;
        } else {
            bytes.push(chars[i] as u8);
            i += 1;
        }
    }
    Ok(bytes)
}

fn escape_len(data: &[u8]) -> usize {
    data.iter().map(|&b| match b {
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' => 1,
        _ => 3, // '.' + two hex digits
    }).sum()
}

pub(crate) fn write_escape(f: &mut fmt::Formatter<'_>, data: &[u8]) -> fmt::Result {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    for &b in data {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' => {
                f.write_str(std::str::from_utf8(&[b]).unwrap())?;
            }
            _ => {
                let buf = [
                    b'.',
                    HEX_CHARS[(b >> 4) as usize],
                    HEX_CHARS[(b & 0xf) as usize],
                ];
                // Safety: We know this buffer is valid ASCII/UTF-8
                f.write_str(unsafe { std::str::from_utf8_unchecked(&buf) })?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::NamespaceTrackname;

    #[test]
    fn to_string() {
        let ntn = NamespaceTrackname::new(vec![b"example.net".to_vec(), b"team2".to_vec(), b"project_x".to_vec()], b"report".to_vec());
        assert_eq!(ntn.to_string(), "example.2enet-team2-project_x--report")
    }

    #[test]
    fn from_string() {
        let txt = "example.2enet-team2-project_x--report";
        let ntn = NamespaceTrackname::new(vec![b"example.net".to_vec(), b"team2".to_vec(), b"project_x".to_vec()], b"report".to_vec());
        assert_eq!(ntn, txt.parse().unwrap());
    }
}