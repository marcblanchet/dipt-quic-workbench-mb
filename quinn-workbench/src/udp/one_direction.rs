use crate::config::traffic::UdpOneDirectionTraffic;
use in_memory_network::async_rt;
use in_memory_network::async_rt::time::Instant;
use in_memory_network::quinn_interop::InMemoryUdpSocket;
use quinn::udp::Transmit;
use std::time::Duration;

const PAYLOAD_CHUNK_SIZE_BYTES: usize = 1200;

pub fn run_traffic_pattern(
    client_socket: InMemoryUdpSocket,
    t: &UdpOneDirectionTraffic,
) -> async_rt::JoinHandle<()> {
    let send_interval = Duration::from_millis(t.send_interval_ms);
    let duration = Duration::from_millis(t.duration_ms);
    let payload_bytes = t.payload_bytes;
    let target = t.target;
    async_rt::spawn(async move {
        let chunk = vec![0; PAYLOAD_CHUNK_SIZE_BYTES];
        let start = Instant::now();

        while start.elapsed() < duration {
            let mut pending_send = payload_bytes as usize;
            while pending_send > 0 {
                let next_chunk_size_bytes = pending_send.min(PAYLOAD_CHUNK_SIZE_BYTES);
                pending_send -= next_chunk_size_bytes;

                client_socket.send(&Transmit {
                    destination: target.into(),
                    ecn: None,
                    contents: &chunk[..next_chunk_size_bytes],
                    segment_size: None,
                    src_ip: None,
                });
            }

            async_rt::time::sleep(send_interval).await;
        }
    })
}
