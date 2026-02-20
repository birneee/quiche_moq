use crate::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_08, MOQ_VERSION_DRAFT_09, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16};

pub type Version = u64;

pub fn version_to_name(v: Version) -> &'static str {
    match v {
        MOQ_VERSION_DRAFT_07 => "07",
        MOQ_VERSION_DRAFT_08 => "08",
        MOQ_VERSION_DRAFT_09 => "09",
        MOQ_VERSION_DRAFT_10 => "10",
        MOQ_VERSION_DRAFT_11 => "11",
        MOQ_VERSION_DRAFT_12 => "12",
        MOQ_VERSION_DRAFT_13 => "13",
        MOQ_VERSION_DRAFT_14 => "14",
        MOQ_VERSION_DRAFT_15 => "15",
        MOQ_VERSION_DRAFT_16 => "16",
        _ => unimplemented!()
    }
}
