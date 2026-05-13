use crate::config::traffic::UdpOneDirectionTraffic;
use bytes::Bytes;
use in_memory_network::async_rt;
use in_memory_network::async_rt::time::Instant;
use in_memory_network::network::InMemoryNetwork;
use in_memory_network::transmit::{DEFAULT_TTL, OwnedTransmit};
use std::sync::Arc;
use std::time::Duration;

const PAYLOAD_CHUNK_SIZE_BYTES: usize = 1200;

pub fn run_traffic_pattern(
    network: Arc<InMemoryNetwork>,
    t: &UdpOneDirectionTraffic,
) -> async_rt::JoinHandle<()> {
    let sender_node = network.node(t.source.ip()).clone();
    let send_interval = Duration::from_millis(t.send_interval_ms);
    let duration = Duration::from_millis(t.duration_ms);
    let payload_bytes = t.payload_bytes;
    let source = t.source;
    let target = t.target;
    async_rt::spawn(async move {
        let chunk = Bytes::from(vec![0; PAYLOAD_CHUNK_SIZE_BYTES]);
        let start = Instant::now();

        while start.elapsed() < duration {
            let mut pending_send = payload_bytes as usize;
            while pending_send > 0 {
                let next_chunk_size_bytes = pending_send.min(PAYLOAD_CHUNK_SIZE_BYTES);
                pending_send -= next_chunk_size_bytes;

                network.send_udp(
                    sender_node.clone(),
                    OwnedTransmit {
                        source,
                        destination: target,
                        ecn: None,
                        contents: chunk.slice(..next_chunk_size_bytes),
                        segment_size: None,
                        ttl: DEFAULT_TTL,
                    },
                );
            }

            async_rt::time::sleep(send_interval).await;
        }
    })
}
