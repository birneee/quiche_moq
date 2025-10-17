use crate::bytes::{FromBytes, ToBytes};
use crate::{Parameter, RequestId, Version, MAX_REQUEST_ID_SETUP_PARAMETER_ID, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13, PATH_SETUP_PARAMETER_ID, ROLE_SETUP_PARAMETER_ID};
use octets::{Octets, OctetsMut};
use crate::parameter::ParameterValue;
use crate::role::Role;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SetupParameters {
    pub path: Option<Vec<u8>>,
    pub max_request_id: Option<RequestId>,
    pub role: Option<Role>,
    pub extra_parameters: Vec<Parameter>,
}

impl SetupParameters {

    fn number_of_parameters(&self) -> usize {
        self.path.as_ref().map_or(0, |_| 1)
            + self.max_request_id.as_ref().map_or(0, |_| 1)
            + self.role.as_ref().map_or(0, |_| 1)
            + self.extra_parameters.len()
    }
}

impl FromBytes for SetupParameters {
    /// including the length varint
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let mut s = Self {
            path: None,
            max_request_id: None,
            role: None,
            extra_parameters: vec![],
        };
        let number_of_parameters = b.get_varint()?;
        for _ in 0..number_of_parameters {
            let p = Parameter::from_bytes(b, version)?;
            match (p.ty, &p.value, version) {
                (MAX_REQUEST_ID_SETUP_PARAMETER_ID, ParameterValue::Bytes(v), MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10) => {
                    assert_eq!(v.len(), 1);
                    s.max_request_id = Some(v[0] as u64);
                }
                (MAX_REQUEST_ID_SETUP_PARAMETER_ID, ParameterValue::Varint(v), MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13) => {
                    s.max_request_id = Some(*v)
                }
                (PATH_SETUP_PARAMETER_ID, ParameterValue::Bytes(v), _) => {
                    s.path = Some(v.clone())
                }
                (ROLE_SETUP_PARAMETER_ID, ParameterValue::Bytes(v), MOQ_VERSION_DRAFT_07) => {
                    s.role = Some(Role::from_bytes(&mut Octets::with_slice(&v), version)?);
                }
                _ => { // unknown
                    s.extra_parameters.push(p);
                }
            }
        }
        Ok(s)
    }
}

impl ToBytes for SetupParameters {
    /// including the length varint
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        b.put_varint(self.number_of_parameters() as u64)?;
        if let Some(path) = &self.path {
            Parameter::new_bytes(PATH_SETUP_PARAMETER_ID, path.clone()).to_bytes(b, version)?;
        }
        if let Some(max_request_id) = self.max_request_id {
            Parameter::new_varint(MAX_REQUEST_ID_SETUP_PARAMETER_ID, max_request_id).to_bytes(b, version)?;
        }
        if let Some(role) = &self.role {
            Parameter::new_varint(ROLE_SETUP_PARAMETER_ID, role.to_id()).to_bytes(b, version)?;
        }
        for param in &self.extra_parameters {
            param.to_bytes(b, version)?;
        }
        Ok(())
    }
}
