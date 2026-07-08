use super::{
    display_path, json_array, json_string, option_json, parse_common_options, parse_usize_option,
    required_option, success_envelope, trace_for, write_command_error, write_error_kind,
    CommonOptions, OutputFormat, EXIT_OK,
};
use eva_config::{load_project_config, ProjectConfig};
use eva_core::{AgentId, EvaError, RequestId};
use eva_memory::{
    BuiltContext, ContextBudget, ContextBuilder, ContextRequest, FileSystemKnowledgeStore,
    FileSystemMemoryStore, InMemoryKnowledgeService, InMemoryMemoryService, KnowledgeId,
    KnowledgeItem, KnowledgeSearchResult, KnowledgeSource, MemoryCompression, MemoryRecord,
    MemoryRetention, MemoryWrite,
};
use eva_observability::TraceFields;
use eva_storage::{DurableBackendOptions, FileSystemDurableBackend};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MemoryCommand {
    Context(MemoryContextOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MemoryContextOptions {
    common: CommonOptions,
    agent_id: String,
    query: String,
    request_id: String,
    private_limit: usize,
    global_limit: usize,
    knowledge_limit: usize,
    durable_backend: Option<PathBuf>,
}

pub(super) fn parse_memory_command(args: &[String]) -> Result<MemoryCommand, EvaError> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| EvaError::invalid_argument("missing memory subcommand"))?;
    match subcommand.as_str() {
        "context" => Ok(MemoryCommand::Context(parse_memory_context_options(rest)?)),
        value => {
            Err(EvaError::unsupported("unknown memory subcommand")
                .with_context("subcommand", value))
        }
    }
}

pub(super) fn execute_memory<W, E>(
    command: MemoryCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> Result<i32, EvaError>
where
    W: Write,
    E: Write,
{
    match command {
        MemoryCommand::Context(options) => {
            let trace = trace_for("cli.memory.context");
            match load_project_config(&options.common.project_root)
                .and_then(|project| build_memory_context(&project, &options))
            {
                Ok(context) => {
                    write_memory_context(stdout, options.common.output, &context, &trace)?;
                    Ok(EXIT_OK)
                }
                Err(error) => write_command_error(
                    stderr,
                    options.common.output,
                    "memory.context",
                    &error,
                    &trace,
                ),
            }
        }
    }
}

fn parse_memory_context_options(args: &[String]) -> Result<MemoryContextOptions, EvaError> {
    let mut passthrough = Vec::new();
    let mut agent_id = "root-agent".to_owned();
    let mut query = "memory".to_owned();
    let mut request_id = "req-memory-1".to_owned();
    let mut private_limit = 8;
    let mut global_limit = 8;
    let mut knowledge_limit = 8;
    let mut durable_backend = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--agent" | "--agent-id" => {
                index += 1;
                agent_id = required_option(args, index, "agent option")?.clone();
            }
            "--query" => {
                index += 1;
                query = required_option(args, index, "query option")?.clone();
            }
            "--request-id" | "--task-id" | "--task" => {
                index += 1;
                request_id = required_option(args, index, "request id option")?.clone();
            }
            "--private-limit" => {
                index += 1;
                private_limit = parse_usize_option(
                    "private_limit",
                    required_option(args, index, "private limit option")?,
                )?;
            }
            "--global-limit" => {
                index += 1;
                global_limit = parse_usize_option(
                    "global_limit",
                    required_option(args, index, "global limit option")?,
                )?;
            }
            "--knowledge-limit" => {
                index += 1;
                knowledge_limit = parse_usize_option(
                    "knowledge_limit",
                    required_option(args, index, "knowledge limit option")?,
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
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }

    AgentId::parse(&agent_id)?;
    RequestId::parse(&request_id)?;
    Ok(MemoryContextOptions {
        common: parse_common_options(&passthrough)?,
        agent_id,
        query,
        request_id,
        private_limit,
        global_limit,
        knowledge_limit,
        durable_backend,
    })
}

fn build_memory_context(
    project: &ProjectConfig,
    options: &MemoryContextOptions,
) -> Result<BuiltContext, EvaError> {
    let agent_id = AgentId::parse(&options.agent_id)?;
    if !project.agents.iter().any(|agent| agent.id == agent_id) {
        return Err(
            EvaError::not_found("Agent does not exist for memory context")
                .with_context("agent_id", agent_id.as_str()),
        );
    }
    let request_id = RequestId::parse(&options.request_id)?;
    let now_ms = current_time_ms();
    let memory_writes = seeded_memory_writes(project, &agent_id, &request_id, now_ms);
    let knowledge_items = seeded_knowledge_items(request_id.clone())?;
    let (memory, knowledge) = if let Some(root) = &options.durable_backend {
        let backend = FileSystemDurableBackend::open(DurableBackendOptions::read_write(root))?;
        let mut memory_store = FileSystemMemoryStore::from_durable_layout(backend.layout());
        for write in memory_writes {
            memory_store.write(write)?;
        }
        let mut knowledge_store = FileSystemKnowledgeStore::from_durable_layout(backend.layout());
        for item in &knowledge_items {
            knowledge_store.write_item(item)?;
        }
        (memory_store.load()?, knowledge_store.load_index()?)
    } else {
        let mut memory = InMemoryMemoryService::new();
        for write in memory_writes {
            memory.write(write)?;
        }
        let mut knowledge = InMemoryKnowledgeService::new();
        for item in knowledge_items {
            knowledge.index(item)?;
        }
        (memory, knowledge)
    };
    ContextBuilder::new(&memory, &knowledge).build(
        ContextRequest::new(request_id, agent_id, options.query.clone())
            .with_budget(ContextBudget {
                private_memory: options.private_limit,
                global_memory: options.global_limit,
                knowledge: options.knowledge_limit,
            })
            .with_now_ms(now_ms),
    )
}

fn seeded_memory_writes(
    project: &ProjectConfig,
    agent_id: &AgentId,
    request_id: &RequestId,
    now_ms: u128,
) -> Vec<MemoryWrite> {
    vec![
        MemoryWrite::private(
            agent_id.clone(),
            "agent.identity",
            format!("agent {agent_id} owns this private context"),
        )
        .with_request_id(request_id.clone())
        .with_created_at_ms(now_ms),
        MemoryWrite::private(
            agent_id.clone(),
            "project.agent_count",
            project.agents.len().to_string(),
        )
        .with_request_id(request_id.clone())
        .with_created_at_ms(now_ms),
        MemoryWrite::private(agent_id.clone(), "session.secret", "token=memory-secret")
            .with_request_id(request_id.clone())
            .with_ttl_ms(now_ms, 60_000)
            .with_compression(MemoryCompression::RunLength),
        MemoryWrite::private(agent_id.clone(), "expired.note", "password=expired-secret")
            .with_request_id(request_id.clone())
            .with_ttl_ms(now_ms.saturating_sub(10_000), 1),
        MemoryWrite::global(
            "release.checkpoint",
            "V1.9.4 durable memory and knowledge context",
        )
        .with_request_id(request_id.clone())
        .with_retention(MemoryRetention::Persistent)
        .with_created_at_ms(now_ms),
        MemoryWrite::global("workspace.root", display_path(&project.project_root))
            .with_request_id(request_id.clone())
            .with_retention(MemoryRetention::Persistent)
            .with_created_at_ms(now_ms),
    ]
}

fn seeded_knowledge_items(request_id: RequestId) -> Result<Vec<KnowledgeItem>, EvaError> {
    let items = [
        (
            "memory-service",
            "MemoryService",
            "Agent private memory is isolated by agent_id; global memory is shared and audited.",
            "v1.2 memory private global audit context",
        ),
        (
            "context-builder",
            "ContextBuilder",
            "ContextBuilder assembles private memory, global memory, and knowledge under request budgets.",
            "v1.2 context budget knowledge lua controlled api",
        ),
        (
            "knowledge-service",
            "KnowledgeService",
            "KnowledgeService indexes traceable documents and code snippets with lightweight digests.",
            "v1.2 knowledge index search citation digest",
        ),
    ];
    items
        .into_iter()
        .map(|(id, title, summary, content)| {
            Ok(KnowledgeItem::new(
                KnowledgeId::parse(id)?,
                KnowledgeSource::new(format!("docs/{id}.md"), title, content.as_bytes()),
                summary,
                content,
            )?
            .with_tag("v1.2")
            .with_tag("v1.9.4")
            .with_request_id(request_id.clone()))
        })
        .collect()
}

fn current_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn write_memory_context<W: Write>(
    writer: &mut W,
    output: OutputFormat,
    context: &BuiltContext,
    trace: &TraceFields,
) -> Result<(), EvaError> {
    match output {
        OutputFormat::Text => {
            writeln!(writer, "Eva memory context").map_err(write_error_kind)?;
            writeln!(writer, "request: {}", context.request_id).map_err(write_error_kind)?;
            writeln!(writer, "agent: {}", context.agent_id).map_err(write_error_kind)?;
            writeln!(writer, "query: {}", context.query).map_err(write_error_kind)?;
            writeln!(writer, "private_memory: {}", context.memory.len())
                .map_err(write_error_kind)?;
            writeln!(writer, "global_memory: {}", context.global_memory.len())
                .map_err(write_error_kind)?;
            writeln!(writer, "knowledge: {}", context.knowledge.len()).map_err(write_error_kind)
        }
        OutputFormat::Json => writeln!(
            writer,
            "{}",
            success_envelope(
                "memory.context",
                EXIT_OK,
                &memory_context_json(context),
                trace
            )
        )
        .map_err(write_error_kind),
    }
}

fn memory_context_json(context: &BuiltContext) -> String {
    format!(
        "{{\"request_id\":{},\"agent_id\":{},\"query\":{},\"totals\":{{\"items\":{},\"private_memory\":{},\"global_memory\":{},\"knowledge\":{}}},\"memory\":{},\"global_memory\":{},\"knowledge\":{},\"lua_context\":{},\"audit\":{}}}",
        json_string(context.request_id.as_str()),
        json_string(context.agent_id.as_str()),
        json_string(&context.query),
        context.total_items(),
        context.memory.len(),
        context.global_memory.len(),
        context.knowledge.len(),
        json_array(context.memory.iter().map(memory_record_json)),
        json_array(context.global_memory.iter().map(memory_record_json)),
        json_array(context.knowledge.iter().map(knowledge_result_json)),
        lua_context_json(context),
        json_array(context.audit.iter().map(|entry| json_string(entry)))
    )
}

fn memory_record_json(record: &MemoryRecord) -> String {
    format!(
        "{{\"key\":{},\"value\":{},\"visibility\":{},\"owner_agent\":{},\"retention\":{},\"version\":{},\"audit_reason\":{},\"created_at_ms\":{},\"expires_at_ms\":{},\"compression\":{}}}",
        json_string(&record.key),
        json_string(&record.value),
        json_string(record.visibility.as_str()),
        option_json(record.owner_agent.as_ref().map(|agent| agent.as_str())),
        json_string(record.retention.as_str()),
        record.version.0,
        json_string(&record.audit_reason),
        record.created_at_ms,
        record
            .expires_at_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_owned()),
        json_string(record.compression.as_str())
    )
}

fn knowledge_result_json(result: &KnowledgeSearchResult) -> String {
    format!(
        "{{\"id\":{},\"title\":{},\"source\":{},\"digest\":{},\"summary\":{},\"score\":{},\"matched_by\":{}}}",
        json_string(result.item.id.as_str()),
        json_string(&result.item.source.title),
        json_string(&result.item.source.uri),
        json_string(&result.item.source.digest),
        json_string(&result.item.summary),
        result.score,
        json_array(result.matched_by.iter().map(|entry| json_string(entry)))
    )
}

fn lua_context_json(context: &BuiltContext) -> String {
    let snapshot = context.lua_summary();
    format!(
        "{{\"private_memory_count\":{},\"global_memory_count\":{},\"knowledge_count\":{},\"audit\":{}}}",
        snapshot.private_memory_count,
        snapshot.global_memory_count,
        snapshot.knowledge_count,
        json_array(snapshot.audit.iter().map(|entry| json_string(entry)))
    )
}
