use crate::ServerEndpoint;
use reqwest::blocking::RequestBuilder;
use reqwest::header::RETRY_AFTER;
use std::{fmt, io::Read};
use stockhouse_wire::ReplyError;
use stockhouse_wire_v2::{
    BatchKind, BatchReply, EntryKey, MAX_BATCH_REPLY, MAX_NAR_REPLY, MAX_PROBE_REPLY, NarReply,
    ProbeReply, parse_batch_reply, parse_nar_reply, parse_probe_reply,
};

/// A v2 HTTP exchange that did not produce a classified reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestFailure {
    Transport,
    Reply(ReplyError),
}

impl fmt::Display for RequestFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport => f.write_str("request failed"),
            Self::Reply(error) => error.fmt(f),
        }
    }
}

impl ServerEndpoint {
    /// Sends an authenticated version probe.
    pub(crate) fn probe_reply(
        &self,
        request: RequestBuilder,
    ) -> Result<ProbeReply, RequestFailure> {
        read_v2(request, MAX_PROBE_REPLY, true, parse_probe_reply)
    }

    /// Sends a v2 batch request and checks all replies against the sent entry keys.
    pub fn batch_reply(
        &self,
        request: RequestBuilder,
        kind: BatchKind,
        keys: &[EntryKey],
    ) -> Result<BatchReply, RequestFailure> {
        read_v2(request, MAX_BATCH_REPLY, true, |status, retry, body| {
            parse_batch_reply(kind, keys, status, retry, body)
        })
    }

    /// Sends a v2 NAR request and classifies its reply.
    pub fn nar_reply(&self, request: RequestBuilder) -> Result<NarReply, RequestFailure> {
        read_v2(request, MAX_NAR_REPLY, false, parse_nar_reply)
    }
}

fn read_v2<T>(
    request: RequestBuilder,
    limit: usize,
    ignore_404_body: bool,
    classify: impl FnOnce(u16, Option<&str>, &[u8]) -> Result<T, ReplyError>,
) -> Result<T, RequestFailure> {
    let response = request.send().map_err(|_| RequestFailure::Transport)?;
    let status = response.status().as_u16();
    let retry_after = response.headers().get(RETRY_AFTER).cloned();
    let retry_after = retry_after.as_ref().and_then(|value| value.to_str().ok());
    let mut body = Vec::new();
    if !(matches!(status, 401 | 429 | 503) || (status == 404 && ignore_404_body)) {
        // Include one extra byte so the codec can distinguish a reply at the cap
        // from one that exceeds it, including a NAR 404's special case.
        if response
            .take(limit as u64 + 1)
            .read_to_end(&mut body)
            .is_err()
        {
            if status != 404 {
                return Err(RequestFailure::Transport);
            }
            // A partial NAR 404 body cannot establish an unknown upload.
            body.clear();
        }
    }
    classify(status, retry_after, &body).map_err(RequestFailure::Reply)
}
