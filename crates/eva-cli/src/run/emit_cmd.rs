//! 类型化事件发布子命令；在进入内存或 durable EventBus 前完成所有 ID、目标和 payload 校验。

use super::{
    json_string, option_json, parse_common_options, required_option, success_envelope, trace_for,
    write_command_error, write_error_kind, CommonOptions, OutputFormat, EXIT_OK,
};
use eva_core::{
    AdapterId, AgentId, CapabilityName, EvaError, Event, EventId, EventPayload, EventTarget,
    GenerationId, RequestId, Topic, TraceContext,
};
use eva_eventbus::{DurableEventBus, EventBus, EventReceipt, InMemoryEventBus};
use eva_observability::TraceFields;
use eva_storage::{DurableBackendOptions, FileSystemDurableBackend};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
/// `eva emit` 的完整已解析命令。
pub(super) struct EmitCommand {
    /// 项目根和输出格式。
    common: CommonOptions,
    /// 必须解析为具体绝对 Topic 的事件路径。
    topic: String,
    /// 可选事件 ID；缺省按当前时间生成。
    event_id: Option<String>,
    /// 可选请求关联 ID。
    request_id: Option<String>,
    /// 可选运行时代际 ID。
    generation_id: Option<String>,
    /// 可选事件链根 ID。
    correlation_id: Option<String>,
    /// 可选直接父事件 ID。
    causation_id: Option<String>,
    /// 互斥的事件投递目标。
    target: EmitTarget,
    /// 互斥的事件 payload 表示。
    payload: EmitPayload,
    /// 可选 durable backend 根；缺省发布到进程内 EventBus。
    durable_backend: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// CLI 层尚未强类型化的互斥目标，解析完成后转换为 `EventTarget`。
enum EmitTarget {
    /// 未指定显式目标时广播。
    #[default]
    Broadcast,
    /// 定向 Agent ID 文本。
    Agent(
        /// 接收事件的目标 Agent 标识。
        String,
    ),
    /// 定向 Capability 名称文本。
    Capability(
        /// 接收事件的目标 capability 名称。
        String,
    ),
    /// 定向 Adapter ID 文本。
    Adapter(
        /// 接收事件的目标 Adapter 标识。
        String,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// CLI 接受的互斥 payload 输入形式。
enum EmitPayload {
    /// 显式或默认空 payload。
    #[default]
    Empty,
    /// UTF-8 文本 payload。
    Text(
        /// 按 UTF-8 字节发送的原始文本内容。
        String,
    ),
    /// 十六进制编码的二进制 payload。
    BytesHex(
        /// 等待校验并解码的十六进制字符串。
        String,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// EventBus 发布回执及用户输入 metadata 的稳定输出投影。
struct EmitReport {
    /// 实际 EventBus 后端种类。
    backend_kind: String,
    /// durable 后端路径；内存后端为空。
    backend_path: Option<String>,
    /// EventBus 接受的事件 ID。
    event_id: String,
    /// 已校验 topic。
    topic: String,
    /// 后端分配的发布序列号。
    sequence: u64,
    /// broadcast、agent、capability 或 adapter。
    target_kind: String,
    /// 定向目标的可选稳定值。
    target_value: Option<String>,
    /// empty、text 或 bytes。
    payload_kind: String,
    /// payload 的实际字节数。
    payload_size: usize,
    /// 可选请求关联 ID。
    request_id: Option<String>,
    /// 可选代际 ID。
    generation_id: Option<String>,
    /// 可选 correlation ID。
    correlation_id: Option<String>,
    /// 可选 causation ID。
    causation_id: Option<String>,
}

/// 解析 emit 的 topic、metadata、目标、payload 与后端选项。
///
/// 目标和 payload 通过专用 setter 保证最多出现一种；解析末尾再次强类型校验，确保发布阶段
/// 不会接收到含糊输入。首个非选项参数兼容作为 topic。
pub(super) fn parse_emit_command(args: &[String]) -> Result<EmitCommand, EvaError> {
    let mut passthrough = Vec::new();
    let mut topic = None;
    let mut event_id = None;
    let mut request_id = None;
    let mut generation_id = None;
    let mut correlation_id = None;
    let mut causation_id = None;
    let mut target = EmitTarget::Broadcast;
    let mut payload = EmitPayload::Empty;
    let mut payload_set = false;
    let mut durable_backend = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--project" | "--project-root" | "-p" | "--output" | "-o" => {
                passthrough.push(args[index].clone());
                index += 1;
                passthrough.push(required_option(args, index, "common option")?.clone());
            }
            "--topic" => {
                index += 1;
                set_once(
                    &mut topic,
                    required_option(args, index, "topic option")?.clone(),
                    "topic",
                )?;
            }
            "--event-id" | "--event" => {
                index += 1;
                event_id = Some(required_option(args, index, "event id option")?.clone());
            }
            "--request-id" | "--request" => {
                index += 1;
                request_id = Some(required_option(args, index, "request id option")?.clone());
            }
            "--generation" | "--generation-id" => {
                index += 1;
                generation_id = Some(required_option(args, index, "generation option")?.clone());
            }
            "--correlation-id" => {
                index += 1;
                correlation_id =
                    Some(required_option(args, index, "correlation id option")?.clone());
            }
            "--causation-id" => {
                index += 1;
                causation_id = Some(required_option(args, index, "causation id option")?.clone());
            }
            "--payload" | "--payload-text" => {
                index += 1;
                set_payload(
                    &mut payload,
                    &mut payload_set,
                    EmitPayload::Text(required_option(args, index, "payload option")?.clone()),
                )?;
            }
            "--payload-empty" => {
                set_payload(&mut payload, &mut payload_set, EmitPayload::Empty)?;
            }
            "--payload-bytes-hex" => {
                index += 1;
                set_payload(
                    &mut payload,
                    &mut payload_set,
                    EmitPayload::BytesHex(
                        required_option(args, index, "payload bytes hex option")?.clone(),
                    ),
                )?;
            }
            "--target-agent" | "--agent" => {
                index += 1;
                target = replace_target(
                    target,
                    EmitTarget::Agent(required_option(args, index, "target agent option")?.clone()),
                )?;
            }
            "--target-capability" | "--capability" => {
                index += 1;
                target = replace_target(
                    target,
                    EmitTarget::Capability(
                        required_option(args, index, "target capability option")?.clone(),
                    ),
                )?;
            }
            "--target-adapter" | "--adapter" => {
                index += 1;
                target = replace_target(
                    target,
                    EmitTarget::Adapter(
                        required_option(args, index, "target adapter option")?.clone(),
                    ),
                )?;
            }
            "--durable-backend" | "--durable-backend-root" => {
                index += 1;
                durable_backend = Some(PathBuf::from(required_option(
                    args,
                    index,
                    "durable backend option",
                )?));
            }
            value if value.starts_with('-') => passthrough.push(args[index].clone()),
            value => set_once(&mut topic, value.to_owned(), "topic")?,
        }
        index += 1;
    }

    let topic = topic.ok_or_else(|| {
        EvaError::invalid_argument("emit requires a topic")
            .with_context("suggestion", "use `eva emit /input/user --payload hello`")
    })?;
    Topic::parse(&topic)?;
    if let Some(value) = &event_id {
        EventId::parse(value)?;
    }
    if let Some(value) = &request_id {
        RequestId::parse(value)?;
    }
    if let Some(value) = &generation_id {
        GenerationId::parse(value)?;
    }
    if let Some(value) = &correlation_id {
        EventId::parse(value)?;
    }
    if let Some(value) = &causation_id {
        EventId::parse(value)?;
    }
    validate_target(&target)?;
    validate_payload(&payload)?;

    Ok(EmitCommand {
        common: parse_common_options(&passthrough)?,
        topic,
        event_id,
        request_id,
        generation_id,
        correlation_id,
        causation_id,
        target,
        payload,
        durable_backend,
    })
}

/// 发布事件并按实际命令输出格式报告回执或结构化错误。
pub(super) fn execute_emit<W, E>(
    command: EmitCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    let trace = trace_for("cli.emit");
    match publish_emit(&command) {
        Ok(report) => {
            write_emit(stdout, command.common.output, &report, &trace)?;
            Ok(EXIT_OK)
        }
        Err(error) => write_command_error(stderr, command.common.output, "emit", &error, &trace),
    }
}

/// 将 CLI 值转换为核心事件、附加 trace metadata 并发布到选定 EventBus。
/// 所有强类型解析在调用 `publish_to_bus` 前完成，因此失败不会留下部分持久化事件。
fn publish_emit(command: &EmitCommand) -> Result<EmitReport, EvaError> {
    let topic = Topic::parse(&command.topic)?;
    let event_id_value = command.event_id.clone().unwrap_or_else(default_event_id);
    let event_id = EventId::parse(&event_id_value)?;
    let payload_summary = payload_summary(&command.payload)?;
    let mut event = Event::new(
        event_id.clone(),
        topic.clone(),
        event_payload(&command.payload)?,
    )
    .with_target(event_target(&command.target)?);

    if let Some(request_id) = &command.request_id {
        event = event.with_request_id(RequestId::parse(request_id)?);
    }
    if let Some(generation_id) = &command.generation_id {
        event = event.with_generation_id(GenerationId::parse(generation_id)?);
    }
    if command.correlation_id.is_some() || command.causation_id.is_some() {
        event = event.with_trace(TraceContext::new(
            command
                .correlation_id
                .as_deref()
                .map(EventId::parse)
                .transpose()?,
            command
                .causation_id
                .as_deref()
                .map(EventId::parse)
                .transpose()?,
        ));
    }

    let (receipt, backend_kind, backend_path) =
        publish_to_bus(event, command.durable_backend.as_deref())?;
    let (target_kind, target_value) = target_summary(&receipt.target);
    Ok(EmitReport {
        backend_kind,
        backend_path,
        event_id: receipt.event_id.as_str().to_owned(),
        topic: receipt.topic.as_str().to_owned(),
        sequence: receipt.sequence,
        target_kind,
        target_value,
        payload_kind: payload_summary.0,
        payload_size: payload_summary.1,
        request_id: command.request_id.clone(),
        generation_id: command.generation_id.clone(),
        correlation_id: command.correlation_id.clone(),
        causation_id: command.causation_id.clone(),
    })
}

/// 根据可选根目录选择 durable 或内存 EventBus，并返回统一发布回执与后端描述。
fn publish_to_bus(
    event: Event,
    durable_backend: Option<&Path>,
) -> Result<(EventReceipt, String, Option<String>), EvaError> {
    if let Some(root) = durable_backend {
        let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(root))?;
        let mut bus = DurableEventBus::open(backend.layout())?;
        let receipt = bus.publish(event)?;
        Ok((
            receipt,
            "durable".to_owned(),
            Some(root.display().to_string()),
        ))
    } else {
        let mut bus = InMemoryEventBus::new();
        let receipt = bus.publish(event)?;
        Ok((receipt, "in_memory".to_owned(), None))
    }
}

/// 将 CLI payload 转换为核心不透明 payload；hex 失败返回参数错误。
fn event_payload(payload: &EmitPayload) -> Result<EventPayload, EvaError> {
    match payload {
        EmitPayload::Empty => Ok(EventPayload::empty()),
        EmitPayload::Text(value) => Ok(EventPayload::text(value.clone())),
        EmitPayload::BytesHex(value) => Ok(EventPayload::bytes(hex_decode(value)?)),
    }
}

/// 将 CLI 目标文本解析为对应强类型核心目标。
fn event_target(target: &EmitTarget) -> Result<EventTarget, EvaError> {
    match target {
        EmitTarget::Broadcast => Ok(EventTarget::Broadcast),
        EmitTarget::Agent(value) => Ok(EventTarget::Agent(AgentId::parse(value)?)),
        EmitTarget::Capability(value) => Ok(EventTarget::Capability(CapabilityName::parse(value)?)),
        EmitTarget::Adapter(value) => Ok(EventTarget::Adapter(AdapterId::parse(value)?)),
    }
}

/// 计算输出使用的 payload 类型和真实字节数，不暴露 payload 内容。
fn payload_summary(payload: &EmitPayload) -> Result<(String, usize), EvaError> {
    match payload {
        EmitPayload::Empty => Ok(("empty".to_owned(), 0)),
        EmitPayload::Text(value) => Ok(("text".to_owned(), value.len())),
        EmitPayload::BytesHex(value) => Ok(("bytes".to_owned(), hex_decode(value)?.len())),
    }
}

/// 将核心目标投影为稳定 kind/value 对。
fn target_summary(target: &EventTarget) -> (String, Option<String>) {
    match target {
        EventTarget::Broadcast => ("broadcast".to_owned(), None),
        EventTarget::Agent(value) => ("agent".to_owned(), Some(value.as_str().to_owned())),
        EventTarget::Capability(value) => {
            ("capability".to_owned(), Some(value.as_str().to_owned()))
        }
        EventTarget::Adapter(value) => ("adapter".to_owned(), Some(value.as_str().to_owned())),
    }
}

/// 输出发布回执、后端、目标和 payload 摘要。
fn write_emit<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    report: &EmitReport,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Event emitted").map_err(write_error_kind)?;
            writeln!(writer, "event: {}", report.event_id).map_err(write_error_kind)?;
            writeln!(writer, "topic: {}", report.topic).map_err(write_error_kind)?;
            writeln!(writer, "backend: {}", report.backend_kind).map_err(write_error_kind)?;
            if let Some(path) = &report.backend_path {
                writeln!(writer, "backend_path: {path}").map_err(write_error_kind)?;
            }
            writeln!(writer, "sequence: {}", report.sequence).map_err(write_error_kind)?;
            writeln!(writer, "target: {}", target_text(report)).map_err(write_error_kind)?;
            writeln!(writer, "payload: {}", report.payload_kind).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope("emit", EXIT_OK, &emit_report_json(report), trace)
        )
        .map_err(write_error_kind),
    }
}

/// 将目标 kind/value 组合为紧凑文本表示。
fn target_text(report: &EmitReport) -> String {
    match &report.target_value {
        Some(value) => format!("{}:{value}", report.target_kind),
        None => report.target_kind.clone(),
    }
}

/// 将发布报告编码为稳定 JSON，不包含原始 payload 内容。
fn emit_report_json(report: &EmitReport) -> String {
    format!(
        "{{\"status\":\"published\",\"backend\":{{\"kind\":{},\"path\":{}}},\"event_id\":{},\"topic\":{},\"sequence\":{},\"target\":{{\"kind\":{},\"value\":{}}},\"payload\":{{\"kind\":{},\"size\":{}}},\"metadata\":{{\"request_id\":{},\"generation_id\":{},\"correlation_id\":{},\"causation_id\":{}}}}}",
        json_string(&report.backend_kind),
        option_json(report.backend_path.as_deref()),
        json_string(&report.event_id),
        json_string(&report.topic),
        report.sequence,
        json_string(&report.target_kind),
        option_json(report.target_value.as_deref()),
        json_string(&report.payload_kind),
        report.payload_size,
        option_json(report.request_id.as_deref()),
        option_json(report.generation_id.as_deref()),
        option_json(report.correlation_id.as_deref()),
        option_json(report.causation_id.as_deref())
    )
}

/// 以 Unix 时间秒和纳秒生成进程内默认事件 ID；系统时钟异常时安全回退为 epoch。
fn default_event_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("evt-cli-emit-{}-{}", now.as_secs(), now.subsec_nanos())
}

/// 设置只允许出现一次的选项，重复时返回明确参数错误。
fn set_once(slot: &mut Option<String>, value: String, name: &'static str) -> Result<(), EvaError> {
    if slot.is_some() {
        return Err(EvaError::invalid_argument(format!("duplicate {name}")));
    }
    *slot = Some(value);
    Ok(())
}

/// 从默认广播替换为一个显式目标，并拒绝多个目标选项组合。
fn replace_target(current: EmitTarget, next: EmitTarget) -> Result<EmitTarget, EvaError> {
    if !matches!(current, EmitTarget::Broadcast) {
        return Err(EvaError::invalid_argument(
            "emit accepts only one explicit target",
        ));
    }
    Ok(next)
}

/// 设置唯一 payload 形式；即使两次都指定 empty 也视为重复输入。
fn set_payload(
    current: &mut EmitPayload,
    payload_set: &mut bool,
    next: EmitPayload,
) -> Result<(), EvaError> {
    if *payload_set {
        return Err(EvaError::invalid_argument(
            "emit accepts only one payload option",
        ));
    }
    *current = next;
    *payload_set = true;
    Ok(())
}

/// 在发布前对显式目标执行相应强类型校验。
fn validate_target(target: &EmitTarget) -> Result<(), EvaError> {
    match target {
        EmitTarget::Broadcast => Ok(()),
        EmitTarget::Agent(value) => AgentId::parse(value).map(|_| ()),
        EmitTarget::Capability(value) => CapabilityName::parse(value).map(|_| ()),
        EmitTarget::Adapter(value) => AdapterId::parse(value).map(|_| ()),
    }
}

/// 在发布前验证二进制 hex payload；其他 payload 无额外格式约束。
fn validate_payload(payload: &EmitPayload) -> Result<(), EvaError> {
    if let EmitPayload::BytesHex(value) = payload {
        hex_decode(value)?;
    }
    Ok(())
}

/// 将偶数长度 ASCII hex 解码为字节；失败保留 payload 上下文且不做宽松修正。
fn hex_decode(value: &str) -> Result<Vec<u8>, EvaError> {
    if !value.len().is_multiple_of(2) {
        return Err(
            EvaError::invalid_argument("hex payload must have an even length")
                .with_context("payload", value),
        );
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let mut chars = value.as_bytes().chunks_exact(2);
    for pair in &mut chars {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

/// 将一个 ASCII 十六进制字符转换为 4 bit 值。
fn hex_nibble(value: u8) -> Result<u8, EvaError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(EvaError::invalid_argument(
            "hex payload may only contain hexadecimal characters",
        )),
    }
}
