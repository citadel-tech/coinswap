#[derive(Debug, Clone)]
pub struct BlockRef {
    pub height: u64,
    pub hash: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum BackendEvent {
    TxSeen { raw_tx: Vec<u8> },
    BlockConnected(BlockRef),
}

pub struct ZmqBackend {
    socket: zmq::Socket,
}

impl ZmqBackend {
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
