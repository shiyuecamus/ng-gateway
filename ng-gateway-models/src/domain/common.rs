use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_aux::prelude::*;
use serde_with::{serde_as, DisplayFromStr};
use validator::Validate;

#[allow(unused)]
#[derive(Deserialize, Validate)]
pub struct AuthRequest {
    #[validate(range(min = 1000, max = 9999))]
    id: u64,
}

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct PageParams {
    #[serde(default, deserialize_with = "deserialize_option_number_from_string")]
    #[validate(required(message = "page is required"))]
    pub page: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_option_number_from_string")]
    #[validate(required(message = "pageSize is required"))]
    pub page_size: Option<u32>,
}

#[allow(unused)]
#[serde_as]
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct TimeRangeParams {
    #[serde_as(as = "Option<DisplayFromStr>")]
    pub start_time: Option<DateTime<Utc>>,
    #[serde_as(as = "Option<DisplayFromStr>")]
    pub end_time: Option<DateTime<Utc>>,
}

/// 通用的批量删除请求体
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeletePayload {
    /// 要删除的主键 ID 列表
    #[validate(length(min = 1, message = "ids is required"))]
    pub ids: Vec<i32>,
}

/// 根据设备清空资源的通用请求体
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct ClearByDevicePayload {
    /// 设备 ID
    #[validate(range(min = 1, message = "deviceId is required"))]
    pub device_id: i32,
}

/// 根据通道清空资源的通用请求体
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct ClearByChannelPayload {
    /// 通道 ID
    #[validate(range(min = 1, message = "channelId is required"))]
    pub channel_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageResult<T> {
    pub pages: u32,
    pub records: Vec<T>,
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
}
