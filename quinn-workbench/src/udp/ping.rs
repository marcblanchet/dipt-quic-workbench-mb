use crate::config::traffic::PingTraffic;
use futures::FutureExt;
use in_memory_network::async_rt;
use in_memory_network::async_rt::time::Instant;
use in_memory_network::quinn_interop::{BufsAndMeta, InMemoryUdpSocket};
use parking_lot::Mutex;
use quinn::AsyncUdpSocket;
use quinn::udp::Transmit;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

pub fn run_server_forever(server_socket: InMemoryUdpSocket, client_ip: IpAddr) {
    async_rt::spawn(async move {
        let mut bufs_and_meta = BufsAndMeta::new(1200, 5);

        loop {
            // Receive next transmits
            for packet in server_socket.receive(&mut bufs_and_meta).await.unwrap() {
                assert_eq!(packet.source_addr.ip(), client_ip);

                // Echo transmit
                server_socket
                    .try_send(&Transmit {
                        destination: SocketAddr::new(client_ip, 8080),
                        ecn: None,
                        contents: packet.payload,
                        segment_size: None,
                        src_ip: None,
                    })
                    .unwrap();
            }
        }
    });
}

pub fn run_traffic_pattern(
    client_socket: InMemoryUdpSocket,
    t: &PingTraffic,
    simulation_start: Instant,
) -> async_rt::JoinHandle<()> {
    let client_socket = Arc::new(client_socket);
    let interval = Duration::from_millis(t.interval_ms);
    let duration = Duration::from_millis(t.duration_ms);
    let deadline = Duration::from_millis(t.deadline_ms);
    let server_ip = t.server_ip;

    let in_flight = Arc::new(Mutex::new(HashMap::new()));
    let lost = Arc::new(Mutex::new(Vec::new()));

    let (sender_done_tx, mut sender_done_rx) = futures::channel::oneshot::channel();

    // Sender
    let client_socket_cp = client_socket.clone();
    let in_flight_cp = in_flight.clone();
    let lost_cp = lost.clone();
    let start_at = Duration::from_millis(t.start_at_ms);
    let task = async_rt::spawn(async move {
        // Don't start until the specified moment
        let time_until_start = start_at.saturating_sub(simulation_start.elapsed());
        if !time_until_start.is_zero() {
            async_rt::time::sleep(time_until_start).await;
        }

        let ping_start = Instant::now();
        let mut ping_nr: u64 = 0;
        while ping_start.elapsed() < duration {
            // Send ping
            let payload = ping_nr.to_le_bytes();

            client_socket_cp
                .try_send(&Transmit {
                    destination: SocketAddr::new(server_ip, 8080),
                    ecn: None,
                    contents: &payload,
                    segment_size: None,
                    src_ip: None,
                })
                .unwrap();

            let sent_ping_nr = ping_nr;
            in_flight_cp.lock().insert(sent_ping_nr, Instant::now());
            ping_nr += 1;

            // Track pings as lost after the deadline has passed
            let in_flight_cp = in_flight_cp.clone();
            let lost_cp = lost_cp.clone();
            async_rt::spawn(async move {
                async_rt::time::sleep(deadline).await;
                if let Some(ping_sent) = in_flight_cp.lock().remove(&sent_ping_nr) {
                    lost_cp.lock().push(sent_ping_nr);
                    let ping_lost = Instant::now();
                    println!(
                        "P{} | {:.2}s - SENT | {:.2}s - LOST | {:.2}s - DURATION",
                        sent_ping_nr,
                        (ping_sent - simulation_start).as_secs_f64(),
                        (ping_lost - simulation_start).as_secs_f64(),
                        (ping_lost - ping_sent).as_secs_f64()
                    );
                }
            });

            // Sleep before sending the next ping
            async_rt::time::sleep(interval).await;
        }

        _ = sender_done_tx.send(());
        println!("{:.2}s Done", simulation_start.elapsed().as_secs_f64());
    });

    // Receiver
    async_rt::spawn(async move {
        let mut bufs_and_meta = BufsAndMeta::new(1200, 5);

        loop {
            // Receive next transmits, shutting down the task when the sender is done
            let packets = futures::select! {
                _ = sender_done_rx => {
                    return;
                }
                packets = client_socket.receive(&mut bufs_and_meta).fuse() => {
                    packets.unwrap()
                }
            };

            for packet in packets {
                assert_eq!(packet.source_addr.ip(), server_ip);

                let ping_nr = u64::from_le_bytes(packet.payload.try_into().unwrap());

                if let Some(ping_sent) = in_flight.lock().remove(&ping_nr) {
                    let ping_received = Instant::now();
                    println!(
                        "P{ping_nr} | {:.2}s - SENT | {:.2}s - RECEIVED | {:.2}s - DURATION",
                        (ping_sent - simulation_start).as_secs_f64(),
                        (ping_received - simulation_start).as_secs_f64(),
                        (ping_received - ping_sent).as_secs_f64()
                    );
                }
            }
        }
    });

    task
}
