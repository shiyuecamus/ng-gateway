//! Annotation worker — background frame rendering pipeline.

use crate::{decoded::DecodedFrame, pipeline::annotator::FrameAnnotator};
use bytes::Bytes;
use dashmap::DashMap;
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::{domain::prelude::AnalysisCore, entities::ai::pipeline::AnnotationConfig};
use std::{
    cmp::Reverse,
    collections::{HashMap, VecDeque},
    sync::Arc,
};
use tokio::{
    sync::mpsc::{self, error::TryRecvError},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Async annotation request submitted from the inference hot path.
pub(super) struct AnnotateRequest {
    pub channel_id: i32,
    pub frame: Arc<DecodedFrame>,
    pub core: Arc<AnalysisCore>,
    pub config: Arc<AnnotationConfig>,
}

/// Run the annotation worker loop, processing requests from the bounded channel.
///
/// Uses bounded internal parallelism to improve throughput under load.
/// Timeliness policy is "latest wins per channel": while draining the mailbox,
/// stale requests for the same channel are coalesced and dropped.
pub(super) async fn annotation_worker_loop(
    mut rx: mpsc::Receiver<AnnotateRequest>,
    annotator: Arc<dyn FrameAnnotator>,
    latest_annotated_frames: Arc<DashMap<i32, Bytes>>,
    shutdown_token: CancellationToken,
    max_in_flight: usize,
) {
    let max_in_flight = max_in_flight.max(1);
    let mut pending = VecDeque::new();
    let mut in_flight = JoinSet::new();
    let mut receiver_closed = false;

    loop {
        // Fill available in-flight slots first.
        while in_flight.len() < max_in_flight {
            let Some(request) = pending.pop_front() else {
                break;
            };
            spawn_annotation_job(&mut in_flight, Arc::clone(&annotator), request);
        }

        if receiver_closed && in_flight.is_empty() && pending.is_empty() {
            break;
        }

        tokio::select! {
            biased;
            _ = shutdown_token.cancelled() => {
                pending.clear();
                receiver_closed = true;
                if in_flight.is_empty() {
                    break;
                }
            }
            joined = in_flight.join_next(), if !in_flight.is_empty() => {
                match joined {
                    Some(Ok((channel_id, Ok(jpeg)))) => {
                        latest_annotated_frames.insert(channel_id, jpeg);
                    }
                    Some(Ok((channel_id, Err(error)))) => {
                        warn!(channel_id, error = %error, "background annotation failed");
                    }
                    Some(Err(join_error)) => {
                        warn!(error = %join_error, "annotation worker task panicked");
                    }
                    None => {}
                }
            }
            maybe_request = rx.recv(), if !receiver_closed => {
                match maybe_request {
                    Some(first) => enqueue_latest_by_channel(first, &mut rx, &mut pending, max_in_flight * 8),
                    None => {
                        receiver_closed = true;
                    }
                }
            }
            else => {
                // Receiver is closed and no in-flight jobs are left.
                break;
            }
        }
    }

    // Best-effort cancellation on shutdown path.
    if !in_flight.is_empty() {
        debug!(
            remaining_jobs = in_flight.len(),
            "aborting remaining annotation jobs"
        );
        in_flight.abort_all();
    }
}

fn spawn_annotation_job(
    in_flight: &mut JoinSet<(i32, Result<Bytes, AiEngineError>)>,
    annotator: Arc<dyn FrameAnnotator>,
    request: AnnotateRequest,
) {
    in_flight.spawn(async move {
        let channel_id = request.channel_id;
        let worker_annotator = Arc::clone(&annotator);
        let result = tokio::task::spawn_blocking(move || {
            worker_annotator.annotate(
                request.frame.as_ref(),
                request.core.as_ref(),
                request.config.as_ref(),
            )
        })
        .await
        .map_err(|e| {
            ng_gateway_error::ai::AiEngineError::InternalError(format!(
                "annotation blocking task join error: {e}"
            ))
        })
        .and_then(|inner| inner);
        (channel_id, result)
    });
}

/// Enqueue a burst with "latest frame wins" coalescing per channel.
fn enqueue_latest_by_channel(
    first: AnnotateRequest,
    rx: &mut mpsc::Receiver<AnnotateRequest>,
    pending: &mut VecDeque<AnnotateRequest>,
    max_burst_drain: usize,
) {
    let mut latest: HashMap<i32, AnnotateRequest> = HashMap::with_capacity(16);
    latest.insert(first.channel_id, first);

    let mut drained = 0usize;
    while drained < max_burst_drain {
        match rx.try_recv() {
            Ok(req) => {
                latest.insert(req.channel_id, req);
                drained += 1;
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => {
                break;
            }
        }
    }

    let mut burst: Vec<AnnotateRequest> = latest.into_values().collect();
    // Newest frames are scheduled first.
    burst.sort_unstable_by_key(|req| Reverse(req.core.frame_seq));
    pending.extend(burst);
}
