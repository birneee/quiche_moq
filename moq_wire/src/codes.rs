//! Error and status codes, unknown ones from a peer kept as `Other`.

/// A code space: named codes, `From` both ways with the wire value.
macro_rules! codes {
    ($(#[$meta:meta])* $name:ident: $repr:ty { $($(#[$vmeta:meta])* $variant:ident = $value:literal,)* }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)*
            Other($repr),
        }

        impl From<$repr> for $name {
            fn from(value: $repr) -> Self {
                match value {
                    $($value => Self::$variant,)*
                    other => Self::Other(other),
                }
            }
        }

        impl From<$name> for $repr {
            fn from(code: $name) -> $repr {
                match code {
                    $($name::$variant => $value,)*
                    $name::Other(value) => value,
                }
            }
        }
    };
}

codes! {
    /// Why a session closes (draft-16 §13.1.1).
    SessionCloseCode: u32 {
        NoError = 0x0,
        InternalError = 0x1,
        Unauthorized = 0x2,
        ProtocolViolation = 0x3,
        InvalidRequestId = 0x4,
        DuplicateTrackAlias = 0x5,
        KeyValueFormattingError = 0x6,
        TooManyRequests = 0x7,
        InvalidPath = 0x8,
        MalformedPath = 0x9,
        GoawayTimeout = 0x10,
        ControlMessageTimeout = 0x11,
        DataStreamTimeout = 0x12,
        AuthTokenCacheOverflow = 0x13,
        DuplicateAuthTokenAlias = 0x14,
        VersionNegotiationFailed = 0x15,
        MalformedAuthToken = 0x16,
        UnknownAuthTokenAlias = 0x17,
        ExpiredAuthToken = 0x18,
        InvalidAuthority = 0x19,
        MalformedAuthority = 0x1A,
    }
}

codes! {
    /// Why a request was rejected, in REQUEST_ERROR (draft-ietf-moq-transport-19 §15.11.2).
    RequestErrorCode: u64 {
        InternalError = 0x0,
        Unauthorized = 0x1,
        Timeout = 0x2,
        NotSupported = 0x3,
        MalformedAuthToken = 0x4,
        ExpiredAuthToken = 0x5,
        GoingAway = 0x6,
        ExcessiveLoad = 0x9,
        DoesNotExist = 0x10,
        InvalidRange = 0x11,
        MalformedTrack = 0x12,
        /// until draft 16
        DuplicateSubscription = 0x19,
        Uninterested = 0x20,
        PrefixOverlap = 0x30,
        NamespaceTooLarge = 0x31,
        InvalidJoiningRequestId = 0x32,
        UnsupportedExtension = 0x33,
        Redirect = 0x34,
        ConflictingFilters = 0x35,
        InvalidFilter = 0x36,
    }
}

codes! {
    /// Why a publication ended, in PUBLISH_DONE (draft-16 §9.15).
    PublishDoneStatus: u64 {
        /// An implementation specific error.
        InternalError = 0x0,
        /// The track is no longer published.
        TrackEnded = 0x2,
        /// The end of the subscription filter is reached.
        SubscriptionEnded = 0x3,
        /// A relay detected a malformed track.
        MalformedTrack = 0x12,
    }
}

codes! {
    /// Why a data stream was reset (draft-16 §13.1.3).
    ResetCode: u64 {
        InternalError = 0x0,
        Canceled = 0x1,
        DeliveryTimeout = 0x2,
        SessionClosed = 0x3,
    }
}

impl ResetCode {
    /// As a WebTransport stream error, limited to 32 bits: larger codes become INTERNAL_ERROR.
    pub fn to_u32(self) -> u32 {
        u32::try_from(u64::from(self)).unwrap_or(u64::from(ResetCode::InternalError) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_value_round_trips() {
        assert_eq!(RequestErrorCode::from(0x10), RequestErrorCode::DoesNotExist);
        assert_eq!(u64::from(RequestErrorCode::DoesNotExist), 0x10);
        assert_eq!(RequestErrorCode::from(0x99), RequestErrorCode::Other(0x99));
        assert_eq!(u64::from(RequestErrorCode::Other(0x99)), 0x99);
    }
}
