//! ZMQ subscriber for Bitcoin node notifications.

/// Reference to a block received via ZMQ.
#[derive(Debug, Clone)]
pub struct BlockRef {
    /// Height of the block if known / 0 when unavailable.
    pub height: u64,
    /// Raw block hash.
    pub hash: Vec<u8>,
}

/// Events generated from the ZMQ subscriber.
#[derive(Debug, Clone)]
pub enum BackendEvent {
    /// Notifies when a transaction is seen (mempool or block) and includes the raw bytes.
    TxSeen {
        /// Raw transaction bytes.
        raw_tx: Vec<u8>,
    },
    /// Notifies when chain tip is updated with rawblock data.
    BlockConnected(BlockRef),
}

/// ZMQ backend used by the watcher to subscribe to node notifications.
pub struct ZmqBackend {
    socket: zmq::Socket,
}

impl ZmqBackend {
    /// Connects to a ZMQ endpoint and subscribes to rawtx and rawblock topics.
    pub fn new(endpoint: &str) -> Self {
        let ctx = zmq::Context::new();
        let socket = ctx.socket(zmq::SUB).expect("socket");

        socket.connect(endpoint).expect("connect");

        // Subscribe to both topics
        socket.set_subscribe(b"rawtx").expect("subscribe rawtx");
        socket
            .set_subscribe(b"rawblock")
            .expect("subscribe rawblock");

        Self { socket }
    }

    fn recv_event(&self) -> Option<(String, Vec<u8>)> {
        let msg = self.socket.recv_multipart(zmq::DONTWAIT).ok()?;
        if msg.len() < 2 {
            return None;
        }

        let topic = String::from_utf8_lossy(&msg[0]).to_string();
        let payload = msg[1].clone();
        Some((topic, payload))
    }
}

impl ZmqBackend {
    /// Non-blocking poll for the next backend event.
    pub fn poll(&mut self) -> Option<BackendEvent> {
        let (topic, payload) = self.recv_event()?;

        match topic.as_str() {
            "rawtx" => Some(BackendEvent::TxSeen { raw_tx: payload }),
            "rawblock" => Some(BackendEvent::BlockConnected(BlockRef {
                height: 0,
                hash: payload,
            })),
            _ => None,
        }
    }
}
