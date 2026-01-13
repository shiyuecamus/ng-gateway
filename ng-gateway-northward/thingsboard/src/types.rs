// ThingsBoard Gateway API message types
// These types are defined for future RPC/Attribute handling
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::collections::HashMap;

/// ThingsBoard attribute update notification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TbAttributeChanged {
    pub device: String,
    pub data: HashMap<String, serde_json::Value>,
}

/// ThingsBoard gateway RPC request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TbGatewayRpcRequest {
    pub method: String,
    pub params: Option<serde_json::Value>,
}

/// ThingsBoard sub-device RPC request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TbSubDeviceRpcRequest {
    pub device: String,
    pub data: TbRpcData,
}

/// ThingsBoard RPC data structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TbRpcData {
    pub id: i32,
    pub method: String,
    pub params: Option<serde_json::Value>,
}

/// ThingsBoard gateway RPC response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TbGatewayRpcResponse {
    pub id: i32,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}

/// ThingsBoard sub-device RPC response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TbSubDeviceRpcResponse<'a> {
    pub id: i32,
    pub device: &'a str,
    pub data: TbSubDeviceRpcData<'a>,
}

/// ThingsBoard sub-device RPC response code
#[derive(Debug, Clone, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum TbSubDeviceRpcResponseCode {
    Success = 1,
    Error = -1,
}

/// ThingsBoard sub-device RPC response data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TbSubDeviceRpcData<'a> {
    pub code: TbSubDeviceRpcResponseCode,
    pub message: Option<&'a str>,
    pub data: Option<serde_json::Value>,
}
