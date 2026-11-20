#![deny(clippy::indexing_slicing)]

use reqwest::header::HeaderMap;

pub mod redact_common {
    pub fn redact_error_detail(detail: &str) -> String {
        detail.to_string()
    }
}

pub fn link_span_to_current(_span: &tracing::Span) {}

pub fn current_traceparent() -> Option<String> {
    span_traceparent(&tracing::Span::current())
}

pub fn span_traceparent(span: &tracing::Span) -> Option<String> {
    let id = span.id()?.into_u64();
    Some(format!("00-{id:032x}-{id:016x}-01"))
}

pub fn traceparent_of_span(span: &tracing::Span) -> Option<String> {
    span_traceparent(span)
}

pub fn inject_trace_context_into_request(builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    builder
}

pub fn trace_context_headers() -> HeaderMap {
    HeaderMap::new()
}

pub fn inject_trace_context(_headers: &mut HeaderMap) {}

pub fn span_from_meta_traceparent(
    _meta: &serde_json::Map<String, serde_json::Value>,
) -> tracing::Span {
    tracing::info_span!("acp_dispatch")
}

pub fn link_span_to_meta(_span: &tracing::Span, _meta: &serde_json::Value) -> bool {
    false
}

pub fn link_current_span_to_meta(_meta: &serde_json::Value) {}

pub fn set_parent_from_traceparent(_span: &tracing::Span, _traceparent: &str) -> bool {
    false
}
