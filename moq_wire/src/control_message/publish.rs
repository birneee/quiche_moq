use crate::bytes::{FromBytes, ToBytes};
use crate::vi64;
use crate::{NamespaceTrackname, Parameters, RequestId, TrackAlias, Version,
            PUBLISH_MESSAGE_ID, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_18};
use octets::{Octets, OctetsMut};
use crate::control_message::ControlMessage;
use crate::tuple::Tuple;

#[derive(Debug, Clone)]
pub struct PublishMessage {
    pub request_id: RequestId,
    pub namespace_trackname: NamespaceTrackname,
    pub track_alias: TrackAlias,
    pub parameters: Parameters,
    pub track_extensions: Parameters,
}

impl PublishMessage {
    pub fn new(
        request_id: RequestId,
        namespace_trackname: NamespaceTrackname,
        track_alias: TrackAlias,
        parameters: Parameters,
    ) -> Self {
        Self {
            request_id,
            namespace_trackname,
            track_alias,
            parameters,
            track_extensions: Parameters(vec![]),
        }
    }
}

impl ControlMessage for PublishMessage {
    const MESSAGE_IDS: &'static [u64] = &[PUBLISH_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "publish" }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        match version {
            MOQ_VERSION_DRAFT_16..=MOQ_VERSION_DRAFT_18 => {}
            _ => unimplemented!("PUBLISH is only supported in draft-16 and later"),
        }
        vi64::write_varint(b, self.request_id, version)?;
        self.namespace_trackname.namespace().to_bytes(b, version)?;
        let name = self.namespace_trackname.trackname();
        vi64::write_varint(b, name.len() as u64, version)?;
        b.put_bytes(name)?;
        vi64::write_varint(b, self.track_alias, version)?;
        self.parameters.to_bytes(b, version)?;
        self.track_extensions.to_bytes(b, version)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        match version {
            MOQ_VERSION_DRAFT_16..=MOQ_VERSION_DRAFT_18 => {}
            _ => unimplemented!("PUBLISH is only supported in draft-16 and later"),
        }
        let request_id = vi64::read_varint(b, version)?;
        let track_namespace = Tuple::from_bytes(b, version)?.0;
        let track_name_len = vi64::read_varint(b, version)?;
        let track_name = b.get_bytes(track_name_len as usize)?.to_vec();
        let track_alias = vi64::read_varint(b, version)?;
        let parameters = Parameters::from_bytes(b, version)?;
        let track_extensions = Parameters::from_bytes(b, version)?;
        Ok(Self {
            request_id,
            namespace_trackname: NamespaceTrackname::new(track_namespace, track_name),
            track_alias,
            parameters,
            track_extensions,
        })
    }
}
