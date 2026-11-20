use std::{convert::Infallible, time::Duration};

use axum::{
    body::{Body, Bytes},
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE},
        HeaderName,
    },
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    BoxError,
};
use holon_domain::runtime_event::EVENT_STREAM_HEARTBEAT_INTERVAL;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

pub fn heartbeat_sse<E>(receiver: mpsc::Receiver<Result<Event, E>>, interval: Duration) -> Response
where
    E: Into<BoxError> + Send + 'static,
{
    Sse::new(ReceiverStream::new(receiver))
        .keep_alive(KeepAlive::new().interval(interval).text("heartbeat"))
        .into_response()
}

pub fn runtime_event_stream(receiver: mpsc::Receiver<Result<Bytes, Infallible>>) -> Response {
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        match tokio::time::timeout(EVENT_STREAM_HEARTBEAT_INTERVAL, receiver.recv()).await {
            Ok(Some(frame)) => Some((frame, receiver)),
            Ok(None) => None,
            Err(_) => Some((Ok(Bytes::from_static(b": heartbeat\n\n")), receiver)),
        }
    });
    (
        [
            (
                HeaderName::from_static(holon_domain::runtime_event::EVENT_CONTRACT_VERSION_HEADER),
                holon_domain::runtime_event::RUNTIME_EVENT_CONTRACT_VERSION.to_string(),
            ),
            (CONTENT_TYPE, "text/event-stream".into()),
            (CACHE_CONTROL, "no-cache".into()),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

#[cfg(test)]
mod tests;
