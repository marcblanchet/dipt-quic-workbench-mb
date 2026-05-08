use crate::config::NetworkConfig;
use crate::config::cli::{NetworkOpt, RtOpt};
use crate::config::traffic::{QuicRequestResponseTraffic, TrafficKind};
use crate::quic::{client, server};
use anyhow::Context;
use fastrand::Rng;
use futures::StreamExt;
use in_memory_network::async_rt;
use in_memory_network::async_rt::time::Instant;
use in_memory_network::network::InMemoryNetwork;
use in_memory_network::network::event::NetworkEvents;
use in_memory_network::network::spec::NetworkSpec;
use in_memory_network::pcap_exporter::{InMemoryKeyLog, PcapExporter};
use in_memory_network::tracing::tracer::SimulationStepTracer;
use parking_lot::Mutex;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::net::IpAddr;
use std::sync::Arc;
use std::{cmp, io};

#[derive(Default)]
pub struct QuicSimulation {
    pub tracer_and_network: Option<(Arc<SimulationStepTracer>, Arc<InMemoryNetwork>)>,
}

impl QuicSimulation {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn run(
        &mut self,
        cli_rt_options: &RtOpt,
        cli_network_options: &NetworkOpt,
        network_config: NetworkConfig,
        traffic: Vec<TrafficKind>,
    ) -> anyhow::Result<()> {
        println!("--- Params ---");
        let (quinn_rng_seed, simulated_network_rng_seed) = if cli_network_options.non_deterministic
        {
            let mut rng = Rng::new();
            (rng.u64(..), rng.u64(..))
        } else {
            (
                cli_network_options.quinn_rng_seed,
                cli_network_options.network_rng_seed,
            )
        };
        println!("* Quinn seed: {}", quinn_rng_seed);
        println!("* Network seed: {}", simulated_network_rng_seed);
        println!(
            "* Network graph path: {}",
            cli_network_options.network_graph.display()
        );
        println!(
            "* Network events path: {}",
            cli_network_options.network_events.display()
        );

        let start = Instant::now();

        let quic_configs = network_config.network_graph.quic_configs();

        // Network check
        let network_spec: NetworkSpec = network_config.network_graph.into();
        let network_events = NetworkEvents::new(
            network_config
                .network_events
                .clone()
                .into_iter()
                .map(|e| e.into())
                .collect(),
            &network_spec.nodes,
            &network_spec.links,
        );

        // Mute warnings during the connectivity check to avoid spamming the console
        let connectivity_check_tracer =
            SimulationStepTracer::new(network_spec.clone()).mute_warnings();
        let connectivity_check_network = InMemoryNetwork::initialize(
            network_spec.clone(),
            network_events.clone(),
            Arc::new(connectivity_check_tracer),
            Rng::with_seed(simulated_network_rng_seed),
            start,
            cli_rt_options.disable_time_warping,
        )?;

        println!("--- Network ---");
        println!("* Initial link statuses (derived from events):");
        for link_spec in &network_spec.links {
            let status = connectivity_check_network.get_link_status(&link_spec.id);
            println!("  * {}: {}", link_spec.id, status);
        }
        println!("* Initial node statuses (derived from events):");
        for node_spec in &network_spec.nodes {
            let status = connectivity_check_network.get_node_status(&node_spec.id);
            println!("  * {}: {}", node_spec.id, status);
        }
        if cli_rt_options.disable_time_warping {
            println!("* Connectivity check skipped to save time (time warping is disabled)");
        } else {
            let pairs = ip_pairs_from_traffic(&traffic);
            println!("* Running connectivity check for the following node pairs:");
            for &(ip1, ip2) in &pairs {
                print!("  * {ip1} <-> {ip2} ... ");
                io::stdout().flush()?;

                let node1 = connectivity_check_network.node(ip1);
                let node2 = connectivity_check_network.node(ip2);
                let (arrived1, arrived2) = connectivity_check_network
                    .assert_connectivity_between_nodes(node1, node2)
                    .await?;

                println!(
                    "passed (packets arrived after {} ms and {} ms)",
                    arrived1.as_millis(),
                    arrived2.as_millis()
                );
            }
        }
        drop(connectivity_check_network);

        let start = Instant::now();

        // Network
        let tracer = Arc::new(SimulationStepTracer::new(network_spec.clone()));
        let network = InMemoryNetwork::initialize(
            network_spec,
            network_events,
            tracer.clone(),
            Rng::with_seed(simulated_network_rng_seed),
            start,
            cli_rt_options.disable_time_warping,
        )?;
        self.tracer_and_network = Some((tracer.clone(), network.clone()));

        // Set up server certificate
        let key = PrivatePkcs8KeyDer::from(server::KEY_PAIR_DER_RSA);
        let cert = CertificateDer::from(server::CERT_DER_RSA);

        let traffic = request_response_traffic(&traffic);

        let mut quinn_rng = Rng::with_seed(quinn_rng_seed);

        // Let servers listen in the background
        let (handled_connections_tx, mut handled_connections_rx) =
            futures::channel::mpsc::unbounded();
        for t in &traffic {
            let server_node = network.node(t.server_ip);
            let server_keylog = Arc::new(InMemoryKeyLog::default());
            let server_pcap_exporter =
                PcapExporter::for_node(server_node.id(), Some(server_keylog.clone()))
                    .context("failed to create pcap exporter")?;
            let server = server::server_endpoint(
                start,
                server_keylog,
                cert.clone(),
                key.clone_key().into(),
                network
                    .udp_socket_for_node(server_pcap_exporter, server_node.clone())
                    .unwrap(),
                &quic_configs[server_node.id().as_ref()],
                File::create(format!("{}.qlog", server_node.id()))?,
                &mut quinn_rng,
            )?;
            server::server_listen(
                server.clone(),
                t.response_size,
                handled_connections_tx.clone(),
            );
        }

        println!("--- Requests ---");

        // Let clients send requests concurrently
        let mut traffic_tasks = Vec::new();
        for (i, t) in traffic.iter().enumerate() {
            // Create the client endpoint
            let client_node = network.node(t.client_ip);
            let client_keylog = Arc::new(InMemoryKeyLog::default());
            let client_pcap_exporter =
                PcapExporter::for_node(client_node.id(), Some(client_keylog.clone()))
                    .context("failed to create pcap exporter")?;
            let client = client::client_endpoint(
                start,
                client_keylog,
                cert.clone(),
                network
                    .udp_socket_for_node(client_pcap_exporter, client_node.clone())
                    .unwrap(),
                &quic_configs[client_node.id().as_ref()],
                File::create(format!("{}.qlog", client_node.id()))?,
                &mut quinn_rng,
            )?;

            if traffic.len() > 1 && i == 0 {
                println!(
                    "There are multiple clients (on separate nodes). We will only display request-response logs for {}.",
                    client_node.id()
                );
            }

            let log_writer: Arc<Mutex<dyn Write + Send + Sync>> = if i == 0 {
                Arc::new(Mutex::new(io::stdout()))
            } else {
                Arc::new(Mutex::new(io::empty()))
            };

            let task = async_rt::spawn(client::run_one_traffic(
                network.clone(),
                t.clone(),
                start,
                client,
                log_writer,
            ));
            traffic_tasks.push(task);
        }

        // Wait for all traffic runs to finish
        let mut total_connections = 0;
        for task in traffic_tasks {
            let connection_count = task
                .await
                .context("traffic task crashed")?
                .context("traffic task errored")?;

            total_connections += connection_count;
        }

        // Cleanly shut down the servers
        let mut handled_connections = 0;
        while let Some(conn_task_handle) = handled_connections_rx.next().await {
            conn_task_handle
                .await
                .context("server connection task crashed")?
                .context("server connection task errored")?;

            handled_connections += 1;
            if handled_connections >= total_connections {
                break;
            }
        }

        Ok(())
    }
}

fn ip_pairs_from_traffic(traffic: &[TrafficKind]) -> Vec<(IpAddr, IpAddr)> {
    let mut pairs = HashSet::new();
    for t in traffic {
        match t {
            TrafficKind::QuicRequestResponse(request_response) => {
                // Order IPs to prevent duplicates
                let fst = cmp::min(request_response.client_ip, request_response.server_ip);
                let snd = cmp::max(request_response.client_ip, request_response.server_ip);
                pairs.insert((fst, snd));
            }
        }
    }

    // Sort to ensure determinism
    let mut pairs = pairs.into_iter().collect::<Vec<_>>();
    pairs.sort();
    pairs
}

fn request_response_traffic(traffic: &Vec<TrafficKind>) -> Vec<QuicRequestResponseTraffic> {
    let mut filtered_traffic = Vec::new();
    for t in traffic {
        match t {
            TrafficKind::QuicRequestResponse(request_response) => {
                filtered_traffic.push(request_response.clone())
            }
        }
    }

    filtered_traffic
}
