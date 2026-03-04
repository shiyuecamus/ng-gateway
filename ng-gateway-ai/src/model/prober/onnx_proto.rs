//! Hand-declared subset of the ONNX `ModelProto` protobuf schema.
//!
//! Uses `prost::Message` derive to decode only the top-level fields we need:
//! producer info, opset version, and metadata properties. The full graph
//! definition (field 7) is skipped automatically by prost since we don't
//! declare it.
//!
//! Reference: <https://github.com/onnx/onnx/blob/main/onnx/onnx.proto3>

/// Top-level ONNX model container.
///
/// Only declares the fields we extract during probing. Unknown fields
/// (including the heavy `GraphProto` at field 7) are silently skipped
/// by `prost::Message::decode`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ModelProto {
    /// IR version (field 1).
    #[prost(int64, tag = "1")]
    pub ir_version: i64,
    /// Opset imports (field 2, repeated).
    #[prost(message, repeated, tag = "2")]
    pub opset_import: Vec<OperatorSetIdProto>,
    /// Producer framework name (field 3).
    #[prost(string, tag = "3")]
    pub producer_name: String,
    /// Producer framework version (field 4).
    #[prost(string, tag = "4")]
    pub producer_version: String,
    /// Model domain (field 5).
    #[prost(string, tag = "5")]
    pub domain: String,
    /// Model version integer (field 6).
    #[prost(int64, tag = "6")]
    pub model_version: i64,
    /// Documentation string (field 7).
    #[prost(string, tag = "7")]
    pub doc_string: String,
    /// Metadata key-value pairs (field 14, repeated).
    #[prost(message, repeated, tag = "14")]
    pub metadata_props: Vec<StringStringEntryProto>,
}

/// Opset identifier within a model.
#[derive(Clone, PartialEq, prost::Message)]
pub struct OperatorSetIdProto {
    /// Opset domain (empty string = default ONNX domain).
    #[prost(string, tag = "1")]
    pub domain: String,
    /// Opset version.
    #[prost(int64, tag = "2")]
    pub version: i64,
}

/// Key-value metadata entry.
#[derive(Clone, PartialEq, prost::Message)]
pub struct StringStringEntryProto {
    /// Metadata key.
    #[prost(string, tag = "1")]
    pub key: String,
    /// Metadata value.
    #[prost(string, tag = "2")]
    pub value: String,
}
