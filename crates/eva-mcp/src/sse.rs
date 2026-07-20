//! Incremental Server-Sent Events decoding for MCP Streamable HTTP.
//!
//! The parser accepts arbitrary byte fragmentation, bounds every retained
//! line and event, and emits only complete UTF-8 events. The stream router can
//! wait for one JSON-RPC request ID while retaining a bounded set of
//! interleaved responses and notifications.

use crate::json_rpc::{parse_sse_json_rpc_envelope, McpJsonRpcMessageId, McpJsonRpcMessageKind};
use eva_core::EvaError;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::io::{self, Read};
use std::sync::Arc;

/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str =
    "bounded incremental MCP SSE decoding and JSON-RPC message correlation";

const SSE_READ_BUFFER_BYTES: usize = 8 * 1024;
const SSE_ITEMS_PER_PUSH_LIMIT: usize = 256;
const SSE_PENDING_MESSAGE_LIMIT: usize = 256;

/// One complete JSON-RPC message decoded from an SSE event.
#[derive(Clone, PartialEq, Eq)]
pub struct McpSseMessage {
    /// Direct JSON-RPC request/response ID, or `None` for a notification.
    pub request_id: Option<McpJsonRpcMessageId>,
    /// Direct JSON-RPC envelope role.
    pub kind: McpJsonRpcMessageKind,
    /// Last valid SSE `id` value observed before this event.
    pub event_id: Option<String>,
    /// Explicit SSE event type. The default message event is `None`.
    pub event_type: Option<String>,
    /// Complete UTF-8 JSON-RPC payload assembled from all `data` fields.
    pub data: String,
}

impl fmt::Debug for McpSseMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpSseMessage")
            .field("request_id_present", &self.request_id.is_some())
            .field("kind", &self.kind)
            .field("event_id_present", &self.event_id.is_some())
            .field("event_type_present", &self.event_type.is_some())
            .field("data_len", &self.data.len())
            .finish()
    }
}

/// One incremental SSE item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpSseItem {
    /// A comment line used as a keep-alive ping.
    Ping,
    /// A complete JSON-RPC event.
    Message(McpSseMessage),
}

/// Stateful incremental SSE parser.
pub struct McpSseParser {
    output_limit_bytes: usize,
    line: Vec<u8>,
    data: String,
    has_data: bool,
    event_type: Option<String>,
    last_event_id: Option<String>,
    retry_ms: Option<u64>,
    first_line: bool,
    skip_lf_after_cr: bool,
    closed: bool,
}

impl fmt::Debug for McpSseParser {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpSseParser")
            .field("output_limit_bytes", &self.output_limit_bytes)
            .field("line_len", &self.line.len())
            .field("data_len", &self.data.len())
            .field("has_data", &self.has_data)
            .field("event_type_present", &self.event_type.is_some())
            .field("last_event_id_present", &self.last_event_id.is_some())
            .field("retry_ms", &self.retry_ms)
            .field("closed", &self.closed)
            .finish()
    }
}

impl McpSseParser {
    /// Create a parser with one bound applied independently to each retained
    /// line and complete event payload.
    pub fn new(output_limit_bytes: usize) -> Result<Self, EvaError> {
        if output_limit_bytes == 0 {
            return Err(EvaError::invalid_argument(
                "MCP SSE output limit must be greater than zero",
            ));
        }
        Ok(Self {
            output_limit_bytes,
            line: Vec::new(),
            data: String::new(),
            has_data: false,
            event_type: None,
            last_event_id: None,
            retry_ms: None,
            first_line: true,
            skip_lf_after_cr: false,
            closed: false,
        })
    }

    /// Incrementally consume bytes and return complete items without retaining
    /// caller-owned chunks.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<McpSseItem>, EvaError> {
        if self.closed {
            return Err(sse_protocol_error(
                "MCP SSE parser is already closed",
                "mcp_sse_parser_closed",
            ));
        }
        let result = self.push_inner(chunk);
        if result.is_err() {
            self.closed = true;
        }
        result
    }

    /// Finish the parser at EOF. A partial line or undispatched event is a
    /// deterministic disconnect error and is never emitted as complete data.
    pub fn finish(&mut self) -> Result<(), EvaError> {
        if self.closed {
            return Err(sse_protocol_error(
                "MCP SSE parser is already closed",
                "mcp_sse_parser_closed",
            ));
        }
        self.closed = true;
        if !self.line.is_empty() && std::str::from_utf8(&self.line).is_err() {
            return Err(sse_protocol_error(
                "MCP SSE line is not valid UTF-8",
                "mcp_sse_utf8_invalid",
            ));
        }
        if !self.line.is_empty() || self.has_data || self.event_type.is_some() {
            return Err(sse_disconnect_error(true));
        }
        Ok(())
    }

    /// Return the latest valid SSE reconnect delay without applying reconnect
    /// behavior inside the parser.
    pub const fn retry_ms(&self) -> Option<u64> {
        self.retry_ms
    }

    fn push_inner(&mut self, chunk: &[u8]) -> Result<Vec<McpSseItem>, EvaError> {
        let mut items = Vec::new();
        for &byte in chunk {
            if self.skip_lf_after_cr {
                self.skip_lf_after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\r' => {
                    self.emit_line(&mut items)?;
                    self.skip_lf_after_cr = true;
                }
                b'\n' => self.emit_line(&mut items)?,
                _ => {
                    if self.line.len() == self.output_limit_bytes {
                        return Err(sse_limit_error(
                            "MCP SSE line exceeded output limit",
                            "mcp_sse_line_too_large",
                            self.output_limit_bytes,
                        ));
                    }
                    self.line.push(byte);
                }
            }
        }
        Ok(items)
    }

    fn emit_line(&mut self, items: &mut Vec<McpSseItem>) -> Result<(), EvaError> {
        let mut line = std::mem::take(&mut self.line);
        if self.first_line {
            self.first_line = false;
            if line.starts_with(&[0xef, 0xbb, 0xbf]) {
                line.drain(..3);
            }
        }
        let line = std::str::from_utf8(&line).map_err(|_| {
            sse_protocol_error("MCP SSE line is not valid UTF-8", "mcp_sse_utf8_invalid")
        })?;
        let item = self.process_line(line)?;
        if let Some(item) = item {
            if items.len() == SSE_ITEMS_PER_PUSH_LIMIT {
                return Err(sse_limit_error(
                    "MCP SSE chunk emitted too many items",
                    "mcp_sse_item_limit",
                    SSE_ITEMS_PER_PUSH_LIMIT,
                ));
            }
            items.push(item);
        }
        Ok(())
    }

    fn process_line(&mut self, line: &str) -> Result<Option<McpSseItem>, EvaError> {
        if line.is_empty() {
            return self.dispatch_event();
        }
        if line.starts_with(':') {
            return Ok(Some(McpSseItem::Ping));
        }
        let (field, mut value) = line.split_once(':').unwrap_or((line, ""));
        if let Some(stripped) = value.strip_prefix(' ') {
            value = stripped;
        }
        match field {
            "data" => {
                let separator = usize::from(self.has_data);
                let next_len = self
                    .data
                    .len()
                    .checked_add(separator)
                    .and_then(|length| length.checked_add(value.len()))
                    .ok_or_else(|| {
                        sse_limit_error(
                            "MCP SSE event size overflowed",
                            "mcp_sse_event_too_large",
                            self.output_limit_bytes,
                        )
                    })?;
                if next_len > self.output_limit_bytes {
                    return Err(sse_limit_error(
                        "MCP SSE event exceeded output limit",
                        "mcp_sse_event_too_large",
                        self.output_limit_bytes,
                    ));
                }
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "event" => {
                self.event_type = (!value.is_empty()).then(|| value.to_owned());
            }
            "id" if !value.contains('\0') => {
                self.last_event_id = Some(value.to_owned());
            }
            "retry" if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                if let Ok(retry_ms) = value.parse::<u64>() {
                    self.retry_ms = Some(retry_ms);
                }
            }
            _ => {}
        }
        Ok(None)
    }

    fn dispatch_event(&mut self) -> Result<Option<McpSseItem>, EvaError> {
        if !self.has_data {
            self.event_type = None;
            return Ok(None);
        }
        let data = std::mem::take(&mut self.data);
        self.has_data = false;
        let event_type = self.event_type.take();
        if event_type.as_deref() == Some("error") {
            return Err(sse_protocol_error(
                "MCP SSE server emitted an error event",
                "mcp_sse_error_event",
            ));
        }
        let envelope = parse_sse_json_rpc_envelope(&data)?;
        Ok(Some(McpSseItem::Message(McpSseMessage {
            request_id: envelope.request_id,
            kind: envelope.kind,
            event_id: self.last_event_id.clone(),
            event_type,
            data,
        })))
    }
}

/// Internal source boundary used by socket and reader-backed event streams.
pub(crate) trait McpSseSource: Send {
    fn read_sse(&mut self, buffer: &mut [u8]) -> Result<usize, EvaError>;
}

/// Cloneable, redacted control handle for an abortable SSE source.
#[derive(Clone)]
pub(crate) struct McpSseAbortHandle {
    abort: Arc<dyn Fn() -> Result<(), EvaError> + Send + Sync>,
}

impl fmt::Debug for McpSseAbortHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpSseAbortHandle")
            .field("abort_capable", &true)
            .finish_non_exhaustive()
    }
}

impl McpSseAbortHandle {
    pub(crate) fn new(abort: impl Fn() -> Result<(), EvaError> + Send + Sync + 'static) -> Self {
        Self {
            abort: Arc::new(abort),
        }
    }

    pub(crate) fn abort(&self) -> Result<(), EvaError> {
        (self.abort)()
    }
}

struct ReaderSseSource<R> {
    reader: R,
}

impl<R: Read + Send> McpSseSource for ReaderSseSource<R> {
    fn read_sse(&mut self, buffer: &mut [u8]) -> Result<usize, EvaError> {
        self.reader.read(buffer).map_err(map_sse_reader_error)
    }
}

/// Blocking incremental SSE reader with bounded interleaved-message routing.
pub struct McpSseEventStream {
    source: Box<dyn McpSseSource>,
    abort: Option<McpSseAbortHandle>,
    parser: McpSseParser,
    ready: VecDeque<McpSseItem>,
    pending_responses: BTreeMap<McpJsonRpcMessageId, VecDeque<McpSseMessage>>,
    pending_peer_messages: VecDeque<McpSseMessage>,
    pending_message_count: usize,
    pending_bytes: usize,
    output_limit_bytes: usize,
    ping_count: usize,
    disconnected: bool,
}

impl fmt::Debug for McpSseEventStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpSseEventStream")
            .field("abort_capable", &self.abort.is_some())
            .field("parser", &self.parser)
            .field("ready_count", &self.ready.len())
            .field("pending_response_ids", &self.pending_responses.len())
            .field(
                "pending_peer_message_count",
                &self.pending_peer_messages.len(),
            )
            .field("pending_message_count", &self.pending_message_count)
            .field("pending_bytes", &self.pending_bytes)
            .field("output_limit_bytes", &self.output_limit_bytes)
            .field("ping_count", &self.ping_count)
            .field("disconnected", &self.disconnected)
            .finish()
    }
}

impl McpSseEventStream {
    /// Build a stream over any blocking reader. Production HTTP sessions use
    /// the same parser through a framing-aware internal source.
    pub fn from_reader(
        reader: impl Read + Send + 'static,
        output_limit_bytes: usize,
    ) -> Result<Self, EvaError> {
        Self::from_source(Box::new(ReaderSseSource { reader }), output_limit_bytes)
    }

    pub(crate) fn from_source(
        source: Box<dyn McpSseSource>,
        output_limit_bytes: usize,
    ) -> Result<Self, EvaError> {
        Self::from_source_with_abort(source, None, output_limit_bytes)
    }

    pub(crate) fn from_abortable_source(
        source: Box<dyn McpSseSource>,
        abort: McpSseAbortHandle,
        output_limit_bytes: usize,
    ) -> Result<Self, EvaError> {
        Self::from_source_with_abort(source, Some(abort), output_limit_bytes)
    }

    fn from_source_with_abort(
        source: Box<dyn McpSseSource>,
        abort: Option<McpSseAbortHandle>,
        output_limit_bytes: usize,
    ) -> Result<Self, EvaError> {
        Ok(Self {
            source,
            abort,
            parser: McpSseParser::new(output_limit_bytes)?,
            ready: VecDeque::new(),
            pending_responses: BTreeMap::new(),
            pending_peer_messages: VecDeque::new(),
            pending_message_count: 0,
            pending_bytes: 0,
            output_limit_bytes,
            ping_count: 0,
            disconnected: false,
        })
    }

    /// Interrupt a production HTTP-backed reader. Generic readers do not
    /// promise cross-thread cancellation and fail closed here.
    pub fn abort(&self) -> Result<(), EvaError> {
        self.abort_handle()?.abort()
    }

    pub(crate) fn abort_handle(&self) -> Result<McpSseAbortHandle, EvaError> {
        self.abort.clone().ok_or_else(|| {
            EvaError::unsupported("MCP SSE source does not support real I/O abort")
                .with_provider_code("mcp_sse_abort_unavailable")
        })
    }

    pub(crate) const fn output_limit_bytes(&self) -> usize {
        self.output_limit_bytes
    }

    /// Read the next item in wire order. Messages buffered by
    /// `next_response` remain available through `take_response` or
    /// `take_notification`.
    pub fn next_item(&mut self) -> Result<McpSseItem, EvaError> {
        loop {
            if let Some(item) = self.ready.pop_front() {
                if item == McpSseItem::Ping {
                    self.ping_count = self.ping_count.saturating_add(1);
                }
                return Ok(item);
            }
            if self.disconnected {
                return Err(sse_disconnect_error(false));
            }
            let mut buffer = [0_u8; SSE_READ_BUFFER_BYTES];
            let read = match self.source.read_sse(&mut buffer) {
                Ok(read) => read,
                Err(error) => {
                    self.disconnected = true;
                    return Err(error);
                }
            };
            if read > buffer.len() {
                self.disconnected = true;
                return Err(
                    EvaError::internal("MCP SSE source returned an invalid length")
                        .with_provider_code("mcp_sse_source_length_invalid"),
                );
            }
            if read == 0 {
                self.disconnected = true;
                self.parser.finish()?;
                return Err(sse_disconnect_error(false));
            }
            match self.parser.push(&buffer[..read]) {
                Ok(items) => self.ready.extend(items),
                Err(error) => {
                    self.disconnected = true;
                    return Err(error);
                }
            }
        }
    }

    /// Wait for one request ID while retaining bounded interleaved responses
    /// and notifications for later consumers.
    pub fn next_response(
        &mut self,
        request_id: impl Into<McpJsonRpcMessageId>,
    ) -> Result<McpSseMessage, EvaError> {
        let request_id = request_id.into();
        if let Some(message) = self.take_response(&request_id) {
            return Ok(message);
        }
        loop {
            match self.next_item()? {
                McpSseItem::Ping => {}
                McpSseItem::Message(message)
                    if message.kind == McpJsonRpcMessageKind::Response
                        && message.request_id.as_ref() == Some(&request_id) =>
                {
                    return Ok(message);
                }
                McpSseItem::Message(message) => {
                    if let Err(error) = self.buffer_message(message) {
                        self.disconnected = true;
                        return Err(error);
                    }
                }
            }
        }
    }

    /// Remove one previously buffered response for an exact request ID.
    pub fn take_response(&mut self, request_id: &McpJsonRpcMessageId) -> Option<McpSseMessage> {
        let (message, remove_queue) = {
            let queue = self.pending_responses.get_mut(request_id)?;
            let message = queue.pop_front()?;
            (message, queue.is_empty())
        };
        if remove_queue {
            self.pending_responses.remove(request_id);
        }
        self.release_pending(&message);
        Some(message)
    }

    /// Remove one peer request or notification buffered while another
    /// response was awaited. Inspect `kind` to distinguish the two.
    pub fn take_peer_message(&mut self) -> Option<McpSseMessage> {
        let message = self.pending_peer_messages.pop_front()?;
        self.release_pending(&message);
        Some(message)
    }

    /// Remove one notification while retaining interleaved peer requests.
    pub fn take_notification(&mut self) -> Option<McpSseMessage> {
        let position = self
            .pending_peer_messages
            .iter()
            .position(|message| message.kind == McpJsonRpcMessageKind::Notification)?;
        let message = self.pending_peer_messages.remove(position)?;
        self.release_pending(&message);
        Some(message)
    }

    /// Number of comment pings observed by this reader.
    pub const fn ping_count(&self) -> usize {
        self.ping_count
    }

    /// Number of messages retained for a different request or notification.
    pub const fn pending_message_count(&self) -> usize {
        self.pending_message_count
    }

    fn buffer_message(&mut self, message: McpSseMessage) -> Result<(), EvaError> {
        let retained = retained_message_bytes(&message);
        let next_bytes = self.pending_bytes.checked_add(retained).ok_or_else(|| {
            sse_limit_error(
                "MCP SSE pending messages overflowed",
                "mcp_sse_pending_limit",
                self.output_limit_bytes,
            )
        })?;
        if self.pending_message_count == SSE_PENDING_MESSAGE_LIMIT
            || next_bytes > self.output_limit_bytes
        {
            return Err(sse_limit_error(
                "MCP SSE pending messages exceeded limit",
                "mcp_sse_pending_limit",
                self.output_limit_bytes,
            ));
        }
        self.pending_message_count += 1;
        self.pending_bytes = next_bytes;
        if message.kind == McpJsonRpcMessageKind::Response {
            let request_id = message.request_id.clone().ok_or_else(|| {
                sse_protocol_error(
                    "MCP SSE response is missing a request ID",
                    "mcp_sse_response_id_missing",
                )
            })?;
            self.pending_responses
                .entry(request_id)
                .or_default()
                .push_back(message);
        } else {
            self.pending_peer_messages.push_back(message);
        }
        Ok(())
    }

    fn release_pending(&mut self, message: &McpSseMessage) {
        self.pending_message_count = self.pending_message_count.saturating_sub(1);
        self.pending_bytes = self
            .pending_bytes
            .saturating_sub(retained_message_bytes(message));
    }
}

fn retained_message_bytes(message: &McpSseMessage) -> usize {
    message
        .data
        .len()
        .saturating_add(message.event_id.as_ref().map_or(0, String::len))
        .saturating_add(message.event_type.as_ref().map_or(0, String::len))
        .saturating_add(match message.request_id.as_ref() {
            Some(McpJsonRpcMessageId::Number(_)) => std::mem::size_of::<u64>(),
            Some(McpJsonRpcMessageId::String(value)) => value.len(),
            None => 0,
        })
}

pub(crate) fn retained_item_bytes(item: &McpSseItem) -> usize {
    match item {
        McpSseItem::Ping => 0,
        McpSseItem::Message(message) => retained_message_bytes(message),
    }
}

fn map_sse_reader_error(error: io::Error) -> EvaError {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        EvaError::timeout("MCP SSE read timed out").with_provider_code("mcp_sse_read_timeout")
    } else {
        EvaError::unavailable("MCP SSE reader failed")
            .with_provider_code("mcp_sse_read_failed")
            .with_context("io_error_kind", format!("{:?}", error.kind()))
    }
}

fn sse_protocol_error(message: &'static str, code: &'static str) -> EvaError {
    EvaError::unavailable(message)
        .with_provider_code(code)
        .with_retryable(false)
}

fn sse_disconnect_error(partial_event: bool) -> EvaError {
    EvaError::unavailable("MCP SSE stream disconnected")
        .with_provider_code("mcp_sse_disconnected")
        .with_context("partial_event", partial_event.to_string())
}

fn sse_limit_error(message: &'static str, code: &'static str, limit: usize) -> EvaError {
    EvaError::conflict(message)
        .with_provider_code(code)
        .with_context("output_limit_bytes", limit.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn provider_code(error: &EvaError) -> &str {
        error
            .provider_code()
            .map(|code| code.as_str())
            .unwrap_or("missing")
    }

    fn response(id: u64, value: &str) -> String {
        format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{value}}}")
    }

    fn event(data: &str) -> String {
        format!("data: {data}\n\n")
    }

    #[test]
    fn parser_accepts_every_byte_fragment_and_utf8_data() {
        let text = format!(
            "\u{feff}data: {{\"jsonrpc\":\"2.0\",\r\ndata: \"id\":1,\"result\":{{\"text\":\"{}\"}}}}\r\n\r\n",
            "\u{4f60}\u{597d}\u{1f642}"
        );
        let mut parser = McpSseParser::new(1024).unwrap();
        let mut items = Vec::new();
        for byte in text.as_bytes() {
            items.extend(parser.push(&[*byte]).unwrap());
        }
        parser.finish().unwrap();
        assert_eq!(items.len(), 1);
        let McpSseItem::Message(message) = &items[0] else {
            panic!("expected message");
        };
        assert_eq!(message.request_id, Some(McpJsonRpcMessageId::Number(1)));
        assert_eq!(message.kind, McpJsonRpcMessageKind::Response);
        assert!(message.data.contains("\u{4f60}\u{597d}\u{1f642}"));
        assert!(message.data.contains("\n\"id\""));
    }

    #[test]
    fn parser_handles_cr_lf_crlf_ping_ids_and_event_reset() {
        let first = response(1, "true");
        let second = response(2, "false");
        let input =
            format!(": keepalive\rdata: {first}\revent: custom\rid: opaque\r\rdata: {second}\n\n");
        let mut parser = McpSseParser::new(1024).unwrap();
        let items = parser.push(input.as_bytes()).unwrap();
        parser.finish().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], McpSseItem::Ping);
        let McpSseItem::Message(first) = &items[1] else {
            panic!("expected first message");
        };
        assert_eq!(first.event_type.as_deref(), Some("custom"));
        assert_eq!(first.event_id.as_deref(), Some("opaque"));
        let McpSseItem::Message(second) = &items[2] else {
            panic!("expected second message");
        };
        assert_eq!(second.event_type, None);
        assert_eq!(second.event_id.as_deref(), Some("opaque"));
    }

    #[test]
    fn parser_joins_data_and_preserves_valid_retry_on_overflow() {
        let mut parser = McpSseParser::new(1024).unwrap();
        let input = b"retry: 250\nretry: 18446744073709551616\ndata: {\"jsonrpc\":\"2.0\",\ndata: \"method\":\"notice\"}\n\n";
        let items = parser.push(input).unwrap();
        assert_eq!(parser.retry_ms(), Some(250));
        let McpSseItem::Message(message) = &items[0] else {
            panic!("expected message");
        };
        assert_eq!(message.kind, McpJsonRpcMessageKind::Notification);
        assert!(message.data.contains("\n\"method\""));
    }

    #[test]
    fn parser_rejects_explicit_error_events() {
        let mut parser = McpSseParser::new(1024).unwrap();
        let error = parser
            .push(b"event: error\ndata: unavailable\n\n")
            .unwrap_err();
        assert_eq!(provider_code(&error), "mcp_sse_error_event");
    }

    #[test]
    fn stream_correlates_only_responses_and_retains_peer_messages() {
        let input = [
            event(&response(2, "\"early\"")),
            event("{\"jsonrpc\":\"2.0\",\"method\":\"notice\"}"),
            event("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}"),
            event(&response(1, "\"matched\"")),
        ]
        .concat();
        let mut stream =
            McpSseEventStream::from_reader(Cursor::new(input.into_bytes()), 4096).unwrap();
        let matched = stream.next_response(1_u64).unwrap();
        assert_eq!(matched.kind, McpJsonRpcMessageKind::Response);
        assert!(matched.data.contains("matched"));
        let early = stream
            .take_response(&McpJsonRpcMessageId::Number(2))
            .unwrap();
        assert!(early.data.contains("early"));
        assert_eq!(
            stream.take_peer_message().unwrap().kind,
            McpJsonRpcMessageKind::Notification
        );
        let request = stream.take_peer_message().unwrap();
        assert_eq!(request.kind, McpJsonRpcMessageKind::Request);
        assert_eq!(request.request_id, Some(McpJsonRpcMessageId::Number(1)));
        assert_eq!(stream.pending_message_count(), 0);
    }

    #[test]
    fn nested_ids_never_correlate() {
        let nested_notification =
            event("{\"jsonrpc\":\"2.0\",\"method\":\"notice\",\"params\":{\"id\":7}}");
        let direct_response = event(&response(7, "true"));
        let mut stream = McpSseEventStream::from_reader(
            Cursor::new(format!("{nested_notification}{direct_response}").into_bytes()),
            1024,
        )
        .unwrap();
        assert_eq!(
            stream.next_response(7_u64).unwrap().kind,
            McpJsonRpcMessageKind::Response
        );
        let nested = stream.take_peer_message().unwrap();
        assert_eq!(nested.request_id, None);

        let mut parser = McpSseParser::new(1024).unwrap();
        let error = parser
            .push(b"data: {\"jsonrpc\":\"2.0\",\"result\":{\"id\":7}}\n\n")
            .unwrap_err();
        assert_eq!(provider_code(&error), "mcp_protocol_error");
    }

    #[test]
    fn strict_envelopes_support_string_ids_and_reject_conflicting_fields() {
        let mut parser = McpSseParser::new(1024).unwrap();
        let items = parser
            .push(b"data: {\"jsonrpc\":\"2.0\",\"id\":\"opaque\",\"result\":true}\n\n")
            .unwrap();
        let McpSseItem::Message(message) = &items[0] else {
            panic!("expected message");
        };
        assert_eq!(
            message.request_id,
            Some(McpJsonRpcMessageId::String("opaque".to_owned()))
        );
        assert_eq!(message.kind, McpJsonRpcMessageKind::Response);

        for invalid in [
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"id\":2,\"result\":true}",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":true,\"error\":{}}",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"result\":true}",
            "{\"jsonrpc\":\"1.0\",\"id\":1,\"result\":true}",
        ] {
            let mut parser = McpSseParser::new(1024).unwrap();
            let error = parser
                .push(format!("data: {invalid}\n\n").as_bytes())
                .unwrap_err();
            assert_eq!(provider_code(&error), "mcp_protocol_error", "{invalid}");
        }
    }

    #[test]
    fn parser_rejects_invalid_utf8_partial_utf8_and_partial_events() {
        let mut invalid = McpSseParser::new(1024).unwrap();
        let error = invalid.push(b"data: \xff\n").unwrap_err();
        assert_eq!(provider_code(&error), "mcp_sse_utf8_invalid");

        let mut partial_utf8 = McpSseParser::new(1024).unwrap();
        partial_utf8.push(b"data: \xf0\x9f").unwrap();
        let error = partial_utf8.finish().unwrap_err();
        assert_eq!(provider_code(&error), "mcp_sse_utf8_invalid");

        for bytes in [
            b"data: half-line".as_slice(),
            b"data: {\"jsonrpc\":\"2.0\",\"method\":\"notice\"}\n".as_slice(),
        ] {
            let mut parser = McpSseParser::new(1024).unwrap();
            parser.push(bytes).unwrap();
            let error = parser.finish().unwrap_err();
            assert_eq!(provider_code(&error), "mcp_sse_disconnected");
        }
    }

    #[test]
    fn parser_enforces_line_and_aggregate_event_limits_per_event() {
        let mut line = McpSseParser::new(8).unwrap();
        let error = line.push(b"123456789").unwrap_err();
        assert_eq!(provider_code(&error), "mcp_sse_line_too_large");

        let mut aggregate = McpSseParser::new(48).unwrap();
        aggregate.push(b"data: 123456789012345678901234\n").unwrap();
        let error = aggregate
            .push(b"data: 123456789012345678901234\n")
            .unwrap_err();
        assert_eq!(provider_code(&error), "mcp_sse_event_too_large");

        let one = event(&response(1, "true"));
        let two = event(&response(2, "false"));
        let per_event_limit = 64;
        let mut stream = McpSseEventStream::from_reader(
            Cursor::new(format!("{one}{two}").into_bytes()),
            per_event_limit,
        )
        .unwrap();
        assert_eq!(
            stream.next_response(2_u64).unwrap().request_id,
            Some(McpJsonRpcMessageId::Number(2))
        );
        assert!(stream
            .take_response(&McpJsonRpcMessageId::Number(1))
            .is_some());
    }

    #[test]
    fn stream_bounds_pending_messages_by_bytes_and_count() {
        let mut byte_limited = McpSseEventStream::from_reader(
            Cursor::new(
                [event(&response(1, "true")), event(&response(2, "true"))]
                    .concat()
                    .into_bytes(),
            ),
            64,
        )
        .unwrap();
        let error = byte_limited.next_response(99_u64).unwrap_err();
        assert_eq!(provider_code(&error), "mcp_sse_pending_limit");
        let poisoned = byte_limited.next_item().unwrap_err();
        assert_eq!(provider_code(&poisoned), "mcp_sse_disconnected");

        let many = (0..=SSE_PENDING_MESSAGE_LIMIT)
            .map(|id| event(&response(id as u64, "null")))
            .collect::<String>();
        let mut count_limited =
            McpSseEventStream::from_reader(Cursor::new(many.into_bytes()), 1024 * 1024).unwrap();
        let error = count_limited.next_response(u64::MAX).unwrap_err();
        assert_eq!(provider_code(&error), "mcp_sse_pending_limit");
    }

    #[test]
    fn debug_output_redacts_ids_event_metadata_and_payloads() {
        let message = McpSseMessage {
            request_id: Some(McpJsonRpcMessageId::String("secret-request-id".to_owned())),
            kind: McpJsonRpcMessageKind::Request,
            event_id: Some("secret-event-id".to_owned()),
            event_type: Some("secret-event-type".to_owned()),
            data: "secret-payload".to_owned(),
        };
        let debug = format!("{message:?}");
        for secret in [
            "secret-request-id",
            "secret-event-id",
            "secret-event-type",
            "secret-payload",
        ] {
            assert!(!debug.contains(secret));
        }
        assert!(
            !format!("{:?}", message.request_id.as_ref().unwrap()).contains("secret-request-id")
        );
    }
}
