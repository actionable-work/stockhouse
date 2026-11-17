//! The verified NAR stream: the latest chunk is held back until the
//! verifier finishes, so altered bytes never complete a transfer.

use crate::body::ByteStream;
use bytes::Bytes;
use futures_util::StreamExt;
use std::io;
use stockhouse_proto::{ContentVerifier, ServedIntegrity};

struct State {
    upstream: ByteStream,
    verifier: Option<ContentVerifier>,
    held: Option<Bytes>,
}

enum Step {
    Emit(Bytes),
    Fail(io::Error),
    Done,
}

/// Wraps an upstream stream with a consistency check.
pub(crate) fn verified(upstream: ByteStream, expected: ServedIntegrity) -> ByteStream {
    let state = State { upstream, verifier: Some(ContentVerifier::new(expected)), held: None };
    futures_util::stream::unfold(state, |mut state| async move {
        match next(&mut state).await {
            Step::Emit(bytes) => Some((Ok(bytes), state)),
            Step::Fail(error) => {
                state.verifier = None;
                state.held = None;
                Some((Err(error), state))
            }
            Step::Done => None,
        }
    })
    .boxed()
}

async fn next(state: &mut State) -> Step {
    loop {
        let Some(verifier) = state.verifier.as_mut() else { return Step::Done };
        match state.upstream.next().await {
            Some(Ok(chunk)) if chunk.is_empty() => continue,
            Some(Ok(chunk)) => {
                if let Err(e) = verifier.update(&chunk) {
                    return Step::Fail(io::Error::other(e));
                }
                if let Some(previous) = state.held.replace(chunk) {
                    return Step::Emit(previous);
                }
            }
            Some(Err(error)) => return Step::Fail(error),
            None => return finish(state),
        }
    }
}

fn finish(state: &mut State) -> Step {
    let verifier = state.verifier.take().expect("verifier present while streaming");
    match (verifier.finish(), state.held.take()) {
        (Err(e), _) => Step::Fail(io::Error::other(e)),
        (Ok(()), Some(last)) => Step::Emit(last),
        (Ok(()), None) => Step::Done,
    }
}
