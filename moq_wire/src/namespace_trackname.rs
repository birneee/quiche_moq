use crate::prelude::*;
use core::fmt::{self, Display, Formatter};
use core::str::FromStr;
use crate::bytes::{FromBytes, ToBytes};
use crate::error::{Error, Result};
use crate::namespace::{unescape, write_escaped};
use crate::{Mode, Namespace, Reader, Version, Writer, MAX_FULL_TRACK_NAME_LEN};

/// A Full Track Name of at most 4096 bytes (draft-ietf-moq-transport-16 §2.4.1).
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct NamespaceTrackname {
    namespace: Namespace,
    trackname: Vec<u8>,
}

impl NamespaceTrackname {
    /// A namespace of 1 to 32 fields, see [`Namespace::new`].
    pub fn new(namespace: Vec<Vec<u8>>, trackname: Vec<u8>) -> Result<Self> {
        Self::from_parts(Namespace::new(namespace)?, trackname)
    }

    pub fn from_parts(namespace: Namespace, trackname: Vec<u8>) -> Result<Self> {
        let len = namespace.byte_len() + trackname.len();
        if len > MAX_FULL_TRACK_NAME_LEN {
            return Err(Error::ProtocolViolation(format!("full track name of {len} bytes, at most {MAX_FULL_TRACK_NAME_LEN}")));
        }
        Ok(Self { namespace, trackname })
    }

    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    pub fn trackname(&self) -> &[u8] {
        &self.trackname
    }

    /// Bytes of namespace and track name.
    pub fn byte_len(&self) -> usize {
        self.namespace.byte_len() + self.trackname.len()
    }
}

impl FromBytes for NamespaceTrackname {
    fn from_bytes<M: Mode>(r: &mut Reader<'_, M>, version: Version) -> Result<Self> {
        let namespace = Namespace::from_bytes(r, version)?;
        let trackname = r.length_prefixed_bytes()?.to_vec();
        Self::from_parts(namespace, trackname)
    }
}

impl ToBytes for NamespaceTrackname {
    fn to_bytes(&self, w: &mut Writer, version: Version) -> Result<()> {
        self.namespace.to_bytes(w, version)?;
        Ok(w.length_prefixed_bytes(&self.trackname)?)
    }
}

/// Namespace and track name separated by `--`, e.g. `example.2enet-team2--report`.
impl Display for NamespaceTrackname {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.namespace.fmt(f)?;
        f.write_str("--")?;
        write_escaped(f, &self.trackname)
    }
}

impl FromStr for NamespaceTrackname {
    type Err = String;

    fn from_str(s: &str) -> core::result::Result<Self, String> {
        let (namespace, trackname) = s.rsplit_once("--").ok_or_else(|| format!("no '--' before the track name in {s:?}"))?;
        Self::from_parts(namespace.parse()?, unescape(trackname)?).map_err(|e| format!("{e:?}"))
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use crate::NamespaceTrackname;

    fn example() -> NamespaceTrackname {
        NamespaceTrackname::new(vec![b"example.net".to_vec(), b"team2".to_vec(), b"project_x".to_vec()], b"report".to_vec()).unwrap()
    }

    #[test]
    fn text_form_round_trips() {
        assert_eq!(example().to_string(), "example.2enet-team2-project_x--report");
        assert_eq!("example.2enet-team2-project_x--report".parse(), Ok(example()));
    }

    #[test]
    fn limits() {
        assert!(NamespaceTrackname::new(vec![], b"t".to_vec()).is_err());
        assert!(NamespaceTrackname::new(vec![b"n".to_vec(); 33], b"t".to_vec()).is_err());
        assert!(NamespaceTrackname::new(vec![vec![b'n'; 4000]], vec![b't'; 96]).is_ok());
        assert!(NamespaceTrackname::new(vec![vec![b'n'; 4000]], vec![b't'; 97]).is_err());
    }

    #[test]
    fn wire_round_trip() {
        let version = crate::MOQ_VERSION_DRAFT_16;
        let bytes = crate::bytes::to_vec(&example(), version).unwrap();
        assert_eq!(crate::bytes::from_slice::<NamespaceTrackname>(&bytes, version).unwrap(), example());
    }
}
