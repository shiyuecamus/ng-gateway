use super::{
    error::Error,
    frame::asdu::{CauseOfTransmission, CommonAddr, TypeID},
    session::{create_with_stream, state::SessionConfig, Session, SessionEventLoop},
};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};
use tokio::{net::TcpStream, time};

#[derive(Debug, Clone)]
pub struct ClientOption {
    socket_addr: SocketAddr,
    connect_timeout_ms: u64,
    t0_ms: u64,
    t1_ms: u64,
    t2_ms: u64,
    t3_ms: u64,
    k_window: u16,
    w_threshold: u16,
    send_queue_capacity: usize,
    tcp_nodelay: bool,
    max_pending_asdu_bytes: usize,
    discard_low_priority_when_window_full: bool,
    merge_low_priority: bool,
    low_prio_flush_max_age_ms: u64,
}

impl ClientOption {
    pub fn new(socket_addr: SocketAddr) -> Self {
        ClientOption {
            socket_addr,
            ..Default::default()
        }
    }
}

impl From<ClientOption> for SessionConfig {
    fn from(option: ClientOption) -> Self {
        Self {
            connection_timeout_ms: option.connect_timeout_ms,
            t0_ms: option.t0_ms,
            t1_ms: option.t1_ms,
            t2_ms: option.t2_ms,
            t3_ms: option.t3_ms,
            k_window: option.k_window,
            w_threshold: option.w_threshold,
            send_queue_capacity: option.send_queue_capacity,
            tcp_nodelay: option.tcp_nodelay,
            max_pending_asdu_bytes: option.max_pending_asdu_bytes,
            discard_low_priority_when_window_full: option.discard_low_priority_when_window_full,
            merge_low_priority: option.merge_low_priority,
            low_prio_flush_max_age_ms: option.low_prio_flush_max_age_ms,
        }
    }
}

impl Default for ClientOption {
    fn default() -> Self {
        Self {
            socket_addr: SocketAddr::from((IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 2404)),
            connect_timeout_ms: 10_000,
            t0_ms: 10000,
            t1_ms: 15000,
            t2_ms: 10000,
            t3_ms: 20000,
            k_window: 12,
            w_threshold: 8,
            send_queue_capacity: 1024,
            tcp_nodelay: true,
            max_pending_asdu_bytes: 1024 * 1024,
            discard_low_priority_when_window_full: true,
            merge_low_priority: true,
            low_prio_flush_max_age_ms: 2000,
        }
    }
}

/// ClientBuilder constructs a Client with fluent configuration API.
#[derive(Debug, Clone)]
pub struct ClientBuilder {
    option: ClientOption,
}

impl ClientBuilder {
    pub fn new() -> Self {
        Self {
            option: ClientOption::default(),
        }
    }

    pub fn socket_addr(mut self, addr: SocketAddr) -> Self {
        self.option.socket_addr = addr;
        self
    }

    pub fn connect_timeout(mut self, millis: u64) -> Self {
        self.option.connect_timeout_ms = millis;
        self
    }

    pub fn t0(mut self, millis: u64) -> Self {
        self.option.t0_ms = millis;
        self
    }

    pub fn t1(mut self, millis: u64) -> Self {
        self.option.t1_ms = millis;
        self
    }

    pub fn t2(mut self, millis: u64) -> Self {
        self.option.t2_ms = millis;
        self
    }

    pub fn t3(mut self, millis: u64) -> Self {
        self.option.t3_ms = millis;
        self
    }

    pub fn window_k(mut self, k: u16) -> Self {
        self.option.k_window = k;
        self
    }

    pub fn ack_threshold_w(mut self, w: u16) -> Self {
        self.option.w_threshold = w;
        self
    }

    pub fn send_queue_capacity(mut self, cap: usize) -> Self {
        self.option.send_queue_capacity = cap;
        self
    }

    pub fn tcp_nodelay(mut self, on: bool) -> Self {
        self.option.tcp_nodelay = on;
        self
    }

    pub fn max_pending_asdu_bytes(mut self, bytes: usize) -> Self {
        self.option.max_pending_asdu_bytes = bytes;
        self
    }

    pub fn allow_low_priority_discard(mut self, on: bool) -> Self {
        self.option.discard_low_priority_when_window_full = on;
        self
    }

    pub fn merge_low_priority(mut self, on: bool) -> Self {
        self.option.merge_low_priority = on;
        self
    }

    pub fn low_prio_flush_max_age(mut self, millis: u64) -> Self {
        self.option.low_prio_flush_max_age_ms = millis;
        self
    }

    pub fn build(self) -> Client {
        Client::new(self.option)
    }
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Client {
    op: ClientOption,
}

impl Client {
    pub fn new(option: ClientOption) -> Self {
        Client { op: option }
    }

    /// Connect to the remote IEC104 server and return a Session and its EventLoop.
    /// Steps:
    /// 1) Establish TCP with timeout
    /// 2) Create `Session` and `SessionEventLoop`
    /// 3) Return (Session, EventLoop) to caller to run/poll
    pub async fn connect(self) -> Result<(Arc<Session>, SessionEventLoop), Error> {
        let addr = self.op.socket_addr;
        let timeout = time::Duration::from_millis(self.op.connect_timeout_ms);

        let stream = match time::timeout(timeout, TcpStream::connect(addr)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(Error::Io(e)),
            Err(_) => return Err(Error::ErrConnectTimeout),
        };

        if let Err(e) = stream.set_nodelay(self.op.tcp_nodelay) {
            tracing::warn!(error=%e, tcp_nodelay=self.op.tcp_nodelay, "set TCP_NODELAY failed");
        }

        // Build Session/Loop bound to this pre-established stream
        let (session, event_loop) = create_with_stream(addr, self.op.clone().into(), stream);

        Ok((session, event_loop))
    }
}
