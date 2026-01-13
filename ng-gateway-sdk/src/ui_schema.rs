use calamine::Reader;
use rust_xlsxwriter::{
    workbook::Workbook, Color, DataValidation, DataValidationErrorStyle, DataValidationRule,
    Format, FormatPattern, Formula,
};
// Dedicated UI condition/operator types; decoupled from legacy metadata
use crate::DriverError;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::{
    collections::BTreeMap,
    fmt::{self, Display, Formatter},
    io::{Read as IoRead, Seek as IoSeek, Write as IoWrite},
    path::Path,
};

/// Inline i18n helper for driver metadata (UiSchema)
///
/// This macro builds `UiText` values embedded in `DriverSchemas` to provide
/// localized strings without requiring a separate translation bundle. It is
/// intended for labels, descriptions, placeholders, help text, and enum item
/// labels used by the dynamic forms in the UI.
///
/// What it returns
/// - `UiText::Localized` when you pass a map or language aliases
/// - `UiText::Simple` when you pass a plain string
/// - `UiText::Key` when you pass a dictionary key
///
/// When to use what
/// - Prefer `Localized` for authoring-time strings that ship with the schema.
///   Always include an English fallback (e.g. `en-US`).
/// - Use `Key` only if you plan to resolve from a site-level dictionary (stage 2).
///   Keep keys namespaced, e.g. `driver.modbus.connection.port`.
/// - Use `Simple` for quick prototypes or when localization is not needed.
///
/// Supported forms
/// - Map style (explicit locales):
///   `ui_text!({ "en-US" => "Port", "zh-CN" => "端口" })`
/// - Alias style (convenience for common locales):
///   `ui_text!(en = "Port", zh = "端口")`
///   Aliases: `en` -> `en-US`, `zh`/`zh_cn` -> `zh-CN`.
/// - Key style (dictionary indirection):
///   `ui_text!(key = "driver.modbus.connection.port")`
/// - Plain string:
///   `ui_text!("Port")`
///
/// JSON shape (serde tagged enum with `kind`):
/// - Localized:
///   `{ "kind": "Localized", "locales": { "en-US": "Port", "zh-CN": "端口" } }`
/// - Simple:
///   `{ "kind": "Simple", "value": "Port" }`
/// - Key:
///   `{ "kind": "Key", "key": "driver.modbus.connection.port" }`
///
/// UI behavior and fallbacks
/// - The UI resolves `UiText` using the active locale with a fallback chain:
///   current-locale -> base-language -> `en-US` -> `zh-CN` -> any available.
/// - `Key` currently resolves to its key string; site-level dictionaries may
///   override it in a future phase.
///
/// Best practices
/// - Use BCP-47 language tags (e.g., `en-US`, `zh-CN`).
/// - Always provide `en-US`; add other locales as needed.
/// - Keep dictionary keys stable and namespaced.
/// - Use this only for driver form metadata; do not reuse for logs/errors.
#[macro_export]
macro_rules! ui_text {
    // Map style: ui_text!({ "en-US" => "Port", "zh-CN" => "端口" })
    ({ $($key:expr => $val:expr),+ $(,)? }) => {{
        let mut m = ::std::collections::BTreeMap::new();
        $( m.insert($key.to_string(), $val.to_string()); )+
        $crate::UiText::Localized { locales: m }
    }};

    // Aliases: ui_text!(en = "Port", zh = "端口")
    ( en = $en:expr $(, zh = $zh:expr )? $(, zh_cn = $zhcn:expr )? ) => {{
        let mut m = ::std::collections::BTreeMap::new();
        m.insert("en-US".to_string(), $en.to_string());
        $( m.insert("zh-CN".to_string(), $zh.to_string()); )?
        $( m.insert("zh-CN".to_string(), $zhcn.to_string()); )?
        $crate::UiText::Localized { locales: m }
    }};

    // Plain string: ui_text!("Port")
    ($s:expr) => {{
        $crate::UiText::Simple { value: $s.to_string() }
    }};
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DriverSchemas {
    pub channel: Vec<Node>,
    pub device: Vec<Node>,
    pub point: Vec<Node>,
    pub action: Vec<Node>,
}

pub type PluginConfigSchemas = Vec<Node>;

impl DriverSchemas {
    pub fn build_template(&self, entity: FlattenEntity, locale: &str) -> DriverEntityTemplate {
        let mut columns = Vec::new();
        let mut discriminator_keys = Vec::new();

        match entity {
            FlattenEntity::DevicePoints => {
                // Build combined template: device base + device driver + point base + point driver
                // 1) Device base columns
                Self::push_base_columns(FlattenEntity::Device, locale, &mut columns);

                // 2) Device driver config columns (with prefix device_driver_config.)
                Self::flatten_nodes(
                    &self.device,
                    "device_driver_config.",
                    locale,
                    &mut columns,
                    &mut discriminator_keys,
                    None,
                    None,
                );

                // 3) Point base columns
                Self::push_base_columns(FlattenEntity::Point, locale, &mut columns);

                // 4) Point driver config columns (with prefix driver_config.)
                Self::flatten_nodes(
                    &self.point,
                    "driver_config.",
                    locale,
                    &mut columns,
                    &mut discriminator_keys,
                    None,
                    None,
                );
            }
            _ => {
                let (nodes, prefix) = match entity {
                    FlattenEntity::Device => (&self.device, "driver_config."),
                    FlattenEntity::Point => (&self.point, "driver_config."),
                    FlattenEntity::Action => (&self.action, "driver_config."),
                    FlattenEntity::DevicePoints => unreachable!(),
                };

                // 1) Prepend base columns per entity (localized)
                Self::push_base_columns(entity, locale, &mut columns);
                Self::flatten_nodes(
                    nodes,
                    prefix,
                    locale,
                    &mut columns,
                    &mut discriminator_keys,
                    None,
                    None,
                );
            }
        }

        DriverEntityTemplate {
            columns,
            discriminator_keys,
        }
    }

    fn flatten_nodes(
        nodes: &[Node],
        prefix: &str,
        locale: &str,
        out: &mut Vec<FlattenColumn>,
        discriminators: &mut Vec<String>,
        union_discriminator: Option<&str>,
        union_case_value: Option<&serde_json::Value>,
    ) {
        for n in nodes {
            match n {
                Node::Field(field) => {
                    let label = field.label.resolve(locale);
                    let key = format!("{}{}", prefix, field.path);

                    let enum_items_localized = match &field.data_type {
                        UiDataType::Enum { items } => {
                            Some(UiDataType::localize_enum_items(items, locale))
                        }
                        _ => None,
                    };

                    out.push(FlattenColumn {
                        key,
                        label,
                        data_type: field.data_type.clone(),
                        rules: field.rules.clone(),
                        when: field.when.clone(),
                        union_discriminator: union_discriminator.map(|s| s.to_string()),
                        union_case_value: union_case_value.cloned(),
                        enum_items_localized,
                    });
                }
                Node::Group(g) => {
                    Self::flatten_nodes(
                        &g.children,
                        prefix,
                        locale,
                        out,
                        discriminators,
                        union_discriminator,
                        union_case_value,
                    );
                }
                Node::Union(u) => {
                    let discr_key = format!("{}{}", prefix, u.discriminator);
                    discriminators.push(discr_key.clone());

                    // Discriminator column itself (string/numeric depending on downstream usage).
                    // We keep it as String type for template header label equal to discriminator key.
                    out.push(FlattenColumn {
                        key: discr_key.clone(),
                        label: u.discriminator.clone(),
                        data_type: UiDataType::String,
                        rules: None,
                        when: None,
                        union_discriminator: None,
                        union_case_value: None,
                        enum_items_localized: None,
                    });

                    for case in u.mapping.iter() {
                        Self::flatten_nodes(
                            &case.children,
                            prefix,
                            locale,
                            out,
                            discriminators,
                            Some(&discr_key),
                            Some(&case.case_value),
                        );
                    }
                }
            }
        }
    }

    /// Push base columns for each entity before driver schema columns.
    /// This ensures template headers place common fields first and support localization.
    fn push_base_columns(entity: FlattenEntity, locale: &str, out: &mut Vec<FlattenColumn>) {
        match entity {
            FlattenEntity::Device => {
                // device_name
                out.push(FlattenColumn {
                    key: "device_name".to_string(),
                    label: Self::loc(locale, "设备名称", "Device Name"),
                    data_type: UiDataType::String,
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                // device_type (readonly in many flows, but exported for clarity)
                out.push(FlattenColumn {
                    key: "device_type".to_string(),
                    label: Self::loc(locale, "设备类型", "Device Type"),
                    data_type: UiDataType::String,
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
            }
            FlattenEntity::Point => {
                // name
                out.push(FlattenColumn {
                    key: "name".to_string(),
                    label: Self::loc(locale, "名称", "Name"),
                    data_type: UiDataType::String,
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                // key
                out.push(FlattenColumn {
                    key: "key".to_string(),
                    label: Self::loc(locale, "键名", "Key"),
                    data_type: UiDataType::String,
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                // type (DataPointType)
                out.push(FlattenColumn {
                    key: "type".to_string(),
                    label: Self::loc(locale, "类型", "Type"),
                    data_type: UiDataType::Enum {
                        items: Self::enum_items_datapoint_type(locale),
                    },
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                // data_type (DataType)
                out.push(FlattenColumn {
                    key: "data_type".to_string(),
                    label: Self::loc(locale, "数据类型", "Data Type"),
                    data_type: UiDataType::Enum {
                        items: Self::enum_items_data_type(locale),
                    },
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                // access_mode (AccessMode)
                out.push(FlattenColumn {
                    key: "access_mode".to_string(),
                    label: Self::loc(locale, "访问模式", "Access Mode"),
                    data_type: UiDataType::Enum {
                        items: Self::enum_items_access_mode(locale),
                    },
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                // unit
                out.push(FlattenColumn {
                    key: "unit".to_string(),
                    label: Self::loc(locale, "单位", "Unit"),
                    data_type: UiDataType::String,
                    rules: None,
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                // min_value, max_value, scale
                for (k, l_en, l_zh) in [
                    ("min_value", "Min Value", "最小值"),
                    ("max_value", "Max Value", "最大值"),
                    ("scale", "Scale", "缩放比例"),
                ] {
                    out.push(FlattenColumn {
                        key: k.to_string(),
                        label: Self::loc(locale, l_zh, l_en),
                        data_type: UiDataType::Float,
                        rules: None,
                        when: None,
                        union_discriminator: None,
                        union_case_value: None,
                        enum_items_localized: None,
                    });
                }
            }
            FlattenEntity::Action => {
                // One row per parameter. Include action-level fields first.
                out.push(FlattenColumn {
                    key: "action_name".to_string(),
                    label: Self::loc(locale, "动作名称", "Action Name"),
                    data_type: UiDataType::String,
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                out.push(FlattenColumn {
                    key: "command".to_string(),
                    label: Self::loc(locale, "命令", "Command"),
                    data_type: UiDataType::String,
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                // Parameter-level base fields
                out.push(FlattenColumn {
                    key: "param_name".to_string(),
                    label: Self::loc(locale, "参数名称", "Param Name"),
                    data_type: UiDataType::String,
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                out.push(FlattenColumn {
                    key: "param_key".to_string(),
                    label: Self::loc(locale, "参数键名", "Param Key"),
                    data_type: UiDataType::String,
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                out.push(FlattenColumn {
                    key: "param_data_type".to_string(),
                    label: Self::loc(locale, "数据类型", "Param Data Type"),
                    data_type: UiDataType::Enum {
                        items: Self::enum_items_data_type(locale),
                    },
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                out.push(FlattenColumn {
                    key: "param_required".to_string(),
                    label: Self::loc(locale, "是否必填", "Required"),
                    data_type: UiDataType::Boolean,
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                out.push(FlattenColumn {
                    key: "param_default_value".to_string(),
                    label: Self::loc(locale, "默认值", "Default Value"),
                    data_type: UiDataType::Any,
                    rules: None,
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                out.push(FlattenColumn {
                    key: "param_min_value".to_string(),
                    label: Self::loc(locale, "最小值", "Min Value"),
                    data_type: UiDataType::Float,
                    rules: None,
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
                out.push(FlattenColumn {
                    key: "param_max_value".to_string(),
                    label: Self::loc(locale, "最大值", "Max Value"),
                    data_type: UiDataType::Float,
                    rules: None,
                    when: None,
                    union_discriminator: None,
                    union_case_value: None,
                    enum_items_localized: None,
                });
            }
            FlattenEntity::DevicePoints => {
                // DevicePoints is handled specially in build_template() by calling
                // push_base_columns separately for Device and Point entities.
                // This branch should never be reached.
                unreachable!("DevicePoints should not call push_base_columns directly")
            }
        }
    }

    /// Localize helper for column labels
    #[inline]
    fn loc(locale: &str, zh_cn: &str, en: &str) -> String {
        if locale.eq_ignore_ascii_case("zh-CN") || locale.eq_ignore_ascii_case("zh") {
            zh_cn.to_string()
        } else {
            en.to_string()
        }
    }

    /// Build localized enum items for DataPointType
    fn enum_items_datapoint_type(_locale: &str) -> Vec<EnumItem> {
        vec![
            EnumItem {
                key: serde_json::Value::from(0),
                label: UiText::Localized {
                    locales: BTreeMap::from([
                        ("zh-CN".to_string(), "属性".to_string()),
                        ("en".to_string(), "Attribute".to_string()),
                    ]),
                },
            },
            EnumItem {
                key: serde_json::Value::from(1),
                label: UiText::Localized {
                    locales: BTreeMap::from([
                        ("zh-CN".to_string(), "遥测".to_string()),
                        ("en".to_string(), "Telemetry".to_string()),
                    ]),
                },
            },
        ]
    }

    /// Build localized enum items for AccessMode
    fn enum_items_access_mode(_locale: &str) -> Vec<EnumItem> {
        vec![
            EnumItem {
                key: serde_json::Value::from(0),
                label: UiText::Localized {
                    locales: BTreeMap::from([
                        ("zh-CN".to_string(), "只读".to_string()),
                        ("en".to_string(), "Read".to_string()),
                    ]),
                },
            },
            EnumItem {
                key: serde_json::Value::from(1),
                label: UiText::Localized {
                    locales: BTreeMap::from([
                        ("zh-CN".to_string(), "只写".to_string()),
                        ("en".to_string(), "Write".to_string()),
                    ]),
                },
            },
            EnumItem {
                key: serde_json::Value::from(2),
                label: UiText::Localized {
                    locales: BTreeMap::from([
                        ("zh-CN".to_string(), "读写".to_string()),
                        ("en".to_string(), "Read/Write".to_string()),
                    ]),
                },
            },
        ]
    }

    /// Build localized enum items for DataType
    fn enum_items_data_type(_locale: &str) -> Vec<EnumItem> {
        vec![
            (0, "Boolean", "Boolean"),
            (1, "Int8", "Int8"),
            (2, "UInt8", "UInt8"),
            (3, "Int16", "Int16"),
            (4, "UInt16", "UInt16"),
            (5, "Int32", "Int32"),
            (6, "UInt32", "UInt32"),
            (7, "Int64", "Int64"),
            (8, "UInt64", "UInt64"),
            (9, "Float32", "Float32"),
            (10, "Float64", "Float64"),
            (11, "String", "String"),
            (12, "Binary", "Binary"),
            (13, "Timestamp", "Timestamp"),
        ]
        .into_iter()
        .map(|(k, zh, en)| EnumItem {
            key: serde_json::Value::from(k),
            label: UiText::Localized {
                locales: BTreeMap::from([
                    ("zh-CN".to_string(), zh.to_string()),
                    ("en".to_string(), en.to_string()),
                ]),
            },
        })
        .collect()
    }
}

/// Metadata written into hidden `__meta__` sheet alongside the data sheet.
///
/// This metadata is used to drive locale resolution and basic compatibility checks
/// when importing templates generated by the gateway. Only the listed fields are
/// persisted for minimalism and stability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateMetadata {
    /// Driver type identifier, e.g. "modbus", "s7"
    pub driver_type: String,
    /// Optional driver version string
    pub driver_version: Option<String>,
    /// Optional API version string/number encoded as string
    pub api_version: Option<String>,
    /// Entity kind: "device" | "point" | "action"
    pub entity: String,
    /// Locale for header labels, e.g. "zh-CN" | "en-US"
    pub locale: String,
    /// Schema version for future compatibility, e.g. "1.0"
    pub schema_version: String,
}

impl TemplateMetadata {
    pub fn validate(
        &self,
        expected_driver_type: &str,
        expected_entity: FlattenEntity,
    ) -> Result<(), DriverError> {
        if self.driver_type != expected_driver_type {
            return Err(DriverError::ExecutionError(format!(
                "Driver type mismatch: expected {}, got {}",
                expected_driver_type, self.driver_type
            )));
        }

        let entity_str = expected_entity.to_string().to_ascii_lowercase();
        if self.entity != entity_str {
            return Err(DriverError::ExecutionError(format!(
                "Entity mismatch: expected {}, got {}",
                entity_str, self.entity
            )));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Node {
    Field(Box<Field>),
    Group(Group),
    Union(Union),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub label: UiText,
    pub description: Option<UiText>,
    pub collapsible: bool,
    pub order: Option<i32>,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Union {
    pub order: Option<i32>,
    pub discriminator: String,
    pub mapping: Vec<UnionCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnionCase {
    pub case_value: serde_json::Value,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    pub path: String,
    pub label: UiText,
    pub data_type: UiDataType,
    pub default_value: Option<serde_json::Value>,
    pub order: Option<i32>,
    pub ui: Option<UiProps>,
    pub rules: Option<Rules>,
    pub when: Option<Vec<When>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum UiDataType {
    String,
    Integer,
    Float,
    Boolean,
    Enum { items: Vec<EnumItem> },
    Any,
}

impl UiDataType {
    /// Localize the enum items into (key,label) pairs using the same locale strategy
    #[inline]
    pub fn localize_enum_items(
        items: &[EnumItem],
        locale: &str,
    ) -> Vec<(serde_json::Value, String)> {
        items.iter().map(|i| i.localize(locale)).collect()
    }

    /// Find enum key by its localized label. Falls back to raw key string match.
    #[inline]
    pub fn find_enum_key_by_label(
        items: &[EnumItem],
        locale: &str,
        label: &str,
    ) -> Option<serde_json::Value> {
        items
            .iter()
            .find(|i| i.label.resolve(locale) == label)
            .map(|i| i.key.clone())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumItem {
    pub key: serde_json::Value,
    pub label: UiText,
}

impl EnumItem {
    /// Localize the enum item into (key,label) pairs using the same locale strategy
    #[inline]
    pub fn localize(&self, locale: &str) -> (serde_json::Value, String) {
        (self.key.clone(), self.label.resolve(locale))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UiProps {
    pub placeholder: Option<UiText>,
    pub help: Option<UiText>,
    pub prefix: Option<String>,
    pub suffix: Option<String>,
    pub col_span: Option<u8>,
    pub read_only: Option<bool>,
    pub disabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Rules {
    /// Whether the field is required. Prefer set here over Field.required
    pub required: Option<RuleValue<bool>>,
    /// Numeric bounds for Integer/Float
    pub min: Option<RuleValue<f64>>,
    pub max: Option<RuleValue<f64>>,
    /// String length bounds
    pub min_length: Option<RuleValue<u32>>,
    pub max_length: Option<RuleValue<u32>>,
    /// Regex pattern for strings
    pub pattern: Option<RuleValue<String>>,
}

/// RuleValue allows a raw value or an object with value and message.
/// This maximizes wire compatibility with frontend needs (custom error messages).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RuleValue<T> {
    /// Primitive value, e.g., 10
    Value(T),
    /// Object form with error message, e.g., { value: 10, message: UiText }
    WithMessage { value: T, message: Option<UiText> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct When {
    pub target: String,
    pub operator: Operator,
    pub value: serde_json::Value,
    pub effect: WhenEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operator {
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
    Contains,
    Prefix,
    Suffix,
    Regex,
    In,
    NotIn,
    Between,
    NotBetween,
    NotNull,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WhenEffect {
    /// Control whether the node is rendered (mounted) in the UI (removes DOM when false).
    ///
    /// Best practice:
    /// - Use `If`/`IfNot` for structural gating (e.g. union cases), so hidden fields do not
    ///   participate in validation and do not submit values.
    /// - Use `Visible`/`Invisible` for CSS visibility only (keeps DOM and preserves values).
    If,
    /// The inverse of `If` (a convenience effect for readability).
    IfNot,
    Visible,
    Invisible,
    Enable,
    Disable,
    Require,
    Optional,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UiText {
    Simple { value: String },
    Localized { locales: BTreeMap<String, String> },
}

impl From<String> for UiText {
    fn from(value: String) -> Self {
        UiText::Simple { value }
    }
}

impl From<&str> for UiText {
    fn from(value: &str) -> Self {
        UiText::Simple {
            value: value.to_string(),
        }
    }
}

impl UiText {
    pub fn resolve(&self, locale: &str) -> String {
        match self {
            UiText::Simple { value } => value.clone(),
            UiText::Localized { locales } => locales.get(locale).cloned().unwrap_or_default(),
        }
    }
}

/// Entity kinds for building flattened plans
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlattenEntity {
    Device,
    Point,
    Action,
    DevicePoints,
}

impl Display for FlattenEntity {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        match self {
            FlattenEntity::Device => write!(f, "device"),
            FlattenEntity::Point => write!(f, "point"),
            FlattenEntity::Action => write!(f, "action"),
            FlattenEntity::DevicePoints => write!(f, "device-points"),
        }
    }
}

impl TryFrom<&str> for FlattenEntity {
    type Error = DriverError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value.to_ascii_lowercase().as_str() {
            "device" => Ok(FlattenEntity::Device),
            "point" => Ok(FlattenEntity::Point),
            "action" => Ok(FlattenEntity::Action),
            "device-points" => Ok(FlattenEntity::DevicePoints),
            _ => Err(DriverError::InvalidEntity(value.to_string())),
        }
    }
}

/// Context of a single row used for field validation.
#[derive(Debug)]
pub struct ValidateRowContext<'a> {
    /// 0-based row index in the sheet/template
    pub row_index: usize,
    /// Current locale (e.g., "zh-CN")
    pub locale: &'a str,
    /// Map of fully-qualified keys to values (e.g., "driver_config.host")
    pub values: &'a serde_json::Map<String, serde_json::Value>,
}

/// A single flattened column derived from DriverSchemas
#[derive(Debug, Clone)]
pub struct FlattenColumn {
    /// Fully-qualified machine key, e.g. "driver_config.host" or "inputs.ioa"
    pub key: String,
    /// Localized human-readable header text resolved with locale
    pub label: String,
    /// UI data type as declared by schema
    pub data_type: UiDataType,
    /// Validation rules attached to this field (if any)
    pub rules: Option<Rules>,
    /// When conditions (raw) that affect visibility/requirement
    pub when: Option<Vec<When>>,
    /// If this column belongs to a union case, the discriminator key
    pub union_discriminator: Option<String>,
    /// If this column belongs to a union case, the case value
    pub union_case_value: Option<serde_json::Value>,
    /// Localized enum items for UI/Excel dropdowns (key,label)
    pub enum_items_localized: Option<Vec<(serde_json::Value, String)>>,
}

impl FlattenColumn {
    #[inline]
    fn evaluate_when(
        op: &Operator,
        lhs: Option<&serde_json::Value>,
        rhs: &serde_json::Value,
    ) -> bool {
        match op {
            Operator::NotNull => lhs.is_some() && !matches!(lhs, Some(serde_json::Value::Null)),
            Operator::Eq => Self::compare(lhs, rhs, |o| o == 0),
            Operator::Neq => Self::compare(lhs, rhs, |o| o != 0),
            Operator::Gt => Self::compare(lhs, rhs, |o| o > 0),
            Operator::Gte => Self::compare(lhs, rhs, |o| o >= 0),
            Operator::Lt => Self::compare(lhs, rhs, |o| o < 0),
            Operator::Lte => Self::compare(lhs, rhs, |o| o <= 0),
            Operator::Contains => Self::contains(lhs, rhs),
            Operator::Prefix => Self::prefix(lhs, rhs),
            Operator::Suffix => Self::suffix(lhs, rhs),
            Operator::Regex => Self::regex(lhs, rhs),
            Operator::In => Self::in_list(lhs, rhs, true),
            Operator::NotIn => Self::in_list(lhs, rhs, false),
            Operator::Between => Self::between(lhs, rhs, true),
            Operator::NotBetween => Self::between(lhs, rhs, false),
        }
    }

    #[inline]
    fn loosely_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
        match (a, b) {
            (serde_json::Value::String(x), serde_json::Value::String(y)) => x == y,
            (serde_json::Value::Bool(x), serde_json::Value::Bool(y)) => x == y,
            (serde_json::Value::Number(x), serde_json::Value::Number(y)) => {
                x.to_string() == y.to_string()
            }
            _ => a == b,
        }
    }

    #[inline]
    fn compare<F>(lhs: Option<&serde_json::Value>, rhs: &serde_json::Value, f: F) -> bool
    where
        F: FnOnce(i8) -> bool,
    {
        if let Some(l) = lhs {
            if let (Some(a), Some(b)) = (Self::to_f64(l), Self::to_f64(rhs)) {
                return f(a
                    .partial_cmp(&b)
                    .map(|o| match o {
                        std::cmp::Ordering::Less => -1,
                        std::cmp::Ordering::Equal => 0,
                        std::cmp::Ordering::Greater => 1,
                    })
                    .unwrap_or(1));
            }
            if let (Some(a), Some(b)) = (Self::to_string(l), Self::to_string(rhs)) {
                return f(a.cmp(&b) as i8);
            }
            if let (Some(a), Some(b)) = (Self::to_bool(l), Self::to_bool(rhs)) {
                return f((a as i8) - (b as i8));
            }
        }
        false
    }

    #[inline]
    fn to_f64(v: &serde_json::Value) -> Option<f64> {
        match v {
            serde_json::Value::Number(n) => n.as_f64(),
            serde_json::Value::String(s) => s.parse::<f64>().ok(),
            serde_json::Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    #[inline]
    fn to_string(v: &serde_json::Value) -> Option<String> {
        match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    #[inline]
    fn to_bool(v: &serde_json::Value) -> Option<bool> {
        match v {
            serde_json::Value::Bool(b) => Some(*b),
            serde_json::Value::String(s) => match s.as_str() {
                "true" | "1" | "yes" | "on" => Some(true),
                "false" | "0" | "no" | "off" => Some(false),
                _ => None,
            },
            serde_json::Value::Number(n) => Some(n.as_f64().unwrap_or(0.0) != 0.0),
            _ => None,
        }
    }

    #[inline]
    fn contains(lhs: Option<&serde_json::Value>, rhs: &serde_json::Value) -> bool {
        match (lhs, rhs) {
            (Some(serde_json::Value::String(a)), serde_json::Value::String(b)) => a.contains(b),
            (Some(serde_json::Value::Array(arr)), _) => {
                arr.iter().any(|v| Self::loosely_equal(v, rhs))
            }
            _ => false,
        }
    }

    #[inline]
    fn prefix(lhs: Option<&serde_json::Value>, rhs: &serde_json::Value) -> bool {
        match (lhs, rhs) {
            (Some(serde_json::Value::String(a)), serde_json::Value::String(b)) => a.starts_with(b),
            _ => false,
        }
    }

    #[inline]
    fn suffix(lhs: Option<&serde_json::Value>, rhs: &serde_json::Value) -> bool {
        match (lhs, rhs) {
            (Some(serde_json::Value::String(a)), serde_json::Value::String(b)) => a.ends_with(b),
            _ => false,
        }
    }

    #[inline]
    fn regex(lhs: Option<&serde_json::Value>, rhs: &serde_json::Value) -> bool {
        // Only support string target and string pattern for performance and clarity
        let (target, pattern_spec) = match (lhs, rhs) {
            (Some(serde_json::Value::String(s)), serde_json::Value::String(p)) => (s, p),
            _ => return false,
        };

        // Support two styles:
        // 1) Raw pattern or inline flags:    "^abc$", "(?i)^abc$"
        // 2) JS-like delimiter with flags:  "/^abc$/i", "/foo.bar/msx"
        #[inline]
        fn build_regex(spec: &str) -> Option<regex::Regex> {
            if spec.starts_with('/') {
                // Find last '/' as delimiter end; allow pattern to contain '/'
                if let Some(end) = spec.rfind('/') {
                    if end > 0 {
                        let pat = &spec[1..end];
                        let flags = &spec[end + 1..];
                        let mut mods = String::new();
                        for ch in flags.chars() {
                            match ch {
                                // Supported flags mapped to Rust regex inline flags
                                'i' | 'm' | 's' | 'x' => mods.push(ch),
                                // Silently ignore unsupported flags to stay lenient
                                _ => {}
                            }
                        }
                        let final_pat = if mods.is_empty() {
                            pat.to_string()
                        } else {
                            format!("(?{}){}", mods, pat)
                        };
                        return regex::Regex::new(&final_pat).ok();
                    }
                }
            }
            // Fallback: accept inline-flag patterns or plain patterns
            regex::Regex::new(spec).ok()
        }

        if let Some(re) = build_regex(pattern_spec) {
            re.is_match(target)
        } else {
            false
        }
    }

    #[inline]
    fn in_list(lhs: Option<&serde_json::Value>, rhs: &serde_json::Value, positive: bool) -> bool {
        let mut found = false;
        match (lhs, rhs) {
            (Some(lv), serde_json::Value::Array(arr)) => {
                for v in arr {
                    if Self::loosely_equal(lv, v) {
                        found = true;
                        break;
                    }
                }
            }
            (Some(lv), serde_json::Value::String(s)) => {
                // comma separated
                for part in s.split(',') {
                    if Self::to_string(lv).as_deref() == Some(part.trim()) {
                        found = true;
                        break;
                    }
                }
            }
            _ => {}
        }
        if positive {
            found
        } else {
            !found
        }
    }

    #[inline]
    fn between(lhs: Option<&serde_json::Value>, rhs: &serde_json::Value, positive: bool) -> bool {
        if let (Some(a), serde_json::Value::Array(arr)) = (lhs, rhs) {
            if arr.len() == 2 {
                if let (Some(v), Some(min), Some(max)) = (
                    Self::to_f64(a),
                    Self::to_f64(&arr[0]),
                    Self::to_f64(&arr[1]),
                ) {
                    let ok = v >= min && v <= max;
                    return if positive { ok } else { !ok };
                }
            }
        }
        false
    }

    /// Validate and normalize a single cell for this column with full row context.
    ///
    /// Behavior per requirements:
    /// - Locale uses '.' as decimal separator strictly
    /// - Strings are trimmed; empty string => None if not required, error if required
    /// - Enum matching is case-insensitive on both labels and string keys
    /// - Arrays are not supported
    /// - `when` effects are ignored except `Require`/`Optional`, which override required
    #[inline]
    pub fn validate(
        &self,
        ctx: &ValidateRowContext<'_>,
    ) -> Result<Option<serde_json::Value>, FieldError> {
        // Required after applying `when` overrides
        let required = self.resolve_required(ctx);

        // Union case gating
        if !self.union_applicable(ctx) {
            return Ok(None);
        }

        // Read and trim raw value (Option)
        let raw_opt = self.read_and_trim_value(ctx, required)?;
        let raw = match raw_opt {
            Some(v) => v,
            None => return Ok(None),
        };

        // Normalize to target type
        let normalized = self.normalize_value_for_type(&raw, ctx)?;

        // Apply rule-based validations
        self.validate_rules_on(&normalized, ctx)?;

        Ok(Some(normalized))
    }

    #[inline]
    fn resolve_required(&self, ctx: &ValidateRowContext<'_>) -> bool {
        let mut required = match &self.rules.as_ref().and_then(|r| r.required.as_ref()) {
            Some(RuleValue::Value(v)) => *v,
            Some(RuleValue::WithMessage { value, .. }) => *value,
            None => false,
        };
        if let Some(list) = &self.when {
            for w in list.iter() {
                if Self::evaluate_when(
                    &w.operator,
                    DriverEntityTemplate::get_value_by_path(ctx.values, &w.target),
                    &w.value,
                ) {
                    match w.effect {
                        WhenEffect::Require => required = true,
                        WhenEffect::Optional => required = false,
                        _ => {}
                    }
                }
            }
        }
        required
    }

    #[inline]
    fn union_applicable(&self, ctx: &ValidateRowContext<'_>) -> bool {
        if let (Some(discr), Some(case_val)) = (&self.union_discriminator, &self.union_case_value) {
            matches!(
                DriverEntityTemplate::get_value_by_path(ctx.values, discr),
                Some(v) if Self::loosely_equal(v, case_val)
            )
        } else {
            true
        }
    }

    #[inline]
    fn read_and_trim_value(
        &self,
        ctx: &ValidateRowContext<'_>,
        required: bool,
    ) -> Result<Option<serde_json::Value>, FieldError> {
        use serde_json::Value as Json;
        match DriverEntityTemplate::get_value_by_path(ctx.values, &self.key) {
            Some(v) => {
                if let Json::String(s) = v {
                    let trimmed = s.trim();
                    if trimmed.is_empty() {
                        if required {
                            return Err(FieldError {
                                row: ctx.row_index + 2,
                                field: self.key.clone(),
                                code: ValidationCode::Required,
                                message: format!("{} is required", self.label),
                            });
                        } else {
                            return Ok(None);
                        }
                    }
                    // For Any, we keep trimming but not parsing here; parsing happens in normalize
                    Ok(Some(Json::String(trimmed.to_string())))
                } else {
                    Ok(Some(v.clone()))
                }
            }
            None => {
                if required {
                    Err(FieldError {
                        row: ctx.row_index + 2,
                        field: self.key.clone(),
                        code: ValidationCode::Required,
                        message: format!("{} is required", self.label),
                    })
                } else {
                    Ok(None)
                }
            }
        }
    }

    #[inline]
    fn normalize_value_for_type(
        &self,
        value: &serde_json::Value,
        ctx: &ValidateRowContext<'_>,
    ) -> Result<serde_json::Value, FieldError> {
        use serde_json::Value as Json;
        let normalized = match &self.data_type {
            UiDataType::String => match value {
                Json::String(s) => Json::String(s.clone()),
                _ => match Self::to_string(value) {
                    Some(s) => Json::String(s),
                    None => {
                        return Err(FieldError {
                            row: ctx.row_index + 2,
                            field: self.key.clone(),
                            code: ValidationCode::TypeMismatch,
                            message: format!("{} type mismatch (string)", self.label),
                        })
                    }
                },
            },
            UiDataType::Integer => match value {
                Json::Number(n) if n.as_i64().is_some() => value.clone(),
                Json::String(s) => match s.parse::<i64>() {
                    Ok(n) => Json::from(n),
                    Err(_) => {
                        return Err(FieldError {
                            row: ctx.row_index + 2,
                            field: self.key.clone(),
                            code: ValidationCode::TypeMismatch,
                            message: format!("{} type mismatch (integer)", self.label),
                        })
                    }
                },
                _ => {
                    return Err(FieldError {
                        row: ctx.row_index + 2,
                        field: self.key.clone(),
                        code: ValidationCode::TypeMismatch,
                        message: format!("{} type mismatch (integer)", self.label),
                    })
                }
            },
            UiDataType::Float => match value {
                Json::Number(n) if n.as_f64().is_some() => value.clone(),
                Json::String(s) => match s.parse::<f64>() {
                    Ok(n) => Json::from(n),
                    Err(_) => {
                        return Err(FieldError {
                            row: ctx.row_index + 2,
                            field: self.key.clone(),
                            code: ValidationCode::TypeMismatch,
                            message: format!("{} type mismatch (float)", self.label),
                        })
                    }
                },
                _ => {
                    return Err(FieldError {
                        row: ctx.row_index + 2,
                        field: self.key.clone(),
                        code: ValidationCode::TypeMismatch,
                        message: format!("{} type mismatch (float)", self.label),
                    })
                }
            },
            UiDataType::Boolean => match value {
                Json::Bool(_) => value.clone(),
                Json::String(s) => match s.to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" | "y" | "t" | "是" => Json::Bool(true),
                    "false" | "0" | "no" | "off" | "n" | "f" | "否" => Json::Bool(false),
                    _ => {
                        return Err(FieldError {
                            row: ctx.row_index + 2,
                            field: self.key.clone(),
                            code: ValidationCode::TypeMismatch,
                            message: format!("{} type mismatch (boolean)", self.label),
                        })
                    }
                },
                _ => {
                    return Err(FieldError {
                        row: ctx.row_index + 2,
                        field: self.key.clone(),
                        code: ValidationCode::TypeMismatch,
                        message: format!("{} type mismatch (boolean)", self.label),
                    })
                }
            },
            UiDataType::Enum { items } => {
                if items.iter().any(|it| it.key == *value) {
                    value.clone()
                } else {
                    match value {
                        Json::String(s) => {
                            let s_lower = s.to_ascii_lowercase();
                            if let Some((k, _)) =
                                self.enum_items_localized.as_ref().and_then(|pairs| {
                                    pairs
                                        .iter()
                                        .find(|(_, l)| l.to_ascii_lowercase() == s_lower)
                                })
                            {
                                k.clone()
                            } else {
                                let hit = items.iter().find(|it| match &it.key {
                                    Json::String(ks) => ks.eq_ignore_ascii_case(s),
                                    Json::Number(n) => n.to_string().eq_ignore_ascii_case(s),
                                    Json::Bool(b) => b.to_string().eq_ignore_ascii_case(s),
                                    _ => false,
                                });
                                if let Some(h) = hit {
                                    h.key.clone()
                                } else {
                                    return Err(FieldError {
                                        row: ctx.row_index + 2,
                                        field: self.key.clone(),
                                        code: ValidationCode::TypeMismatch,
                                        message: format!("{} not in enum", self.label),
                                    });
                                }
                            }
                        }
                        _ => {
                            return Err(FieldError {
                                row: ctx.row_index + 2,
                                field: self.key.clone(),
                                code: ValidationCode::TypeMismatch,
                                message: format!("{} not in enum", self.label),
                            })
                        }
                    }
                }
            }
            UiDataType::Any => value.clone(),
        };
        Ok(normalized)
    }

    #[inline]
    fn validate_rules_on(
        &self,
        normalized: &serde_json::Value,
        ctx: &ValidateRowContext<'_>,
    ) -> Result<(), FieldError> {
        if let Some(rules) = &self.rules {
            if matches!(self.data_type, UiDataType::Integer | UiDataType::Float) {
                if let Some(v) = FlattenColumn::to_f64(normalized) {
                    if let Some(rule) = &rules.min {
                        let minv = match rule {
                            RuleValue::Value(x) => *x,
                            RuleValue::WithMessage { value, .. } => *value,
                        };
                        if v < minv {
                            return Err(FieldError {
                                row: ctx.row_index + 2,
                                field: self.key.clone(),
                                code: ValidationCode::Range,
                                message: format!("{} < min {}", self.label, minv),
                            });
                        }
                    }
                    if let Some(rule) = &rules.max {
                        let maxv = match rule {
                            RuleValue::Value(x) => *x,
                            RuleValue::WithMessage { value, .. } => *value,
                        };
                        if v > maxv {
                            return Err(FieldError {
                                row: ctx.row_index + 2,
                                field: self.key.clone(),
                                code: ValidationCode::Range,
                                message: format!("{} > max {}", self.label, maxv),
                            });
                        }
                    }
                }
            }

            if matches!(self.data_type, UiDataType::String | UiDataType::Enum { .. }) {
                if let Some(s) = FlattenColumn::to_string(normalized) {
                    if let Some(rule) = &rules.min_length {
                        let minl = match rule {
                            RuleValue::Value(x) => *x as usize,
                            RuleValue::WithMessage { value, .. } => *value as usize,
                        };
                        if s.chars().count() < minl {
                            return Err(FieldError {
                                row: ctx.row_index + 2,
                                field: self.key.clone(),
                                code: ValidationCode::Length,
                                message: format!("{} length < {}", self.label, minl),
                            });
                        }
                    }
                    if let Some(rule) = &rules.max_length {
                        let maxl = match rule {
                            RuleValue::Value(x) => *x as usize,
                            RuleValue::WithMessage { value, .. } => *value as usize,
                        };
                        if s.chars().count() > maxl {
                            return Err(FieldError {
                                row: ctx.row_index + 2,
                                field: self.key.clone(),
                                code: ValidationCode::Length,
                                message: format!("{} length > {}", self.label, maxl),
                            });
                        }
                    }
                }
            }

            if let Some(rule) = &rules.pattern {
                if let Some(s) = FlattenColumn::to_string(normalized) {
                    let pat = match rule {
                        RuleValue::Value(p) => p,
                        RuleValue::WithMessage { value, .. } => value,
                    };
                    if let Ok(re) = regex::Regex::new(pat) {
                        if !re.is_match(&s) {
                            return Err(FieldError {
                                row: ctx.row_index + 2,
                                field: self.key.clone(),
                                code: ValidationCode::Pattern,
                                message: format!("{} does not match pattern", self.label),
                            });
                        }
                    }
                }
            }

            // For Any: apply numeric or string rules based on the runtime value type
            if matches!(self.data_type, UiDataType::Any) {
                if let Some(v) = FlattenColumn::to_f64(normalized) {
                    if let Some(rule) = &rules.min {
                        let minv = match rule {
                            RuleValue::Value(x) => *x,
                            RuleValue::WithMessage { value, .. } => *value,
                        };
                        if v < minv {
                            return Err(FieldError {
                                row: ctx.row_index + 2,
                                field: self.key.clone(),
                                code: ValidationCode::Range,
                                message: format!("{} < min {}", self.label, minv),
                            });
                        }
                    }
                    if let Some(rule) = &rules.max {
                        let maxv = match rule {
                            RuleValue::Value(x) => *x,
                            RuleValue::WithMessage { value, .. } => *value,
                        };
                        if v > maxv {
                            return Err(FieldError {
                                row: ctx.row_index + 2,
                                field: self.key.clone(),
                                code: ValidationCode::Range,
                                message: format!("{} > max {}", self.label, maxv),
                            });
                        }
                    }
                }

                if let Some(s) = FlattenColumn::to_string(normalized) {
                    if let Some(rule) = &rules.min_length {
                        let minl = match rule {
                            RuleValue::Value(x) => *x as usize,
                            RuleValue::WithMessage { value, .. } => *value as usize,
                        };
                        if s.chars().count() < minl {
                            return Err(FieldError {
                                row: ctx.row_index + 2,
                                field: self.key.clone(),
                                code: ValidationCode::Length,
                                message: format!("{} length < {}", self.label, minl),
                            });
                        }
                    }
                    if let Some(rule) = &rules.max_length {
                        let maxl = match rule {
                            RuleValue::Value(x) => *x as usize,
                            RuleValue::WithMessage { value, .. } => *value as usize,
                        };
                        if s.chars().count() > maxl {
                            return Err(FieldError {
                                row: ctx.row_index + 2,
                                field: self.key.clone(),
                                code: ValidationCode::Length,
                                message: format!("{} length > {}", self.label, maxl),
                            });
                        }
                    }

                    if let Some(rule) = &rules.pattern {
                        let pat = match rule {
                            RuleValue::Value(p) => p,
                            RuleValue::WithMessage { value, .. } => value,
                        };
                        if let Ok(re) = regex::Regex::new(pat) {
                            if !re.is_match(&s) {
                                return Err(FieldError {
                                    row: ctx.row_index + 2,
                                    field: self.key.clone(),
                                    code: ValidationCode::Pattern,
                                    message: format!("{} does not match pattern", self.label),
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// A flattened view for a specific entity, ready for template generation or import validation
#[derive(Debug, Clone)]
pub struct DriverEntityTemplate {
    /// All columns (including union discriminator and case fields)
    pub columns: Vec<FlattenColumn>,
    /// All discriminator keys that appear in the plan (e.g. ["driver_config.kind"])
    pub discriminator_keys: Vec<String>,
}

impl DriverEntityTemplate {
    /// Read only TemplateMetadata from `__meta__` sheet. Useful to build a correct
    /// template with the appropriate locale before parsing the data sheet.
    pub fn read_template_metadata<R>(reader: R) -> Result<TemplateMetadata, DriverError>
    where
        R: IoRead + IoSeek,
    {
        let mut workbook = calamine::Xlsx::new(reader)
            .map_err(|e| DriverError::ExecutionError(format!("xlsx open: {e}")))?;
        let meta_range = workbook
            .worksheet_range("__meta__")
            .map_err(|e| DriverError::ExecutionError(format!("xlsx read: {e}")))?;

        let mut meta_map = BTreeMap::new();
        for (ri, row) in meta_range.rows().enumerate() {
            if ri == 0 {
                continue;
            }
            if row.len() >= 2 {
                let key = row[0].to_string();
                let val = row[1].to_string();
                if !key.trim().is_empty() {
                    meta_map.insert(key, val);
                }
            }
        }

        Ok(TemplateMetadata {
            driver_type: meta_map.get("driver_type").cloned().unwrap_or_default(),
            driver_version: meta_map
                .get("driver_version")
                .cloned()
                .filter(|s| !s.is_empty()),
            api_version: meta_map
                .get("api_version")
                .cloned()
                .filter(|s| !s.is_empty()),
            entity: meta_map.get("entity").cloned().unwrap_or_default(),
            locale: meta_map
                .get("locale")
                .cloned()
                .unwrap_or_else(|| "zh-CN".to_string()),
            schema_version: meta_map
                .get("schema_version")
                .cloned()
                .unwrap_or_else(|| "1.0".to_string()),
        })
    }

    /// Validate and normalize rows; currently implements required checks and basic visibility logic.
    /// Further rules (range/length/pattern/union) can extend this method without breaking API.
    pub fn validate_and_normalize_rows(
        &self,
        rows: Vec<serde_json::Map<String, Json>>,
        locale: &str,
    ) -> (Vec<ValidatedRow>, Vec<FieldError>, usize) {
        let mut valids = Vec::with_capacity(rows.len());
        let mut errors = Vec::new();
        let warn_count = 0usize;

        for (idx, row) in rows.into_iter().enumerate() {
            // Initialize normalized row first
            let mut normalized_row = row;
            let mut row_has_error = false;

            // Discriminator presence (coarse check)
            for discr in &self.discriminator_keys {
                if normalized_row.get(discr).is_none() {
                    errors.push(FieldError {
                        row: idx + 2,
                        field: discr.clone(),
                        code: ValidationCode::DiscriminatorMissing,
                        message: format!("missing discriminator: {discr}"),
                    });
                    row_has_error = true;
                }
            }

            for col in &self.columns {
                let present_value =
                    DriverEntityTemplate::get_value_by_path(&normalized_row, &col.key).cloned();
                let ctx_row = ValidateRowContext {
                    row_index: idx,
                    locale,
                    values: &normalized_row,
                };

                match col.validate(&ctx_row) {
                    Ok(Some(new_value)) => {
                        if Some(new_value.clone()) != present_value {
                            Self::insert_nested(&mut normalized_row, &col.key, new_value);
                        }
                    }
                    Ok(None) => {
                        if present_value.is_some() {
                            let _ = Self::remove_by_path(&mut normalized_row, &col.key);
                        }
                    }
                    Err(err) => {
                        errors.push(err);
                        row_has_error = true;
                    }
                }
            }

            if !row_has_error {
                // Note: at this stage `row` already had prefixes folded in read phase
                valids.push(ValidatedRow {
                    row_index: idx + 2,
                    values: normalized_row,
                });
            }
        }

        (valids, errors, warn_count)
    }

    /// Find a column by its machine key
    pub fn get_column(&self, key: &str) -> Option<&FlattenColumn> {
        self.columns.iter().find(|c| c.key == key)
    }

    /// Read rows from a range
    #[inline]
    fn read_rows_from_range(
        &self,
        locale: &str,
        range: calamine::Range<calamine::Data>,
    ) -> Result<Vec<serde_json::Map<String, Json>>, DriverError> {
        let mut rows_iter = range.rows();
        // read header row
        let header = match rows_iter.next() {
            Some(r) => r,
            None => return Ok(Vec::new()),
        };

        // Build column index mapping: header label -> plan index
        let mut header_to_idx = Vec::with_capacity(header.len());
        for cell in header {
            let h = cell.to_string();
            let idx = self.columns.iter().position(|c| c.label == h);
            header_to_idx.push(idx);
        }

        let mut out = Vec::new();
        for row in rows_iter {
            let mut obj = serde_json::Map::new();
            let mut has_any = false;
            for (ci, cell) in row.iter().enumerate() {
                if let Some(Some(pi)) = header_to_idx.get(ci) {
                    let col = &self.columns[*pi];
                    if let Some(v) = Self::map_cell_to_json(cell, col, locale) {
                        obj.insert(col.key.clone(), v);
                        has_any = true;
                    }
                }
            }
            if has_any {
                // Fold special prefixes into nested objects (driver_config.*, inputs.*)
                Self::fold_special_prefixes(&mut obj);
                out.push(obj);
            }
        }
        Ok(out)
    }

    /// Map a single Excel cell (calamine::Data) to a serde_json::Value according to UiDataType.
    ///
    /// Behavior:
    /// - Empty -> None (skip)
    /// - String: trim and skip if empty
    /// - Enum: accept localized label (String); or raw key when types match (String/Number/Bool)
    /// - Boolean: prefer Data::Bool; numeric non-zero => true; string aliases supported
    /// - Integer/Float: prefer numeric cells; fallback parse from string; bool -> 1/0
    /// - Any: preserve native cell types; for String use parse_any_str for rich parsing
    #[inline]
    fn map_cell_to_json(cell: &calamine::Data, col: &FlattenColumn, locale: &str) -> Option<Json> {
        use calamine::Data as CData;
        use serde_json::Value as Json;

        // Normalize Empty and whitespace-only strings to None
        match cell {
            CData::Empty => return None,
            CData::String(s) if s.trim().is_empty() => return None,
            _ => {}
        }

        match &col.data_type {
            UiDataType::String => Some(match cell {
                CData::String(s) => Json::String(s.clone()),
                CData::Int(n) => Json::String(n.to_string()),
                CData::Float(f) => Json::String(f.to_string()),
                CData::Bool(b) => Json::String(b.to_string()),
                CData::DateTimeIso(s) => Json::String(s.clone()),
                CData::DateTime(dt) => Json::String(dt.to_string()),
                CData::DurationIso(s) => Json::String(s.clone()),
                CData::Error(_) => Json::String(cell.to_string()),
                CData::Empty => return None,
            }),

            UiDataType::Integer => match cell {
                CData::Int(n) => Some(Json::from(*n)),
                CData::Float(f) => Some(Json::from(*f as i64)),
                CData::Bool(b) => Some(Json::from(if *b { 1i64 } else { 0i64 })),
                CData::String(s) => {
                    let st = s.trim();
                    if let Ok(n) = st.parse::<i64>() {
                        Some(Json::from(n))
                    } else {
                        None
                    }
                }
                _ => None,
            },

            UiDataType::Float => match cell {
                CData::Float(f) => Some(Json::from(*f)),
                CData::Int(n) => Some(Json::from(*n as f64)),
                CData::Bool(b) => Some(Json::from(if *b { 1.0 } else { 0.0 })),
                CData::String(s) => {
                    let st = s.trim();
                    if let Ok(n) = st.parse::<f64>() {
                        Some(Json::from(n))
                    } else {
                        None
                    }
                }
                _ => None,
            },

            UiDataType::Boolean => match cell {
                CData::Bool(b) => Some(Json::Bool(*b)),
                CData::Int(n) => Some(Json::Bool(*n != 0)),
                CData::Float(f) => Some(Json::Bool(*f != 0.0)),
                CData::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" | "y" | "t" | "是" => Some(Json::Bool(true)),
                    "false" | "0" | "no" | "off" | "n" | "f" | "否" => Some(Json::Bool(false)),
                    _ => None,
                },
                _ => None,
            },

            UiDataType::Enum { items } => {
                // 1) label match for String
                if let CData::String(s) = cell {
                    if let Some(k) = UiDataType::find_enum_key_by_label(items.as_slice(), locale, s)
                    {
                        return Some(k);
                    }
                }
                // 2) direct key match for non-string cells or string-as-key
                let candidate = match cell {
                    CData::Int(n) => Json::from(*n),
                    CData::Float(f) => Json::from(*f),
                    CData::Bool(b) => Json::from(*b),
                    CData::String(s) => Json::String(s.clone()),
                    _ => Json::String(cell.to_string()),
                };
                if items.iter().any(|it| it.key == candidate) {
                    Some(candidate)
                } else {
                    // Keep original string (if any) for later normalize to decide
                    match cell {
                        CData::String(s) => Some(Json::String(s.clone())),
                        _ => None,
                    }
                }
            }

            UiDataType::Any => Some(match cell {
                CData::Int(n) => Json::from(*n),
                CData::Float(f) => Json::from(*f),
                CData::Bool(b) => Json::from(*b),
                CData::String(s) => {
                    if let Some(v) = Self::parse_any_str(s) {
                        v
                    } else {
                        Json::String(s.clone())
                    }
                }
                CData::DateTimeIso(s) => Json::String(s.clone()),
                CData::DateTime(dt) => Json::String(dt.to_string()),
                CData::DurationIso(s) => Json::String(s.clone()),
                CData::Error(_) => Json::String(cell.to_string()),
                CData::Empty => return None,
            }),
        }
    }

    /// Build a workbook for the template
    #[inline]
    fn build_template_workbook(&self, locale: &str) -> Result<Workbook, DriverError> {
        let mut workbook = Workbook::new();

        // Pre-compute enum list labels and formulas without creating worksheets yet.
        let mut enum_list_formulas = vec![None; self.columns.len()];
        for (i, col) in self.columns.iter().enumerate() {
            if let UiDataType::Enum { items } = &col.data_type {
                let localized = UiDataType::localize_enum_items(items, locale);
                let list = localized.into_iter().map(|(_, l)| l).collect::<Vec<_>>();
                if !list.is_empty() {
                    let col_letter = Self::excel_col_letter(i);
                    let start_row = 1; // A1-based
                    let end_row = list.len();
                    let formula = format!(
                        "'__lists__'!${}${}:${}${}",
                        col_letter, start_row, col_letter, end_row
                    );
                    enum_list_formulas[i] = Some(formula);
                }
            }
        }

        // Create the main worksheet first to ensure there is a visible sheet.
        {
            let worksheet = workbook.add_worksheet();
            // Header format: bold, larger font, with a distinctive background fill
            let header_format = Format::new()
                .set_bold()
                .set_font_size(12.0)
                .set_background_color(Color::RGB(0xE6F7FF))
                .set_pattern(FormatPattern::Solid);
            for (i, col) in self.columns.iter().enumerate() {
                worksheet
                    .write_string_with_format(0, i as u16, &col.label, &header_format)
                    .map_err(|e| DriverError::ExecutionError(format!("xlsx write header: {e}")))?;
            }

            for (i, col) in self.columns.iter().enumerate() {
                match &col.data_type {
                    UiDataType::Enum { items: _ } => {
                        if let Some(formula_range) = &enum_list_formulas[i] {
                            let dv = DataValidation::new()
                                .allow_list_formula(Formula::new(formula_range))
                                .set_error_title("Invalid value".to_string())
                                .map_err(|e| {
                                    DriverError::ExecutionError(format!("xlsx validation: {e}"))
                                })?
                                .set_error_message("Please select a valid value".to_string())
                                .map_err(|e| {
                                    DriverError::ExecutionError(format!("xlsx validation: {e}"))
                                })?
                                .set_error_style(DataValidationErrorStyle::Warning);
                            worksheet
                                .add_data_validation(1, i as u16, 50000, i as u16, &dv)
                                .map_err(|e| {
                                    DriverError::ExecutionError(format!("xlsx validation: {e}"))
                                })?;
                        }
                    }
                    UiDataType::Integer => {
                        let (min_opt, max_opt) = col
                            .rules
                            .as_ref()
                            .map(|r| {
                                let minf = r.min.as_ref().map(|rv| match rv {
                                    RuleValue::Value(v) => *v,
                                    RuleValue::WithMessage { value, .. } => *value,
                                });
                                let maxf = r.max.as_ref().map(|rv| match rv {
                                    RuleValue::Value(v) => *v,
                                    RuleValue::WithMessage { value, .. } => *value,
                                });
                                (minf, maxf)
                            })
                            .unwrap_or((None, None));

                        let to_i32 = |v: f64| -> i32 {
                            if v.is_finite() {
                                let rounded = v.round();
                                if rounded > i32::MAX as f64 {
                                    i32::MAX
                                } else if rounded < i32::MIN as f64 {
                                    i32::MIN
                                } else {
                                    rounded as i32
                                }
                            } else if v.is_sign_positive() {
                                i32::MAX
                            } else {
                                i32::MIN
                            }
                        };

                        let rule = match (min_opt, max_opt) {
                            (Some(minf), Some(maxf)) => {
                                DataValidationRule::Between(to_i32(minf), to_i32(maxf))
                            }
                            (Some(minf), None) => {
                                DataValidationRule::GreaterThanOrEqualTo(to_i32(minf))
                            }
                            (None, Some(maxf)) => {
                                DataValidationRule::LessThanOrEqualTo(to_i32(maxf))
                            }
                            _ => DataValidationRule::Between(i32::MIN, i32::MAX),
                        };

                        let dv = DataValidation::new()
                            .allow_whole_number(rule)
                            .set_error_title("Invalid integer".to_string())
                            .map_err(|e| {
                                DriverError::ExecutionError(format!("xlsx validation: {e}"))
                            })?
                            .set_error_message("Please enter a valid integer".to_string())
                            .map_err(|e| {
                                DriverError::ExecutionError(format!("xlsx validation: {e}"))
                            })?
                            .set_error_style(DataValidationErrorStyle::Warning);
                        worksheet
                            .add_data_validation(1, i as u16, 50000, i as u16, &dv)
                            .map_err(|e| {
                                DriverError::ExecutionError(format!("xlsx validation: {e}"))
                            })?;
                    }
                    UiDataType::Float => {
                        // 从规则中提取 min/max（若存在），直接使用 f64 规则
                        let (min_opt, max_opt) = col
                            .rules
                            .as_ref()
                            .map(|r| {
                                let minv = r.min.as_ref().map(|rv| match rv {
                                    RuleValue::Value(v) => *v,
                                    RuleValue::WithMessage { value, .. } => *value,
                                });
                                let maxv = r.max.as_ref().map(|rv| match rv {
                                    RuleValue::Value(v) => *v,
                                    RuleValue::WithMessage { value, .. } => *value,
                                });
                                (minv, maxv)
                            })
                            .unwrap_or((None, None));

                        let rule = match (min_opt, max_opt) {
                            (Some(minv), Some(maxv)) if minv.is_finite() && maxv.is_finite() => {
                                DataValidationRule::Between(minv, maxv)
                            }
                            (Some(minv), None) if minv.is_finite() => {
                                DataValidationRule::GreaterThanOrEqualTo(minv)
                            }
                            (None, Some(maxv)) if maxv.is_finite() => {
                                DataValidationRule::LessThanOrEqualTo(maxv)
                            }
                            _ => DataValidationRule::Between(f64::MIN, f64::MAX),
                        };

                        let dv = DataValidation::new()
                            .allow_decimal_number(rule)
                            .set_error_title("Invalid number".to_string())
                            .map_err(|e| {
                                DriverError::ExecutionError(format!("xlsx validation: {e}"))
                            })?
                            .set_error_message("Please enter a valid number".to_string())
                            .map_err(|e| {
                                DriverError::ExecutionError(format!("xlsx validation: {e}"))
                            })?
                            .set_error_style(DataValidationErrorStyle::Warning);
                        worksheet
                            .add_data_validation(1, i as u16, 50000, i as u16, &dv)
                            .map_err(|e| {
                                DriverError::ExecutionError(format!("xlsx validation: {e}"))
                            })?;
                    }
                    UiDataType::Boolean => {
                        let dv = DataValidation::new()
                            .allow_list_strings(&["true", "false"])
                            .map_err(|e| {
                                DriverError::ExecutionError(format!("xlsx validation: {e}"))
                            })?
                            .set_error_title("Invalid boolean".to_string())
                            .map_err(|e| {
                                DriverError::ExecutionError(format!("xlsx validation: {e}"))
                            })?
                            .set_error_message("Please enter a valid boolean".to_string())
                            .map_err(|e| {
                                DriverError::ExecutionError(format!("xlsx validation: {e}"))
                            })?
                            .set_error_style(DataValidationErrorStyle::Warning);
                        worksheet
                            .add_data_validation(1, i as u16, 50000, i as u16, &dv)
                            .map_err(|e| {
                                DriverError::ExecutionError(format!("xlsx validation: {e}"))
                            })?;
                    }
                    _ => {}
                }
            }
        }

        // After main sheet exists, create and hide the __lists__ sheet and write labels.
        {
            let lists_ws = workbook.add_worksheet();
            let _ = lists_ws.set_hidden(true).set_name("__lists__");
            // Compute and write labels on the fly to avoid storing them upfront.
            for (i, col) in self.columns.iter().enumerate() {
                if let UiDataType::Enum { items } = &col.data_type {
                    let localized = UiDataType::localize_enum_items(items, locale);
                    if localized.is_empty() {
                        continue;
                    }
                    for (ri, (_, label)) in localized.into_iter().enumerate() {
                        lists_ws
                            .write_string(ri as u32, i as u16, &label)
                            .map_err(|e| {
                                DriverError::ExecutionError(format!("xlsx list write: {e}"))
                            })?;
                    }
                }
            }
        }

        Ok(workbook)
    }

    /// Convert a zero-based column index to Excel's A1 column letters.
    /// For example: 0 -> "A", 25 -> "Z", 26 -> "AA".
    #[inline]
    fn excel_col_letter(mut idx: usize) -> String {
        // Excel columns are 1-based in their letter representation
        let mut letters: Vec<char> = Vec::with_capacity(3);
        idx += 1;
        while idx > 0 {
            // Convert to 0-based for the current digit then map to letter
            let rem = (idx - 1) % 26;
            letters.push((b'A' + rem as u8) as char);
            idx = (idx - 1) / 26;
        }
        letters.iter().rev().collect()
    }

    /// Append a hidden `__meta__` worksheet containing the given TemplateMetadata.
    #[inline]
    fn append_meta_sheet(
        workbook: &mut Workbook,
        metadata: &TemplateMetadata,
    ) -> Result<(), DriverError> {
        let meta_ws = workbook.add_worksheet();
        let _ = meta_ws.set_hidden(true).set_name("__meta__");

        // headers
        meta_ws
            .write_string(0u32, 0u16, "key")
            .map_err(|e| DriverError::ExecutionError(format!("xlsx meta header: {e}")))?;
        meta_ws
            .write_string(0u32, 1u16, "value")
            .map_err(|e| DriverError::ExecutionError(format!("xlsx meta header: {e}")))?;

        let items: [(&str, Option<String>); 6] = [
            ("driver_type", Some(metadata.driver_type.clone())),
            ("driver_version", metadata.driver_version.clone()),
            ("api_version", metadata.api_version.clone()),
            ("entity", Some(metadata.entity.clone())),
            ("locale", Some(metadata.locale.clone())),
            ("schema_version", Some(metadata.schema_version.clone())),
        ];

        for (idx, (k, v)) in items.iter().enumerate() {
            let r = (idx as u32) + 1;
            let key_str = k.to_string();
            meta_ws
                .write_string(r, 0u16, &key_str)
                .map_err(|e| DriverError::ExecutionError(format!("xlsx meta key: {e}")))?;
            let v_str = v.clone().unwrap_or_default();
            meta_ws
                .write_string(r, 1u16, v_str.as_str())
                .map_err(|e| DriverError::ExecutionError(format!("xlsx meta value: {e}")))?;
        }

        Ok(())
    }

    /// Extract TemplateMetadata from a calamine workbook's hidden `__meta__` sheet.
    #[inline]
    fn extract_metadata_from_workbook<R>(
        workbook: &mut calamine::Xlsx<R>,
    ) -> Result<TemplateMetadata, DriverError>
    where
        R: IoRead + IoSeek,
    {
        let meta_range = workbook
            .worksheet_range("__meta__")
            .map_err(|e| DriverError::ExecutionError(format!("xlsx read: {e}")))?;

        let mut meta_map: BTreeMap<String, String> = BTreeMap::new();
        for (ri, row) in meta_range.rows().enumerate() {
            if ri == 0 {
                continue;
            }
            if row.len() >= 2 {
                let key = row[0].to_string();
                let val = row[1].to_string();
                if !key.trim().is_empty() {
                    meta_map.insert(key, val);
                }
            }
        }

        Ok(TemplateMetadata {
            driver_type: meta_map.get("driver_type").cloned().unwrap_or_default(),
            driver_version: meta_map
                .get("driver_version")
                .cloned()
                .filter(|s| !s.is_empty()),
            api_version: meta_map
                .get("api_version")
                .cloned()
                .filter(|s| !s.is_empty()),
            entity: meta_map.get("entity").cloned().unwrap_or_default(),
            locale: meta_map
                .get("locale")
                .cloned()
                .unwrap_or_else(|| "zh-CN".to_string()),
            schema_version: meta_map
                .get("schema_version")
                .cloned()
                .unwrap_or_else(|| "1.0".to_string()),
        })
    }

    /// Read the first worksheet as data with header row at Row(1).
    #[inline]
    fn read_first_data_range<R>(
        workbook: &mut calamine::Xlsx<R>,
    ) -> Result<calamine::Range<calamine::Data>, DriverError>
    where
        R: IoRead + IoSeek,
    {
        workbook
            .worksheet_range_at(0)
            .ok_or_else(|| DriverError::ExecutionError("missing first worksheet".to_string()))
            .and_then(|r| r.map_err(|e| DriverError::ExecutionError(format!("xlsx read: {e}"))))
    }

    /// Build a workbook and append a hidden `__meta__` sheet containing TemplateMetadata,
    /// then return the serialized buffer.
    #[inline]
    pub fn write_with_meta_to_buffer(
        &self,
        metadata: &TemplateMetadata,
    ) -> Result<Vec<u8>, DriverError> {
        let mut workbook = self.build_template_workbook(&metadata.locale)?;
        Self::append_meta_sheet(&mut workbook, metadata)?;

        workbook
            .save_to_buffer()
            .map_err(|e| DriverError::ExecutionError(format!("xlsx save buffer: {e}")))
    }

    /// Build a workbook and append a hidden `__meta__` sheet containing TemplateMetadata,
    /// then save to a file path.
    #[inline]
    pub fn write_with_meta_to_file<P: AsRef<Path>>(
        &self,
        metadata: &TemplateMetadata,
        path: P,
    ) -> Result<(), DriverError> {
        let mut workbook = self.build_template_workbook(&metadata.locale)?;
        Self::append_meta_sheet(&mut workbook, metadata)?;

        workbook
            .save(path)
            .map_err(|e| DriverError::ExecutionError(format!("xlsx save: {e}")))
    }

    /// Build a workbook and append a hidden `__meta__` sheet containing TemplateMetadata,
    /// then save to a writer.
    #[inline]
    pub fn write_with_meta_to_writer<W>(
        &self,
        metadata: &TemplateMetadata,
        writer: W,
    ) -> Result<(), DriverError>
    where
        W: IoWrite + IoSeek + Send,
    {
        let mut workbook = self.build_template_workbook(&metadata.locale)?;
        Self::append_meta_sheet(&mut workbook, metadata)?;

        workbook
            .save_to_writer(writer)
            .map_err(|e| DriverError::ExecutionError(format!("xlsx save writer: {e}")))
    }

    /// Read `__meta__` sheet first to resolve locale, then parse the first data worksheet
    /// using localized headers. Returns the metadata and the parsed rows.
    #[inline]
    pub fn read_with_meta_from_reader<R>(
        &self,
        reader: R,
    ) -> Result<(TemplateMetadata, Vec<serde_json::Map<String, Json>>), DriverError>
    where
        R: IoRead + IoSeek,
    {
        let mut workbook = calamine::Xlsx::new(reader)
            .map_err(|e| DriverError::ExecutionError(format!("xlsx open: {e}")))?;

        let metadata = Self::extract_metadata_from_workbook(&mut workbook)?;
        let data_range = Self::read_first_data_range(&mut workbook)?;
        let rows = self.read_rows_from_range(&metadata.locale, data_range)?;
        Ok((metadata, rows))
    }

    /// Read `__meta__` sheet first from a file path, then parse the first data worksheet
    /// using localized headers. Returns the metadata and the parsed rows.
    #[inline]
    pub fn read_with_meta_from_file(
        &self,
        path: &Path,
    ) -> Result<(TemplateMetadata, Vec<serde_json::Map<String, Json>>), DriverError> {
        let mut workbook: calamine::Xlsx<_> = calamine::open_workbook(path)
            .map_err(|e| DriverError::ExecutionError(format!("xlsx open: {e}")))?;

        let metadata = Self::extract_metadata_from_workbook(&mut workbook)?;
        let data_range = Self::read_first_data_range(&mut workbook)?;
        let rows = self.read_rows_from_range(&metadata.locale, data_range)?;
        Ok((metadata, rows))
    }

    /// Parse a string into a JSON value for UiDataType::Any.
    ///
    /// Order of attempts:
    /// - null literal ("null")
    /// - boolean aliases (true/false, 1/0, yes/no, on/off, y/n, t/f, 是/否)
    /// - integer (i64)
    /// - float (f64)
    /// - JSON object/array (starts with '{' or '[')
    /// - quoted string (single or double quotes)
    /// - fallback to plain string
    #[inline]
    fn parse_any_str(s: &str) -> Option<Json> {
        let st = s.trim();
        if st.is_empty() {
            return None;
        }
        if st.eq_ignore_ascii_case("null") {
            return Some(Json::Null);
        }
        match st.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" | "y" | "t" | "是" => return Some(Json::Bool(true)),
            "false" | "0" | "no" | "off" | "n" | "f" | "否" => return Some(Json::Bool(false)),
            _ => {}
        }
        if (st.starts_with('{') && st.ends_with('}')) || (st.starts_with('[') && st.ends_with(']'))
        {
            if let Ok(v) = serde_json::from_str::<Json>(st) {
                return Some(v);
            }
        }
        if ((st.starts_with('"') && st.ends_with('"'))
            || (st.starts_with('\'') && st.ends_with('\'')))
            && st.len() >= 2
        {
            let unquoted = &st[1..st.len() - 1];
            return Some(Json::String(unquoted.to_string()));
        }
        if let Ok(n) = st.parse::<i64>() {
            return Some(Json::from(n));
        }
        if let Ok(n) = st.parse::<f64>() {
            return Some(Json::from(n));
        }
        Some(Json::String(st.to_string()))
    }

    /// Fold keys with well-known prefixes into nested JSON objects. Unlimited depth is supported,
    /// e.g., "driver_config.connect.policy.timeout" becomes
    /// { "driver_config": { "connect": { "policy": { "timeout": v }}}}
    /// Also supports "device_driver_config." prefix for device+points combined templates.
    fn fold_special_prefixes(obj: &mut serde_json::Map<String, Json>) {
        for prefix in ["driver_config.", "device_driver_config."] {
            let mut taken: Vec<(String, Json)> = Vec::new();
            // Collect and remove prefixed entries first to avoid borrow issues
            let keys: Vec<String> = obj
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect();
            for k in keys {
                if let Some(v) = obj.remove(&k) {
                    taken.push((k, v));
                }
            }

            if taken.is_empty() {
                continue;
            }

            // Ensure root object exists
            let root_name = &prefix[..prefix.len() - 1]; // strip trailing dot
            let mut root = obj
                .remove(root_name)
                .and_then(|v| match v {
                    Json::Object(m) => Some(m),
                    _ => None,
                })
                .unwrap_or_default();

            for (full_key, value) in taken.into_iter() {
                // split after the prefix
                let remainder = &full_key[prefix.len()..];
                Self::insert_nested(&mut root, remainder, value);
            }

            obj.insert(root_name.to_string(), Json::Object(root));
        }
    }

    /// Insert a value into a nested object using dotted path (unlimited depth).
    fn insert_nested(root: &mut serde_json::Map<String, Json>, path: &str, value: Json) {
        let mut current = root;
        let parts: Vec<&str> = path.split('.').collect();
        let mut to_insert = Some(value);
        for (i, part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                if let Some(v) = to_insert.take() {
                    current.insert((*part).to_string(), v);
                }
            } else {
                current = Self::ensure_child_map(current, part);
            }
        }
    }

    /// Ensure a child key exists as an object and return its mutable map reference.
    fn ensure_child_map<'a>(
        parent: &'a mut serde_json::Map<String, Json>,
        key: &str,
    ) -> &'a mut serde_json::Map<String, Json> {
        use serde_json::map::Entry;
        // Ensure the entry is initialized as an object
        match parent.entry(key.to_string()) {
            Entry::Occupied(mut occ) => {
                if !matches!(occ.get(), Json::Object(_)) {
                    occ.insert(Json::Object(serde_json::Map::new()));
                }
            }
            Entry::Vacant(vac) => {
                vac.insert(Json::Object(serde_json::Map::new()));
            }
        }
        // Now safely get the mutable object reference
        match parent.get_mut(key) {
            Some(Json::Object(m)) => m,
            _ => unreachable!(),
        }
    }

    /// Get a value from a nested object using dotted path, e.g., "driver_config.ca".
    #[inline]
    fn get_value_by_path<'a>(
        obj: &'a serde_json::Map<String, Json>,
        path: &str,
    ) -> Option<&'a Json> {
        let mut current_map = obj;
        let mut iter = path.split('.').peekable();
        while let Some(seg) = iter.next() {
            let v = current_map.get(seg)?;
            if iter.peek().is_none() {
                return Some(v);
            }
            match v {
                Json::Object(m) => {
                    current_map = m;
                }
                _ => return None,
            }
        }
        None
    }

    /// Remove a value from a nested object using dotted path. Returns removed value if any.
    #[inline]
    fn remove_by_path(obj: &mut serde_json::Map<String, Json>, path: &str) -> Option<Json> {
        let mut current_map = obj;
        let mut iter = path.split('.').peekable();
        while let Some(seg) = iter.next() {
            if iter.peek().is_none() {
                return current_map.remove(seg);
            }
            let next = current_map.get_mut(seg)?;
            match next {
                Json::Object(m) => {
                    current_map = m;
                }
                _ => return None,
            }
        }
        None
    }
}

/// Validation status code for a single field in import flows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValidationCode {
    Required,
    TypeMismatch,
    Range,
    Length,
    Pattern,
    DiscriminatorMissing,
    DiscriminatorMismatch,
    InvisibleFilled,
    Unknown,
}

/// A single field-level error captured during validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldError {
    pub row: usize,
    pub field: String,
    pub code: ValidationCode,
    pub message: String,
}

/// Overall summary stats for an import validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationSummary {
    pub total: usize,
    pub valid: usize,
    pub invalid: usize,
    pub warn: usize,
}

/// Public response shape for import preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportValidationPreview {
    pub summary: ValidationSummary,
    pub error_preview: Vec<FieldError>,
    pub preview: Vec<serde_json::Map<String, Json>>, // normalized rows (first N)
}

/// A row that passed validation (or only with warnings)
#[derive(Debug, Clone)]
pub struct ValidatedRow {
    pub row_index: usize,
    pub values: serde_json::Map<String, Json>,
}

/// Domain mapping trait for converting a normalized `ValidatedRow` into a domain model.
///
/// Implementors should extract required fields from `row.values` and apply any
/// driver- or entity-specific defaults using the provided `context`.
pub trait FromValidatedRow: Sized {
    /// Create a domain object from a validated row and mapping context.
    fn from_validated_row(
        row: &ValidatedRow,
        context: &RowMappingContext,
    ) -> Result<Self, DriverError>;
}

/// Mapping context that provides ambient information for domain conversion.
#[derive(Debug, Clone)]
pub struct RowMappingContext {
    /// Optional entity id for association
    pub entity_id: i32,
    /// Driver type to propagate to domain models
    pub driver_type: String,
    /// Locale used during import (from template metadata)
    pub locale: String,
}

impl DriverEntityTemplate {
    /// Map validated rows to a domain vector using the provided mapping trait.
    pub fn map_to_domain<T>(
        &self,
        validated_rows: Vec<ValidatedRow>,
        context: RowMappingContext,
    ) -> Result<Vec<T>, DriverError>
    where
        T: FromValidatedRow,
    {
        validated_rows
            .iter()
            .map(|row| T::from_validated_row(row, &context))
            .collect()
    }
}
