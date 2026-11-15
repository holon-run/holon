mod parse;
mod plan;
mod send;

#[cfg(any(test, feature = "runtime-tests"))]
pub use parse::{accumulate_chat_completion_stream_events, parse_chat_completion_response};
#[cfg(any(test, feature = "runtime-tests"))]
pub use plan::build_chat_completion_messages;
pub(super) use plan::build_chat_completion_parts;
#[cfg(any(test, feature = "runtime-tests"))]
pub use plan::build_chat_completion_request;
pub(super) use plan::plan_chat_completion_request;
pub use send::send_chat_completion_request_with_material;
#[cfg(any(test, feature = "runtime-tests"))]
pub use send::{
    classify_openai_chat_completion_error, send_chat_completion_request,
    send_chat_completion_stream_request,
};
