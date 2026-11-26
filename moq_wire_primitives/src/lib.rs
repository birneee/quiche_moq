//! The primitives MoQ messages are made of, without `std` and `unsafe`.
//! - [`Reader`] decodes borrowed input; a [`Stream`] reader may run short and be retried,
//!   a [`Frame`] reader holds a complete frame, where running short or leftovers are malformed
//! - [`Writer`] appends to a reused `Vec`, it only fails on values the wire can't carry
//! - [`VarIntForm`] is fixed per reader and writer, so every varint uses the negotiated encoding
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;

mod bounded;
mod error;
mod reader;
mod varint;
mod writer;

pub use bounded::{BoundedBytes, BoundedString, TooLong};
pub use error::{DecodeError, EncodeError};
pub use reader::{Frame, Mode, Reader, Stream};
pub use varint::VarIntForm;
pub use writer::Writer;
