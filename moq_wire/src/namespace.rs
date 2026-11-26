use crate::prelude::*;
use core::fmt::{self, Debug, Display, Formatter, Write};
use core::str::FromStr;
use crate::bytes::{FromBytes, ToBytes};
use crate::error::{Error, Result};
use crate::{Mode, Reader, Version, Writer, MAX_TRACK_NAMESPACE_TUPLE_LENGTH, MIN_TRACK_NAMESPACE_TUPLE_LENGTH};

/// A Track Namespace of 1 to 32 fields (draft-ietf-moq-transport-16 §2.4.1).
#[derive(Eq, PartialEq, Clone, Hash)]
pub struct Namespace(Vec<Vec<u8>>);

impl Namespace {
    pub fn new(fields: Vec<Vec<u8>>) -> Result<Self> {
        let count = MIN_TRACK_NAMESPACE_TUPLE_LENGTH..=MAX_TRACK_NAMESPACE_TUPLE_LENGTH;
        if !count.contains(&fields.len()) {
            return Err(Error::ProtocolViolation(format!("namespace of {} fields, must be {count:?}", fields.len())));
        }
        Ok(Self(fields))
    }

    pub fn fields(&self) -> &[Vec<u8>] {
        &self.0
    }

    pub fn iter(&self) -> core::slice::Iter<'_, Vec<u8>> {
        self.0.iter()
    }

    /// Whether `prefix`'s fields begin this namespace, Namespace Prefix Matching (draft-ietf-moq-transport-18 §8).
    pub fn has_prefix(&self, prefix: &Namespace) -> bool {
        self.0.starts_with(&prefix.0)
    }

    /// Bytes of all fields.
    pub fn byte_len(&self) -> usize {
        self.0.iter().map(Vec::len).sum()
    }
}

impl<'a> IntoIterator for &'a Namespace {
    type Item = &'a Vec<u8>;
    type IntoIter = core::slice::Iter<'a, Vec<u8>>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl FromBytes for Namespace {
    fn from_bytes<M: Mode>(r: &mut Reader<'_, M>, _version: Version) -> Result<Self> {
        Self::new(read_fields(r)?)
    }
}

impl ToBytes for Namespace {
    fn to_bytes(&self, w: &mut Writer, _version: Version) -> Result<()> {
        write_fields(w, &self.0)
    }
}

fn read_fields<M: Mode>(r: &mut Reader<'_, M>) -> Result<Vec<Vec<u8>>> {
    let count = r.varint()?;
    // not preallocated: the count comes from the peer
    let mut fields = Vec::new();
    for _ in 0..count {
        fields.push(r.length_prefixed_bytes()?.to_vec());
    }
    Ok(fields)
}

fn write_fields(w: &mut Writer, fields: &[Vec<u8>]) -> Result<()> {
    w.varint(fields.len() as u64)?;
    for field in fields {
        w.length_prefixed_bytes(field)?;
    }
    Ok(())
}

/// 0 to 32 Track Namespace Fields: a SUBSCRIBE_NAMESPACE's prefix, or the suffix
/// after it a NAMESPACE names (draft-ietf-moq-transport-18 §10.16, §10.18).
#[derive(Eq, PartialEq, Clone, Hash, Debug, Default)]
pub struct NamespaceTuple(Vec<Vec<u8>>);

impl NamespaceTuple {
    pub fn new(fields: Vec<Vec<u8>>) -> Result<Self> {
        if fields.len() > MAX_TRACK_NAMESPACE_TUPLE_LENGTH {
            return Err(Error::ProtocolViolation(format!("namespace tuple of {} fields, must be at most {MAX_TRACK_NAMESPACE_TUPLE_LENGTH}", fields.len())));
        }
        Ok(Self(fields))
    }

    pub fn fields(&self) -> &[Vec<u8>] {
        &self.0
    }

    /// Whether the fields of one begin the other's: SUBSCRIBE_NAMESPACEs sharing one overlap (§10.18).
    pub fn overlaps(&self, other: &NamespaceTuple) -> bool {
        self.0.starts_with(&other.0) || other.0.starts_with(&self.0)
    }
}

impl Namespace {
    /// Whether it begins with the fields of `prefix`.
    pub fn starts_with(&self, prefix: &NamespaceTuple) -> bool {
        self.0.starts_with(prefix.fields())
    }

    /// The fields after `prefix`, if it begins with them.
    pub fn suffix(&self, prefix: &NamespaceTuple) -> Option<NamespaceTuple> {
        self.0.strip_prefix(prefix.fields()).map(|rest| NamespaceTuple(rest.to_vec()))
    }
}

impl FromBytes for NamespaceTuple {
    fn from_bytes<M: Mode>(r: &mut Reader<'_, M>, _version: Version) -> Result<Self> {
        Self::new(read_fields(r)?)
    }
}

impl ToBytes for NamespaceTuple {
    fn to_bytes(&self, w: &mut Writer, _version: Version) -> Result<()> {
        write_fields(w, &self.0)
    }
}

/// The text form of `Namespace`, empty without fields.
impl Display for NamespaceTuple {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write_fields_escaped(f, &self.0)
    }
}

/// Fields joined by `-`, each escaped.
fn write_fields_escaped(f: &mut Formatter<'_>, fields: &[Vec<u8>]) -> fmt::Result {
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            f.write_char('-')?;
        }
        write_escaped(f, field)?;
    }
    Ok(())
}

impl Debug for Namespace {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.iter().map(|field| String::from_utf8_lossy(field))).finish()
    }
}

/// The text form: fields separated by `-`, bytes outside `[a-zA-Z0-9_]` as `.` and two hex digits.
impl Display for Namespace {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write_fields_escaped(f, &self.0)
    }
}

impl FromStr for Namespace {
    type Err = String;

    fn from_str(s: &str) -> core::result::Result<Self, String> {
        let fields = s.split('-').map(unescape).collect::<core::result::Result<_, _>>()?;
        Self::new(fields).map_err(|e| format!("{e:?}"))
    }
}

/// The text form: `[a-zA-Z0-9_]` as is, any other byte as `.` and two hex digits.
pub(crate) fn write_escaped(f: &mut Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for &b in bytes {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' => f.write_char(b as char)?,
            _ => write!(f, ".{b:02x}")?,
        }
    }
    Ok(())
}

/// Inverse of [`write_escaped`], taking any other character as its UTF-8 bytes.
pub(crate) fn unescape(s: &str) -> core::result::Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(s.len());
    let mut rest = s.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        rest = tail;
        if b != b'.' {
            bytes.push(b);
            continue;
        }
        let (&[high, low], tail) = rest.split_first_chunk::<2>().ok_or_else(|| format!("truncated escape in {s:?}"))?;
        let digit = |d: u8| (d as char).to_digit(16).ok_or_else(|| format!("invalid escape in {s:?}"));
        bytes.push((digit(high)? * 16 + digit(low)?) as u8);
        rest = tail;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_count_is_checked() {
        assert!(Namespace::new(vec![]).is_err());
        assert!(Namespace::new(vec![b"n".to_vec(); 33]).is_err());
        assert!("a-b".parse::<Namespace>().is_ok());
    }

    #[test]
    fn text_form_round_trips_any_bytes() {
        let namespace = Namespace::new(vec![b"a.b-c".to_vec(), vec![0, 0xff], "ä".as_bytes().to_vec()]).unwrap();
        assert_eq!(namespace.to_string(), "a.2eb.2dc-.00.ff-.c3.a4");
        assert_eq!(namespace.to_string().parse::<Namespace>(), Ok(namespace));
    }

    /// The example of draft-ietf-moq-transport-18 §8.
    #[test]
    fn prefix_matches_whole_fields() {
        let ns = |s: &str| s.parse::<Namespace>().unwrap();
        assert!(ns("foo-bar").has_prefix(&ns("foo")));
        assert!(ns("foo-bar").has_prefix(&ns("foo-bar")));
        assert!(!ns("foo-bar").has_prefix(&ns("foobar")));
        assert!(!ns("foo").has_prefix(&ns("foo-bar")));
    }

    /// The example of draft-ietf-moq-transport-18 §10.18.
    #[test]
    fn suffix_follows_the_prefix() {
        let ns: Namespace = "example_com-meeting_123-participant_100".parse().unwrap();
        let tuple = |fields: &[&str]| NamespaceTuple::new(fields.iter().map(|f| f.as_bytes().to_vec()).collect()).unwrap();
        assert_eq!(ns.suffix(&tuple(&["example_com", "meeting_123"])), Some(tuple(&["participant_100"])));
        assert_eq!(ns.suffix(&tuple(&[])), Some(tuple(&["example_com", "meeting_123", "participant_100"])));
        assert_eq!(ns.suffix(&tuple(&["example_com", "meeting_1"])), None);
        assert!(tuple(&["a"]).overlaps(&tuple(&["a", "b"])));
        assert!(!tuple(&["a", "c"]).overlaps(&tuple(&["a", "b"])));
    }

    /// Unescaped non-ASCII is taken as its UTF-8 bytes, escapes after it still decode.
    #[test]
    fn non_ascii_input_is_utf8() {
        assert_eq!(unescape("ä.41"), Ok(vec![0xc3, 0xa4, b'A']));
        assert!(unescape(".4").is_err());
        assert!(unescape(".zz").is_err());
    }
}
