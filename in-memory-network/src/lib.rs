#![allow(clippy::type_complexity)]

pub mod async_rt;
pub mod network;
pub mod pcap_exporter;
pub mod quinn_interop;
pub mod tracing;
pub mod transmit;
mod util;

use std::sync::Arc;
use transmit::OwnedTransmit;

#[derive(Clone)]
pub struct InTransitData {
    id: uuid::Uuid,
    duplicate: bool,
    source_id: Arc<str>,
    transmit: OwnedTransmit,
    number: u64,
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::network::event::{
        LinkEventPayload, NetworkEvent, NetworkEventKind, NetworkEvents, UpdateLinkStatus,
    };
    use crate::network::ip::Ipv4Cidr;
    use crate::network::route::{IpRange, Route};
    use crate::network::spec::{NetworkInterface, NetworkLinkSpec, NetworkNodeSpec, NetworkSpec};
    use crate::network::{InMemoryNetwork, PcapOptions};
    use crate::quinn_interop::BufsAndMeta;
    use crate::tracing::tracer::SimulationStepTracer;
    use crate::transmit::DEFAULT_TTL;
    use bon::builder;
    use fastrand::Rng;
    use quinn::crypto::rustls::QuicClientConfig;
    use quinn::rustls::RootCertStore;
    use quinn::rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
    use quinn::{ClientConfig, Endpoint, EndpointConfig, ServerConfig, rustls};
    use std::net::Ipv4Addr;
    use std::sync::Arc;
    use std::time::Duration;

    const SERVER_ADDR: Ipv4Cidr = Ipv4Cidr::from_ipv4(Ipv4Addr::new(88, 88, 88, 88), 24);
    const ROUTER1_ADDR: Ipv4Cidr = Ipv4Cidr::from_ipv4(Ipv4Addr::new(200, 200, 200, 1), 24);
    const ROUTER2_ADDR: Ipv4Cidr = Ipv4Cidr::from_ipv4(Ipv4Addr::new(200, 200, 200, 2), 24);
    const CLIENT_ADDR: Ipv4Cidr = Ipv4Cidr::from_ipv4(Ipv4Addr::new(1, 1, 1, 1), 24);
    const BANDWIDTH_100_MBPS: u64 = 1000 * 1000 * 100;
    const HOST_PORT: u16 = 8080;

    #[builder]
    fn default_network(
        bandwidth_bps: Option<u64>,
        events: Option<Vec<NetworkEvent>>,
    ) -> Arc<InMemoryNetwork> {
        let bandwidth_bps = bandwidth_bps.unwrap_or(BANDWIDTH_100_MBPS);

        let default_link_delay = Duration::from_millis(10);
        let client_cidr = IpRange::from_cidr(CLIENT_ADDR);
        let server_cidr = IpRange::from_cidr(SERVER_ADDR);

        // SERVER_ADDR -> ROUTER1_ADDR

        let network_spec = NetworkSpec {
            nodes: vec![
                NetworkNodeSpec {
                    id: "server".into(),
                    interfaces: vec![NetworkInterface {
                        addresses: vec![SERVER_ADDR],
                        routes: vec![Route {
                            destination: client_cidr.clone(),
                            next: ROUTER1_ADDR.as_ip_addr(),
                            cost: 0,
                        }],
                    }],
                    buffer_size_bytes: u64::MAX,
                    packet_loss_ratio: 0.0,
                    packet_duplication_ratio: 0.0,
                },
                NetworkNodeSpec {
                    id: "client".into(),
                    interfaces: vec![NetworkInterface {
                        addresses: vec![CLIENT_ADDR],
                        routes: vec![Route {
                            destination: server_cidr.clone(),
                            next: ROUTER2_ADDR.as_ip_addr(),
                            cost: 0,
                        }],
                    }],
                    buffer_size_bytes: u64::MAX,
                    packet_loss_ratio: 0.0,
                    packet_duplication_ratio: 0.0,
                },
                NetworkNodeSpec {
                    id: "router1".into(),
                    interfaces: vec![NetworkInterface {
                        addresses: vec![ROUTER1_ADDR],
                        routes: vec![
                            Route {
                                destination: client_cidr.clone(),
                                next: ROUTER2_ADDR.as_ip_addr(),
                                cost: 0,
                            },
                            Route {
                                destination: server_cidr.clone(),
                                next: SERVER_ADDR.as_ip_addr(),
                                cost: 0,
                            },
                        ],
                    }],
                    buffer_size_bytes: u64::MAX,
                    packet_loss_ratio: 0.0,
                    packet_duplication_ratio: 0.0,
                },
                NetworkNodeSpec {
                    id: "router2".into(),
                    interfaces: vec![NetworkInterface {
                        addresses: vec![ROUTER2_ADDR],
                        routes: vec![
                            Route {
                                destination: client_cidr.clone(),
                                next: CLIENT_ADDR.as_ip_addr(),
                                cost: 0,
                            },
                            Route {
                                destination: server_cidr.clone(),
                                next: ROUTER1_ADDR.as_ip_addr(),
                                cost: 0,
                            },
                        ],
                    }],
                    buffer_size_bytes: u64::MAX,
                    packet_loss_ratio: 0.0,
                    packet_duplication_ratio: 0.0,
                },
            ],
            links: vec![
                NetworkLinkSpec {
                    id: "server-router1".to_string().into_boxed_str().into(),
                    source: SERVER_ADDR.as_ip_addr(),
                    target: ROUTER1_ADDR.as_ip_addr(),
                    delay: default_link_delay,
                    bandwidth_bps,
                    congestion_event_ratio: 0.0,
                    extra_delay: Default::default(),
                    extra_delay_ratio: 0.0,
                },
                NetworkLinkSpec {
                    id: "router1-router2".to_string().into_boxed_str().into(),
                    source: ROUTER1_ADDR.as_ip_addr(),
                    target: ROUTER2_ADDR.as_ip_addr(),
                    delay: default_link_delay,
                    bandwidth_bps,
                    congestion_event_ratio: 0.0,
                    extra_delay: Default::default(),
                    extra_delay_ratio: 0.0,
                },
                NetworkLinkSpec {
                    id: "router2-client".to_string().into_boxed_str().into(),
                    source: ROUTER2_ADDR.as_ip_addr(),
                    target: CLIENT_ADDR.as_ip_addr(),
                    delay: default_link_delay,
                    bandwidth_bps,
                    congestion_event_ratio: 0.0,
                    extra_delay: Default::default(),
                    extra_delay_ratio: 0.0,
                },
                NetworkLinkSpec {
                    id: "router1-server".to_string().into_boxed_str().into(),
                    source: ROUTER1_ADDR.as_ip_addr(),
                    target: SERVER_ADDR.as_ip_addr(),
                    delay: default_link_delay,
                    bandwidth_bps,
                    congestion_event_ratio: 0.0,
                    extra_delay: Default::default(),
                    extra_delay_ratio: 0.0,
                },
                NetworkLinkSpec {
                    id: "router2-router1".to_string().into_boxed_str().into(),
                    source: ROUTER2_ADDR.as_ip_addr(),
                    target: ROUTER1_ADDR.as_ip_addr(),
                    delay: default_link_delay,
                    bandwidth_bps,
                    congestion_event_ratio: 0.0,
                    extra_delay: Default::default(),
                    extra_delay_ratio: 0.0,
                },
                NetworkLinkSpec {
                    id: "client-router2".to_string().into_boxed_str().into(),
                    source: CLIENT_ADDR.as_ip_addr(),
                    target: ROUTER2_ADDR.as_ip_addr(),
                    delay: default_link_delay,
                    bandwidth_bps,
                    congestion_event_ratio: 0.0,
                    extra_delay: Default::default(),
                    extra_delay_ratio: 0.0,
                },
            ],
        };

        InMemoryNetwork::initialize(
            network_spec.clone(),
            NetworkEvents::new(
                events.unwrap_or_default(),
                &network_spec.nodes,
                &network_spec.links,
            ),
            Arc::new(SimulationStepTracer::new(network_spec)),
            Rng::with_seed(42),
            async_rt::time::Instant::now(),
            false,
            PcapOptions::Disabled,
        )
        .unwrap()
    }

    fn default_server_config() -> (&'static str, CertificateDer<'static>, ServerConfig) {
        let server_name = "server-name";
        let cert = rcgen::generate_simple_self_signed(vec![server_name.into()]).unwrap();
        let key = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
        let server_cert = CertificateDer::from(cert.cert);
        let server_config =
            ServerConfig::with_single_cert(vec![server_cert.clone()], key.into()).unwrap();
        (server_name, server_cert, server_config)
    }

    fn default_client_config(server_cert: CertificateDer) -> ClientConfig {
        let mut roots = RootCertStore::empty();
        roots.add(server_cert).unwrap();

        let default_provider = rustls::crypto::ring::default_provider();
        let provider = rustls::crypto::CryptoProvider {
            cipher_suites: vec![rustls::crypto::ring::cipher_suite::TLS13_AES_128_GCM_SHA256],
            ..default_provider
        };

        let crypto = rustls::ClientConfig::builder_with_provider(provider.into())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();

        ClientConfig::new(Arc::new(QuicClientConfig::try_from(crypto).unwrap()))
    }

    #[tokio::test(start_paused = true)]
    async fn test_quic_handshake_and_bidi_stream_works() {
        let rt = async_rt::active_rt();

        // Network
        let network = default_network().call();
        let server_socket = network.node(SERVER_ADDR.as_ip_addr()).unwrap();
        let client_socket = network.node(CLIENT_ADDR.as_ip_addr()).unwrap();

        // QUIC config
        let (server_name, server_cert, server_config) = default_server_config();
        let client_config = default_client_config(server_cert);

        // QUIC endpoints
        let server_endpoint = Endpoint::new_with_abstract_socket(
            EndpointConfig::default(),
            Some(server_config),
            Box::new(
                network
                    .udp_socket_for_node(server_socket.clone(), HOST_PORT)
                    .unwrap(),
            ),
            rt.clone(),
        )
        .unwrap();
        let client_endpoint = Endpoint::new_with_abstract_socket(
            EndpointConfig::default(),
            None,
            Box::new(
                network
                    .udp_socket_for_node(client_socket.clone(), HOST_PORT)
                    .unwrap(),
            ),
            rt,
        )
        .unwrap();
        client_endpoint.set_default_client_config(client_config);

        // Run server in the background
        let server_handle = async_rt::spawn(async move {
            let conn = server_endpoint.accept().await.unwrap().await.unwrap();
            let (mut bi_tx, mut bi_rx) = conn.accept_bi().await.unwrap();

            let msg = bi_rx.read_to_end(usize::MAX).await.unwrap();
            assert_eq!(msg.as_slice(), b"hello");

            bi_tx.write_all(b"world").await.unwrap();
            bi_tx.finish().unwrap();
            bi_tx.stopped().await.unwrap();
        });

        // Make a request from the client
        let client_conn = client_endpoint
            .connect(server_socket.socket_addr(HOST_PORT), server_name)
            .unwrap()
            .await
            .unwrap();
        let (mut client_bi_tx, mut client_bi_rx) = client_conn.open_bi().await.unwrap();
        client_bi_tx.write_all(b"hello").await.unwrap();
        client_bi_tx.finish().unwrap();

        let msg = client_bi_rx.read_to_end(usize::MAX).await.unwrap();
        assert_eq!(msg.as_slice(), b"world");

        // The server should now be done
        server_handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn test_packet_arrives_at_expected_time() {
        // Sanity check
        let network = default_network().call();
        let server_node = network.node(SERVER_ADDR.as_ip_addr()).unwrap();
        let client_node = network.node(CLIENT_ADDR.as_ip_addr()).unwrap();
        network
            .assert_connectivity_between_nodes(server_node, client_node)
            .await
            .unwrap();

        // Test
        let network = default_network().call();
        let server_node = network.node(SERVER_ADDR.as_ip_addr()).unwrap();
        let client_node = network.node(CLIENT_ADDR.as_ip_addr()).unwrap();
        let data = network.in_transit_data(
            client_node.id().clone(),
            OwnedTransmit {
                source: client_node.socket_addr(HOST_PORT),
                destination: server_node.socket_addr(HOST_PORT),
                ecn: None,
                contents: b"hello world".to_vec().into(),
                segment_size: None,
                ttl: DEFAULT_TTL,
            },
        );
        let packet_id = data.id;
        network.forward(client_node.clone(), data);

        let mut recv_result = BufsAndMeta::new(1200, 10);
        let mut server_socket = network
            .udp_socket_for_node(server_node.clone(), HOST_PORT)
            .unwrap();
        let received = server_socket.receive_raw(&mut recv_result).await.unwrap();

        assert_eq!(received, 1);
        assert_eq!(recv_result.meta[0].len, 11);
        assert_eq!(&recv_result.bufs[0][..11], b"hello world");

        // This test proves that the packet travels a specific path and is delayed at each hop
        let expected_timings = [
            (Duration::from_millis(0), "client"),
            (Duration::from_millis(10), "router2"),
            (Duration::from_millis(20), "router1"),
            (Duration::from_millis(30), "server"),
        ];

        let stepper = network.tracer.stepper();
        let hops = stepper.get_packet_hops(packet_id);

        assert_eq!(hops.len(), expected_timings.len());
        for ((duration, node), (expected_duration, expected_node)) in
            hops.into_iter().zip(expected_timings)
        {
            assert_eq!(&*node, expected_node);
            assert_eq!(duration, expected_duration, "{node:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_packets_are_batched() {
        let bandwidth = BANDWIDTH_100_MBPS;
        let packet_size_bytes = 1200;

        // If we send four packets, they all take less than 2 ms to send, which means they should
        // be batched together
        let expected_packet_delay =
            Duration::from_secs_f64((packet_size_bytes * 8) as f64 / bandwidth as f64);
        assert!(4 * expected_packet_delay < Duration::from_millis(2));

        // Sanity check
        let network = default_network().bandwidth_bps(bandwidth).call();
        let server_node = network.node(SERVER_ADDR.as_ip_addr()).unwrap();
        let client_node = network.node(CLIENT_ADDR.as_ip_addr()).unwrap();
        network
            .assert_connectivity_between_nodes(client_node, server_node)
            .await
            .unwrap();

        // Actual test
        let network = default_network().bandwidth_bps(bandwidth).call();
        let server_node = network.node(SERVER_ADDR.as_ip_addr()).unwrap();
        let client_node = network.node(CLIENT_ADDR.as_ip_addr()).unwrap();

        let mut packet_ids = Vec::new();
        for _ in 0..4 {
            let data = network.in_transit_data(
                client_node.id().clone(),
                OwnedTransmit {
                    source: client_node.socket_addr(HOST_PORT),
                    destination: server_node.socket_addr(HOST_PORT),
                    ecn: None,
                    contents: vec![42; packet_size_bytes].into(),
                    segment_size: None,
                    ttl: DEFAULT_TTL,
                },
            );

            packet_ids.push(data.id);
            network.forward(client_node.clone(), data.clone());
        }

        let mut recv_result = BufsAndMeta::new(packet_size_bytes, 10);
        let mut server_socket = network
            .udp_socket_for_node(server_node.clone(), HOST_PORT)
            .unwrap();
        let mut received = 0;
        while received < 4 {
            received += server_socket.receive_raw(&mut recv_result).await.unwrap();
            assert!(received >= 1);
            assert_eq!(recv_result.meta[0].len, packet_size_bytes);
        }

        assert_eq!(received, 4);

        let stepper = network.tracer.stepper();

        let mut packet_times = Vec::new();
        for packet_id in packet_ids {
            let packet_sent = stepper.get_packet_sent_from(packet_id, &client_node.id);
            let packet_arrived = stepper.get_packet_arrived_at(packet_id, &server_node.id);
            packet_times.push((packet_sent.unwrap(), packet_arrived.unwrap()));
        }

        // The packets are batched, so they all arrive at the same time
        for w in packet_times.windows(2) {
            assert_eq!(w[0], w[1])
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_packet_is_buffered_when_link_down() {
        // Let one of the links be down for 10 seconds
        let network = default_network()
            .events(vec![
                NetworkEvent {
                    relative_time: Duration::from_secs(0),
                    kind: NetworkEventKind::Link(LinkEventPayload {
                        link_id: "router2-router1".into(),
                        status: Some(UpdateLinkStatus::Down),
                        bandwidth_bps: None,
                        delay: None,
                        extra_delay: None,
                        extra_delay_ratio: None,
                        packet_duplication_ratio: None,
                        packet_loss_ratio: None,
                        congestion_event_ratio: None,
                    }),
                },
                NetworkEvent {
                    relative_time: Duration::from_secs(10),
                    kind: NetworkEventKind::Link(LinkEventPayload {
                        link_id: "router2-router1".into(),
                        status: Some(UpdateLinkStatus::Up),
                        bandwidth_bps: None,
                        delay: None,
                        extra_delay: None,
                        extra_delay_ratio: None,
                        packet_duplication_ratio: None,
                        packet_loss_ratio: None,
                        congestion_event_ratio: None,
                    }),
                },
            ])
            .call();

        let server_node = network.node(SERVER_ADDR.as_ip_addr()).unwrap();
        let client_node = network.node(CLIENT_ADDR.as_ip_addr()).unwrap();

        let data = network.in_transit_data(
            client_node.id().clone(),
            OwnedTransmit {
                source: client_node.socket_addr(HOST_PORT),
                destination: server_node.socket_addr(HOST_PORT),
                ecn: None,
                contents: vec![42; 1200].into(),
                segment_size: None,
                ttl: DEFAULT_TTL,
            },
        );
        let packet_id = data.id;

        network.forward(client_node.clone(), data.clone());
        let mut recv_result = BufsAndMeta::new(1200, 10);
        let mut server_socket = network
            .udp_socket_for_node(server_node.clone(), HOST_PORT)
            .unwrap();
        let received = server_socket.receive_raw(&mut recv_result).await.unwrap();

        assert_eq!(received, 1);
        assert_eq!(recv_result.meta[0].len, 1200);

        let stepper = network.tracer.stepper();
        let hops = stepper.get_packet_hops(packet_id);
        assert_eq!(hops.len(), 4);

        // This test proves that the packet travels a specific path and is delayed at each hop
        let expected_timings = [
            (Duration::from_millis(0), "client"),
            (Duration::from_millis(10), "router2"),
            (Duration::from_millis(10_010), "router1"),
            (Duration::from_millis(10_020), "server"),
        ];

        assert_eq!(hops.len(), expected_timings.len());
        for ((duration, node), (expected_duration, expected_node)) in
            hops.into_iter().zip(expected_timings)
        {
            assert_eq!(&*node, expected_node);
            assert_eq!(duration, expected_duration, "{node:?}");
        }
    }
}
