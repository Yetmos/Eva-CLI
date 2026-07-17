//! JSON Schema 路径、受支持规则与配置对齐验证。
//! JSON Schema path and alignment helpers.

use crate::eva_yaml::ConfigRoots;
use eva_core::EvaError;
use serde_yaml::{Mapping, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// 本模块的架构职责：加载 Schema，并在强类型解析前验证配置结构和字段位置。
/// Architectural responsibility for this module.
pub const RESPONSIBILITY: &str = "load schemas and validate parsed configuration structures";

/// Schema 根目录下约定的配置 Schema 文件路径。
/// Expected schema file paths under a schema root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaPaths {
    /// 主 `eva.yaml` Schema。
    pub eva: PathBuf,
    /// Agent 清单 Schema。
    pub agent: PathBuf,
    /// Adapter 清单 Schema。
    pub adapter: PathBuf,
    /// Capability 清单 Schema。
    pub capability: PathBuf,
    /// Policy 文档 Schema。
    pub policy: PathBuf,
    /// Topic 路由 Schema。
    pub routes: PathBuf,
}

/// 根据已解析配置根返回固定 Schema 文件路径。
/// Returns the canonical schema file paths for a resolved config root.
pub fn schema_paths(roots: &ConfigRoots) -> SchemaPaths {
    SchemaPaths {
        eva: roots.schema_dir.join("eva.schema.json"),
        agent: roots.schema_dir.join("agent.schema.json"),
        adapter: roots.schema_dir.join("adapter.schema.json"),
        capability: roots.schema_dir.join("capability.schema.json"),
        policy: roots.schema_dir.join("policy.schema.json"),
        routes: roots.schema_dir.join("routes.schema.json"),
    }
}

/// 按配置类别为主配置和全部拆分文件运行对应 Schema 验证。
///
/// 任一文件失败即停止，不继续构建部分强类型项目配置；输入文件顺序由上层排序，
/// 因此首个失败位置稳定。
pub fn validate_config_files_with_schemas(
    roots: &ConfigRoots,
    eva_config_path: &Path,
    agent_paths: &[PathBuf],
    adapter_paths: &[PathBuf],
    capability_paths: &[PathBuf],
    policy_paths: &[PathBuf],
    route_path: &Path,
) -> Result<(), EvaError> {
    let schemas = schema_paths(roots);
    validate_yaml_file_with_schema(eva_config_path, &schemas.eva, "eva.yaml")?;
    for path in agent_paths {
        validate_yaml_file_with_schema(path, &schemas.agent, "Agent manifest")?;
    }
    for path in adapter_paths {
        validate_yaml_file_with_schema(path, &schemas.adapter, "Adapter manifest")?;
    }
    for path in capability_paths {
        validate_yaml_file_with_schema(path, &schemas.capability, "Capability manifest")?;
    }
    for path in policy_paths {
        validate_yaml_file_with_schema(path, &schemas.policy, "Policy document")?;
    }
    validate_yaml_file_with_schema(route_path, &schemas.routes, "Topic routes")
}

/// 读取一份 YAML/JSON 数据和 Schema，并从根字段递归验证。
pub fn validate_yaml_file_with_schema(
    data_path: &Path,
    schema_path: &Path,
    config_type: &'static str,
) -> Result<(), EvaError> {
    let schema = read_schema(schema_path, config_type)?;
    let data = read_data(data_path, config_type)?;
    let context = SchemaValidationContext {
        config_type,
        data_path,
        schema_path,
    };
    validate_schema_node(&data, &schema, &context, &FieldPath::root())
}

pub(crate) fn validate_yaml_value_with_schema(
    data: &Value,
    data_path: &Path,
    schema_path: &Path,
    config_type: &'static str,
) -> Result<(), EvaError> {
    let schema = read_schema(schema_path, config_type)?;
    let context = SchemaValidationContext {
        config_type,
        data_path,
        schema_path,
    };
    validate_schema_node(data, &schema, &context, &FieldPath::root())
}

/// `eva-config` 当前接受的 Adapter 传输值。
/// Adapter transport values currently accepted by `eva-config`.
pub const ADAPTER_TRANSPORT_VALUES: &[&str] = &[
    "builtin",
    "stdio",
    "http",
    "eventbus",
    "mcp",
    "skill",
    "hardware",
    "lua_capability",
];

/// `eva-config` 当前接受的 Capability 类别值。
/// Capability kind values currently accepted by `eva-config`.
pub const CAPABILITY_KIND_VALUES: &[&str] =
    &["adapter_capability", "lua_capability", "mcp_tool", "skill"];

/// `eva-config` 当前接受的 Topic 路由投递模式。
/// Topic route delivery values currently accepted by `eva-config`.
pub const ROUTE_DELIVERY_VALUES: &[&str] = &["fanout", "compete"];

/// 贯穿递归验证的配置类型和源文件上下文。
struct SchemaValidationContext<'a> {
    /// 被验证配置类别。
    config_type: &'static str,
    /// 数据文件路径。
    data_path: &'a Path,
    /// Schema 文件路径。
    schema_path: &'a Path,
}

/// 以点号属性和数组下标表示的稳定字段位置。
#[derive(Debug, Clone, PartialEq, Eq)]
struct FieldPath(
    /// 不含根展示符 `$` 的内部路径；空串表示根，其余值由属性名和数组下标组成。
    String,
);

impl FieldPath {
    /// 创建用 `$` 展示的根字段路径。
    fn root() -> Self {
        Self(String::new())
    }

    /// 返回追加对象属性后的字段路径。
    fn property(&self, name: &str) -> Self {
        if self.0.is_empty() {
            Self(name.to_owned())
        } else {
            Self(format!("{}.{}", self.0, name))
        }
    }

    /// 返回追加数组下标后的字段路径。
    fn index(&self, index: usize) -> Self {
        Self(format!("{}[{index}]", self.as_str()))
    }

    /// 返回错误上下文使用的字段位置，空内部值映射为 `$`。
    fn as_str(&self) -> &str {
        if self.0.is_empty() {
            "$"
        } else {
            &self.0
        }
    }
}

/// 读取 JSON Schema；JSON 是 YAML 子集，因此复用 serde_yaml 值模型。
fn read_schema(schema_path: &Path, config_type: &'static str) -> Result<Value, EvaError> {
    let content = fs::read_to_string(schema_path).map_err(|error| {
        EvaError::not_found("failed to read JSON Schema")
            .with_context("config_type", config_type)
            .with_context("schema_path", schema_path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    serde_yaml::from_str(&content).map_err(|error| {
        EvaError::invalid_argument("failed to parse JSON Schema")
            .with_context("config_type", config_type)
            .with_context("schema_path", schema_path.display().to_string())
            .with_context("schema_error", error.to_string())
            .with_context(
                "suggestion",
                "repair the JSON schema file before validating config",
            )
    })
}

/// 读取待验证 YAML，并保留语法错误行列信息。
fn read_data(data_path: &Path, config_type: &'static str) -> Result<Value, EvaError> {
    let content = fs::read_to_string(data_path).map_err(|error| {
        EvaError::not_found("failed to read configuration file")
            .with_context("config_type", config_type)
            .with_context("path", data_path.display().to_string())
            .with_context("io_error", error.to_string())
    })?;
    serde_yaml::from_str(&content).map_err(|error| {
        let mut eva_error = EvaError::invalid_argument("failed to parse YAML")
            .with_context("config_type", config_type)
            .with_context("path", data_path.display().to_string())
            .with_context("yaml_error", error.to_string())
            .with_context("suggestion", "fix YAML syntax before schema validation");
        if let Some(location) = error.location() {
            eva_error = eva_error
                .with_context("line", location.line().to_string())
                .with_context("column", location.column().to_string());
        }
        eva_error
    })
}

/// 按固定顺序递归验证当前 Schema 节点支持的规则。
///
/// 先验证 type/enum/pattern/minProperties，再验证对象 required/properties/additionalProperties，
/// 最后递归数组 items。首个违规立即返回带字段路径的错误；未实现的 Schema 功能不会
/// 被假装支持，pattern 仅允许显式白名单。
fn validate_schema_node(
    data: &Value,
    schema: &Value,
    context: &SchemaValidationContext<'_>,
    field: &FieldPath,
) -> Result<(), EvaError> {
    let Some(schema) = schema.as_mapping() else {
        return Err(schema_violation(
            context,
            field,
            "schema",
            "schema node must be an object",
            "repair the JSON schema node",
        ));
    };

    validate_type(data, schema, context, field)?;
    validate_enum(data, schema, context, field)?;
    validate_pattern(data, schema, context, field)?;
    validate_min_properties(data, schema, context, field)?;

    if let Some(mapping) = data.as_mapping() {
        validate_required(mapping, schema, context, field)?;
        validate_properties(mapping, schema, context, field)?;
        validate_additional_properties(mapping, schema, context, field)?;
    }
    if let Some(items_schema) = schema_get(schema, "items") {
        if let Some(items) = data.as_sequence() {
            for (index, item) in items.iter().enumerate() {
                validate_schema_node(item, items_schema, context, &field.index(index))?;
            }
        }
    }

    Ok(())
}

/// 验证单一或联合 `type` 规则。
fn validate_type(
    data: &Value,
    schema: &Mapping,
    context: &SchemaValidationContext<'_>,
    field: &FieldPath,
) -> Result<(), EvaError> {
    let Some(expected) = schema_get(schema, "type") else {
        return Ok(());
    };
    let matches = match expected {
        Value::String(expected) => value_matches_type(data, expected),
        Value::Sequence(expected) => expected
            .iter()
            .filter_map(Value::as_str)
            .any(|expected| value_matches_type(data, expected)),
        _ => true,
    };
    if matches {
        return Ok(());
    }
    Err(schema_violation(
        context,
        field,
        "type",
        format!(
            "schema type mismatch: expected {}, got {}",
            schema_type_label(expected),
            value_type(data)
        ),
        "set the field to a value with the schema-declared type",
    ))
}

/// 验证值与 Schema 枚举中的任一完整值相等。
fn validate_enum(
    data: &Value,
    schema: &Mapping,
    context: &SchemaValidationContext<'_>,
    field: &FieldPath,
) -> Result<(), EvaError> {
    let Some(values) = schema_get(schema, "enum").and_then(Value::as_sequence) else {
        return Ok(());
    };
    if values.iter().any(|value| value == data) {
        return Ok(());
    }
    Err(schema_violation(
        context,
        field,
        "enum",
        "value is not allowed by schema enum",
        format!("use one of: {}", enum_values(values)),
    ))
}

/// 对字符串应用当前验证器明确支持的 pattern。
fn validate_pattern(
    data: &Value,
    schema: &Mapping,
    context: &SchemaValidationContext<'_>,
    field: &FieldPath,
) -> Result<(), EvaError> {
    let Some(pattern) = schema_get(schema, "pattern").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(value) = data.as_str() else {
        return Ok(());
    };
    if matches_supported_pattern(pattern, value)? {
        return Ok(());
    }
    Err(schema_violation(
        context,
        field,
        "pattern",
        "string does not match schema pattern",
        format!("adjust the field to match pattern {pattern}"),
    ))
}

/// 验证对象至少包含指定属性数量。
fn validate_min_properties(
    data: &Value,
    schema: &Mapping,
    context: &SchemaValidationContext<'_>,
    field: &FieldPath,
) -> Result<(), EvaError> {
    let Some(minimum) = schema_get(schema, "minProperties").and_then(Value::as_u64) else {
        return Ok(());
    };
    let Some(mapping) = data.as_mapping() else {
        return Ok(());
    };
    if mapping.len() as u64 >= minimum {
        return Ok(());
    }
    Err(schema_violation(
        context,
        field,
        "minProperties",
        format!("object must contain at least {minimum} propertie(s)"),
        "add the required policy/configuration properties",
    ))
}

/// 验证对象所有必填属性存在，并定位到缺失属性路径。
fn validate_required(
    data: &Mapping,
    schema: &Mapping,
    context: &SchemaValidationContext<'_>,
    field: &FieldPath,
) -> Result<(), EvaError> {
    let Some(required) = schema_get(schema, "required").and_then(Value::as_sequence) else {
        return Ok(());
    };
    for required in required.iter().filter_map(Value::as_str) {
        if !data.contains_key(Value::String(required.to_owned())) {
            return Err(schema_violation(
                context,
                &field.property(required),
                "required",
                "required field is missing",
                format!("add required field `{required}`"),
            ));
        }
    }
    Ok(())
}

/// 对数据中存在的已声明属性递归验证其子 Schema。
fn validate_properties(
    data: &Mapping,
    schema: &Mapping,
    context: &SchemaValidationContext<'_>,
    field: &FieldPath,
) -> Result<(), EvaError> {
    let Some(properties) = schema_get(schema, "properties").and_then(Value::as_mapping) else {
        return Ok(());
    };
    for (name, property_schema) in properties {
        let Some(name) = name.as_str() else {
            continue;
        };
        if let Some(value) = data.get(Value::String(name.to_owned())) {
            validate_schema_node(value, property_schema, context, &field.property(name))?;
        }
    }
    Ok(())
}

/// 当 `additionalProperties=false` 时拒绝未声明键和非字符串键。
fn validate_additional_properties(
    data: &Mapping,
    schema: &Mapping,
    context: &SchemaValidationContext<'_>,
    field: &FieldPath,
) -> Result<(), EvaError> {
    if !matches!(
        schema_get(schema, "additionalProperties"),
        Some(Value::Bool(false))
    ) {
        return Ok(());
    }
    let Some(properties) = schema_get(schema, "properties").and_then(Value::as_mapping) else {
        return Ok(());
    };
    for key in data.keys() {
        let Some(key) = key.as_str() else {
            return Err(schema_violation(
                context,
                field,
                "additionalProperties",
                "object contains a non-string property name",
                "remove the unsupported property or update the schema",
            ));
        };
        if !properties.contains_key(Value::String(key.to_owned())) {
            return Err(schema_violation(
                context,
                &field.property(key),
                "additionalProperties",
                "object contains a property not allowed by schema",
                "remove the unsupported property or update the schema",
            ));
        }
    }
    Ok(())
}

/// 从 Schema Mapping 读取字符串键。
fn schema_get<'a>(schema: &'a Mapping, key: &str) -> Option<&'a Value> {
    schema.get(Value::String(key.to_owned()))
}

/// 判断 YAML 值是否符合受支持的 JSON Schema 类型名。
fn value_matches_type(value: &Value, expected: &str) -> bool {
    match expected {
        "object" => value.as_mapping().is_some(),
        "array" => value.as_sequence().is_some(),
        "string" => value.as_str().is_some(),
        "boolean" => value.as_bool().is_some(),
        "number" => {
            value.as_f64().is_some() || value.as_i64().is_some() || value.as_u64().is_some()
        }
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "null" => matches!(value, Value::Null),
        _ => true,
    }
}

/// 返回错误消息使用的实际 YAML 值类型名。
fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Sequence(_) => "array",
        Value::Mapping(_) => "object",
        Value::Tagged(_) => "tagged",
    }
}

/// 将单一或联合 Schema 类型渲染为稳定标签。
fn schema_type_label(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Sequence(values) => values
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("|"),
        _ => "unknown".to_owned(),
    }
}

/// 将字符串枚举渲染为补救提示列表。
fn enum_values(values: &[Value]) -> String {
    values
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 执行白名单中的简单 Schema 正则语义。
///
/// 不引入完整正则引擎；遇到未知 pattern 明确返回 Unsupported，防止跳过 Schema 约束
/// 并错误接受配置。
fn matches_supported_pattern(pattern: &str, value: &str) -> Result<bool, EvaError> {
    match pattern {
        "^/.+" => Ok(value.starts_with('/') && value.len() > 1),
        "^[a-zA-Z0-9][a-zA-Z0-9_.-]*$" => Ok(matches_stable_id_pattern(value)),
        _ => Err(
            EvaError::unsupported("schema pattern is not supported by eva-config validator")
                .with_context("pattern", pattern)
                .with_context(
                    "suggestion",
                    "extend eva-config schema pattern support before using this schema",
                ),
        ),
    }
}

/// 验证标识首字符为 ASCII 字母数字，其余为字母数字或 `_.-`。
fn matches_stable_id_pattern(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

/// 构造包含数据、Schema、字段、规则和修复建议的统一违规错误。
fn schema_violation(
    context: &SchemaValidationContext<'_>,
    field: &FieldPath,
    rule: &str,
    message: impl Into<String>,
    suggestion: impl Into<String>,
) -> EvaError {
    EvaError::invalid_argument(message)
        .with_context("config_type", context.config_type)
        .with_context("path", context.data_path.display().to_string())
        .with_context("schema_path", context.schema_path.display().to_string())
        .with_context("field", field.as_str())
        .with_context("schema_rule", rule)
        .with_context("suggestion", suggestion)
}

#[cfg(test)]
/// Schema 路径、枚举对齐和稳定错误位置测试。
mod tests {
    use super::*;
    use crate::eva_yaml::load_eva_config;
    use crate::routes::RouteDelivery;
    use eva_core::ErrorKind;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 返回包含示例 Schema 的工作区根目录。
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    /// 验证约定 Schema 路径均指向示例文件。
    fn schema_paths_point_to_sample_schemas() {
        let root = workspace_root();
        let config = load_eva_config(root.join("config").join("eva.yaml")).unwrap();
        let roots = config.config.resolve_against(&root);
        let paths = schema_paths(&roots);

        assert!(paths.eva.is_file());
        assert!(paths.agent.is_file());
        assert!(paths.adapter.is_file());
        assert!(paths.capability.is_file());
        assert!(paths.policy.is_file());
        assert!(paths.routes.is_file());
    }

    #[test]
    /// 验证公开枚举值与强类型解析器保持一致。
    fn enum_values_match_supported_manifest_values() {
        assert!(ADAPTER_TRANSPORT_VALUES.contains(&"stdio"));
        assert!(ADAPTER_TRANSPORT_VALUES.contains(&"mcp"));
        assert!(CAPABILITY_KIND_VALUES.contains(&"adapter_capability"));
        assert!(CAPABILITY_KIND_VALUES.contains(&"mcp_tool"));
        assert_eq!(
            ROUTE_DELIVERY_VALUES
                .iter()
                .map(|value| RouteDelivery::parse(value).unwrap().as_str())
                .collect::<Vec<_>>(),
            ROUTE_DELIVERY_VALUES
        );
    }

    #[test]
    /// 验证缺失必填字段报告稳定字段位置和建议。
    fn validator_reports_required_field_with_stable_location() {
        let root = test_temp_dir("schema-required");
        let schema = root.join("agent.schema.json");
        let data = root.join("agent.yaml");
        fs::write(
            &schema,
            r#"{
              "type": "object",
              "required": ["id", "enabled"],
              "properties": {
                "id": { "type": "string" },
                "enabled": { "type": "boolean" }
              },
              "additionalProperties": true
            }"#,
        )
        .unwrap();
        fs::write(&data, "id: root-agent\n").unwrap();

        let error = validate_yaml_file_with_schema(&data, &schema, "Agent manifest").unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert_context(&error, "field", "enabled");
        assert_context(&error, "schema_rule", "required");
        assert_context(&error, "suggestion", "add required field `enabled`");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// 验证嵌套路由中的额外属性被精确定位并拒绝。
    fn validator_rejects_additional_properties_in_nested_route_rule() {
        let root = test_temp_dir("schema-additional");
        let schema = root.join("routes.schema.json");
        let data = root.join("routes.yaml");
        fs::write(
            &schema,
            r#"{
              "type": "object",
              "required": ["routes"],
              "properties": {
                "routes": {
                  "type": "array",
                  "items": {
                    "type": "object",
                    "required": ["pattern", "delivery", "agents"],
                    "properties": {
                      "pattern": { "type": "string", "pattern": "^/.+" },
                      "delivery": { "type": "string", "enum": ["fanout", "compete"] },
                      "agents": { "type": "array", "items": { "type": "string" } }
                    },
                    "additionalProperties": false
                  }
                }
              },
              "additionalProperties": false
            }"#,
        )
        .unwrap();
        fs::write(
            &data,
            r#"routes:
  - pattern: /sys
    delivery: fanout
    agents:
      - root-agent
    surprise: true
"#,
        )
        .unwrap();

        let error = validate_yaml_file_with_schema(&data, &schema, "Topic routes").unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert_context(&error, "field", "routes[0].surprise");
        assert_context(&error, "schema_rule", "additionalProperties");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// 验证枚举不匹配错误包含允许值提示。
    fn validator_rejects_schema_enum_mismatch() {
        let root = test_temp_dir("schema-enum");
        let schema = root.join("adapter.schema.json");
        let data = root.join("adapter.yaml");
        fs::write(
            &schema,
            r#"{
              "type": "object",
              "required": ["transport"],
              "properties": {
                "transport": { "type": "string", "enum": ["stdio", "http"] }
              },
              "additionalProperties": true
            }"#,
        )
        .unwrap();
        fs::write(&data, "transport: mcp\n").unwrap();

        let error = validate_yaml_file_with_schema(&data, &schema, "Adapter manifest").unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        assert_context(&error, "field", "transport");
        assert_context(&error, "schema_rule", "enum");
        assert_context(&error, "suggestion", "use one of: stdio, http");

        fs::remove_dir_all(root).unwrap();
    }

    /// 断言错误上下文包含指定键值。
    fn assert_context(error: &EvaError, key: &str, value: &str) {
        assert!(
            error
                .context()
                .entries()
                .iter()
                .any(|(entry_key, entry_value)| entry_key == key && entry_value == value),
            "missing context {key}={value:?}: {:?}",
            error.context().entries()
        );
    }

    /// 创建进程和时间戳隔离的临时测试目录。
    fn test_temp_dir(name: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("eva-config-{name}-{}-{now}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
