use crate::prelude::*;
use crate::role::Role;
use crate::{BytesKey, KeyValuePair, KeyValuePairs, RequestId, VarintKey, Version};

const ROLE: VarintKey = VarintKey::new(0x00);
const PATH: BytesKey = BytesKey::new(0x01);
const MAX_REQUEST_ID: VarintKey = VarintKey::new(0x02);
const MAX_FILTER_RANGES: VarintKey = VarintKey::new(0x06);
const MAX_AUTH_TOKEN_CACHE_SIZE: VarintKey = VarintKey::new(0x04);
const AUTHORITY: BytesKey = BytesKey::new(0x05);
// draft 14 gives it 0x05 too, a collision with AUTHORITY that drafts 15 and 16 resolve to 0x07
const MOQT_IMPLEMENTATION: BytesKey = BytesKey::new(0x07);
const MAX_REQUEST_UPDATES: VarintKey = VarintKey::new(0x08);

/// The Setup Options (draft-ietf-moq-transport-19 §10.3.1) of SETUP, the Setup Parameters of
/// CLIENT_SETUP and SERVER_SETUP before draft 17. Options a draft lacks are left out, unknown ones ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetupOptions {
    /// PATH, by a client on native QUIC
    pub path: Option<Vec<u8>>,
    /// AUTHORITY, by a client on native QUIC, draft 14 on
    pub authority: Option<Vec<u8>>,
    /// MOQT_IMPLEMENTATION, draft 14 on
    pub implementation: Option<Vec<u8>>,
    /// MAX_REQUEST_ID (MAX_SUBSCRIBE_ID before draft 11) until draft 16, QUIC's stream limit after
    pub max_request_id: Option<RequestId>,
    /// MAX_AUTH_TOKEN_CACHE_SIZE, draft 11 on, 0 if absent
    pub max_auth_token_cache_size: u64,
    /// MAX_REQUEST_UPDATES, draft 19 on
    pub max_request_updates: Option<u64>,
    /// MAX_FILTER_RANGES, draft 19 on, 0 if absent: no Range Filters
    pub max_filter_ranges: u64,
    /// ROLE, draft 7 only
    pub role: Option<Role>,
}

impl SetupOptions {
    pub(crate) fn to_pairs(&self, version: Version) -> KeyValuePairs {
        let draft = version.draft();
        let mut pairs = KeyValuePairs::new();
        let mut put_bytes = |key, value: &Option<Vec<u8>>, from_draft| {
            if let Some(bytes) = value && draft >= from_draft {
                pairs.insert(KeyValuePair::Bytes(key, bytes.clone()));
            }
        };
        put_bytes(PATH, &self.path, 7);
        put_bytes(AUTHORITY, &self.authority, 14);
        put_bytes(MOQT_IMPLEMENTATION, &self.implementation, 14);
        if let Some(max_request_id) = self.max_request_id && version.limits_requests() {
            pairs.insert(KeyValuePair::Varint(MAX_REQUEST_ID, max_request_id.0));
        }
        if self.max_auth_token_cache_size != 0 && draft >= 11 {
            pairs.insert(KeyValuePair::Varint(MAX_AUTH_TOKEN_CACHE_SIZE, self.max_auth_token_cache_size));
        }
        if let Some(n) = self.max_request_updates && draft >= 19 {
            pairs.insert(KeyValuePair::Varint(MAX_REQUEST_UPDATES, n));
        }
        if self.max_filter_ranges != 0 && draft >= 19 {
            pairs.insert(KeyValuePair::Varint(MAX_FILTER_RANGES, self.max_filter_ranges));
        }
        if let Some(role) = &self.role && draft == 7 {
            pairs.insert(KeyValuePair::Varint(ROLE, role.to_id()));
        }
        pairs
    }

    // TODO AUTHORIZATION TOKEN, until then it is ignored like unknown options
    pub(crate) fn from_pairs(pairs: &KeyValuePairs, version: Version) -> crate::Result<Self> {
        let draft = version.draft();
        let bytes = |key, from_draft| pairs.get_bytes(key).filter(|_| draft >= from_draft).map(<[u8]>::to_vec);
        let varint = |key, applies: bool| pairs.get_varint(key).filter(|_| applies);
        Ok(Self {
            path: bytes(PATH, 7),
            authority: bytes(AUTHORITY, 14),
            implementation: bytes(MOQT_IMPLEMENTATION, 14),
            max_request_id: varint(MAX_REQUEST_ID, version.limits_requests()).map(RequestId),
            max_auth_token_cache_size: varint(MAX_AUTH_TOKEN_CACHE_SIZE, draft >= 11).unwrap_or(0),
            max_request_updates: varint(MAX_REQUEST_UPDATES, draft >= 19),
            max_filter_ranges: varint(MAX_FILTER_RANGES, draft >= 19).unwrap_or(0),
            role: varint(ROLE, draft == 7).map(Role::from_id).transpose()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_19};
    use rstest::rstest;

    fn options() -> SetupOptions {
        SetupOptions {
            path: Some(b"/p".to_vec()),
            authority: Some(b"a".to_vec()),
            implementation: Some(b"q".to_vec()),
            max_request_id: Some(RequestId(9)),
            max_auth_token_cache_size: 5,
            max_request_updates: Some(2),
            max_filter_ranges: 4,
            role: Some(Role::PubSub),
        }
    }

    /// What a draft lacks is left out.
    #[rstest]
    #[case(MOQ_VERSION_DRAFT_07, SetupOptions { authority: None, implementation: None, max_auth_token_cache_size: 0, max_request_updates: None, max_filter_ranges: 0, ..options() })]
    #[case(MOQ_VERSION_DRAFT_11, SetupOptions { authority: None, implementation: None, max_request_updates: None, max_filter_ranges: 0, role: None, ..options() })]
    #[case(MOQ_VERSION_DRAFT_16, SetupOptions { max_request_updates: None, max_filter_ranges: 0, role: None, ..options() })]
    #[case(MOQ_VERSION_DRAFT_19, SetupOptions { max_request_id: None, role: None, ..options() })]
    fn maps_to_each_draft(#[case] version: Version, #[case] expected: SetupOptions) {
        assert_eq!(SetupOptions::from_pairs(&options().to_pairs(version), version).unwrap(), expected);
    }

    #[test]
    fn unknown_options_are_ignored() {
        let pairs = [KeyValuePair::Bytes(BytesKey::new(0x3F), vec![1])].into_iter().collect();
        assert_eq!(SetupOptions::from_pairs(&pairs, MOQ_VERSION_DRAFT_16).unwrap(), SetupOptions::default());
    }
}
