//! Response body plumbing.

use bytes::Bytes;
use futures_util::stream::BoxStream;
use http_body_util::{BodyExt, Empty, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::body::Frame;

/// An upstream byte stream.
pub(crate) type ByteStream = BoxStream<'static, Result<Bytes, std::io::Error>>;
/// The proxy's response body; an `Err` item aborts the connection.
pub(crate) type Body = UnsyncBoxBody<Bytes, std::io::Error>;

pub(crate) fn full(data: impl Into<Bytes>) -> Body {
    Full::new(data.into()).map_err(|never| match never {}).boxed_unsync()
}

pub(crate) fn empty() -> Body {
    Empty::new().map_err(|never| match never {}).boxed_unsync()
}

pub(crate) fn stream(stream: ByteStream) -> Body {
    use futures_util::StreamExt;
    BodyExt::boxed_unsync(StreamBody::new(stream.map(|r| r.map(Frame::data))))
}
