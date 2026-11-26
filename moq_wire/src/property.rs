//! Types of Track and Object Properties (draft-ietf-moq-transport-19 §12), Extension Headers before draft 18.

use crate::{BytesKey, VarintKey};

pub const OBJECT_DELIVERY_TIMEOUT: VarintKey = VarintKey::new(0x02);
pub const MAX_CACHE_DURATION: VarintKey = VarintKey::new(0x04);
pub const SUBGROUP_DELIVERY_TIMEOUT: VarintKey = VarintKey::new(0x06);
pub const IMMUTABLE_PROPERTIES: BytesKey = BytesKey::new(0x0B);
pub const DEFAULT_PUBLISHER_PRIORITY: VarintKey = VarintKey::new(0x0E);
pub const DEFAULT_PUBLISHER_GROUP_ORDER: VarintKey = VarintKey::new(0x22);
pub const DYNAMIC_GROUPS: VarintKey = VarintKey::new(0x30);
/// Groups before this object's group that don't and won't exist, an Object Property.
pub const PRIOR_GROUP_ID_GAP: VarintKey = VarintKey::new(0x3C);
/// Objects before this one in its group that don't and won't exist, an Object Property.
pub const PRIOR_OBJECT_ID_GAP: VarintKey = VarintKey::new(0x3E);
