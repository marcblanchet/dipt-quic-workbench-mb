use crate::config::NetworkConfig;
use crate::config::cli::SimulateOpt;
use crate::config::traffic::{QuicRequestResponseTraffic, TrafficJson, TrafficKind};
use crate::udp::ping;
use crate::util::{print_link_stats, print_max_buffer_usage_per_node, print_node_stats};
use crate::{load_network_config, load_traffic, util};
use crate::{quic, udp};
use anyhow::{Context, anyhow, bail};
use fastrand::Rng;
use futures::StreamExt;
use in_memory_network::async_rt;
use in_memory_network::async_rt::time::Instant;
use in_memory_network::network::event::NetworkEvents;
use in_memory_network::network::spec::NetworkSpec;
use in_memory_network::network::{InMemoryNetwork, PcapOptions};
use in_memory_network::tracing::tracer::SimulationStepTracer;
use parking_lot::Mutex;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::net::IpAddr;
use std::sync::Arc;
use std::{cmp, fs, io};

fn validate_traffic(opts: &QuicRequestResponseTraffic) -> anyhow::Result<()> {
    if opts.request_interval_ms != 0 && opts.concurrent_streams_per_connection != 1 {
        bail!(
            "incompatible QUIC traffic options used: `request_interval_ms` is only valid when `concurrent_streams_per_connection` is set to `1` (its default value)"
        );
    }

    Ok(())
}

pub async fn run_and_report_stats(cli_options: &SimulateOpt) -> anyhow::Result<()> {
    let network_config = load_network_config(&cli_options.network)?;
    let traffic = load_traffic(cli_options)?;

    // Necessary for reporting
    let mut node_ips_by_role = HashMap::<_, Vec<_>>::new();
    for traffic in &traffic.traffic_patterns {
        match traffic {
            TrafficKind::QuicRequestResponse(t) => {
                validate_traffic(t)?;
                node_ips_by_role
                    .entry("quic client")
                    .or_default()
                    .push(t.client.ip());
                node_ips_by_role
                    .entry("quic server")
                    .or_default()
                    .push(t.server.ip());
            }
            TrafficKind::UdpPing(t) => {
                node_ips_by_role
                    .entry("ping client")
                    .or_default()
                    .push(t.client.ip());
                node_ips_by_role
                    .entry("ping server")
                    .or_default()
                    .push(t.server.ip());
            }
            TrafficKind::UdpOneDirection(t) => {
                node_ips_by_role
                    .entry("udp sender")
                    .or_default()
                    .push(t.source.ip());
                node_ips_by_role
                    .entry("udp receiver")
                    .or_default()
                    .push(t.target.ip());
            }
        }
    }

    let first_traffic = traffic.traffic_patterns.first().ok_or(anyhow!(
        "the specified traffic json file does not contain any traffic patterns"
    ))?;
    let (main_traffic_name, force_verbose_stats) = match first_traffic {
        TrafficKind::QuicRequestResponse(_) => ("Requests", false),
        TrafficKind::UdpPing(_) => ("Ping", true),
        TrafficKind::UdpOneDirection(_) => ("UDP", true),
    };

    let all_traffic_is_udp = traffic
        .traffic_patterns
        .iter()
        .all(|t| matches!(t, TrafficKind::UdpOneDirection(_)));

    let mut simulation = Simulation::new();
    let result = simulation
        .run(cli_options, network_config, traffic, main_traffic_name)
        .await;

    let Some((tracer, network)) = simulation.tracer_and_network else {
        eprintln!("Error...");
        return result;
    };

    println!("--- Replay log ---");
    let replay_log_path = "replay-log.json";
    let json_steps = serde_json::to_vec_pretty(&tracer.stepper().steps()).unwrap();
    fs::write(replay_log_path, json_steps).context("failed to store replay log")?;
    println!("* Replay log available at {replay_log_path}");

    println!("--- Node stats ---");
    let verified_simulation = tracer
        .verifier()
        .context("failed to create simulation verifier")?
        .verify()
        .context("failed to verify simulation")?;

    let node_ids_by_role: HashMap<_, _> = node_ips_by_role
        .into_iter()
        .map(|(role, node_ips)| {
            (
                role,
                node_ips
                    .into_iter()
                    .map(|ip| network.node(ip).id().as_ref())
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let duplicate_ids = util::duplicates(node_ids_by_role.values().flatten().copied());
    if !duplicate_ids.is_empty() && !all_traffic_is_udp {
        let duplicates = duplicate_ids.join(", ");
        bail!(
            "it is currently not allowed to use the same node in different traffic specs, but the following nodes were reused: {duplicates}"
        )
    }

    print_node_stats(
        &network.get_node_ids(),
        &verified_simulation,
        &node_ids_by_role,
        cli_options.rt.verbose_node_stats || force_verbose_stats,
    );
    print_max_buffer_usage_per_node(&verified_simulation);
    print_link_stats(&verified_simulation, &network);

    const DISPLAY_MAX_ERRORS: usize = 10;
    if !verified_simulation.non_fatal_errors.is_empty() {
        print!("--- Internal errors");
        if verified_simulation.non_fatal_errors.len() > DISPLAY_MAX_ERRORS {
            print!(
                " (showing {DISPLAY_MAX_ERRORS} of {})",
                verified_simulation.non_fatal_errors.len()
            );
        }

        println!(" ---");
        println!(
            "(These errors might indicate a bug in the workbench, please report them to the project's maintainers.)"
        );
    }
    for error in verified_simulation
        .non_fatal_errors
        .into_iter()
        .take(DISPLAY_MAX_ERRORS)
    {
        println!("* {error}");
    }

    if result.is_err() {
        eprintln!("Error...");
    }

    result
}

#[derive(Default)]
pub struct Simulation {
    pub tracer_and_network: Option<(Arc<SimulationStepTracer>, Arc<InMemoryNetwork>)>,
}

impl Simulation {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn run(
        &mut self,
        cli_options: &SimulateOpt,
        network_config: NetworkConfig,
        traffic: TrafficJson,
        main_traffic_pattern: &str,
    ) -> anyhow::Result<()> {
        let traffic_patterns = traffic.traffic_patterns;

        println!("--- Params ---");
        let (quinn_rng_seed, simulated_network_rng_seed) = if cli_options.network.non_deterministic
        {
            let mut rng = Rng::new();
            (rng.u64(..), rng.u64(..))
        } else {
            (
                cli_options.network.quinn_rng_seed,
                cli_options.network.network_rng_seed,
            )
        };
        println!("* Quinn seed: {}", quinn_rng_seed);
        println!("* Network seed: {}", simulated_network_rng_seed);
        println!(
            "* Network graph path: {}",
            cli_options.network.network_graph.display()
        );
        println!(
            "* Network events path: {}",
            cli_options.network.network_events.display()
        );
        println!("* Traffic patterns path: {}", cli_options.traffic.display(),);

        let start = Instant::now();

        let quic_configs = network_config.network_graph.quic_configs();
        let mut quinn_rng = Rng::with_seed(quinn_rng_seed);

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
            cli_options.rt.disable_time_warping,
            PcapOptions::Disabled,
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
        if cli_options.rt.disable_time_warping {
            println!("* Connectivity check skipped to save time (time warping is disabled)");
        } else {
            let pairs = socket_pairs_from_traffic(&traffic_patterns);
            println!("* Running connectivity check for the following node pairs:");
            for &(ip1, ip2) in &pairs {
                let node1 = connectivity_check_network.node(ip1);
                let node2 = connectivity_check_network.node(ip2);

                print!("  * {} ({ip1}) <-> {} ({ip2}) ... ", node1.id(), node2.id());
                io::stdout().flush()?;

                let connectivity_check_result = connectivity_check_network
                    .assert_connectivity_between_nodes(node1, node2)
                    .await;

                match connectivity_check_result {
                    Ok((arrived1, arrived2)) => {
                        println!(
                            "passed (packets arrived after {} ms and {} ms)",
                            arrived1.as_millis(),
                            arrived2.as_millis()
                        );
                    }
                    Err(e) => {
                        println!("failed!");
                        return Err(e);
                    }
                }
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
            cli_options.rt.disable_time_warping,
            PcapOptions::WithTlsKeys,
        )?;
        self.tracer_and_network = Some((tracer.clone(), network.clone()));

        // Set up TLS certificate (used by QUIC servers)
        let key = PrivatePkcs8KeyDer::from(quic::server::KEY_PAIR_DER_RSA);
        let cert = CertificateDer::from(quic::server::CERT_DER_RSA);

        // Track handled QUIC connections
        let (handled_connections_tx, mut handled_connections_rx) =
            futures::channel::mpsc::unbounded();

        // Set up necessary background servers
        for traffic in &traffic_patterns {
            match traffic {
                TrafficKind::QuicRequestResponse(t) => {
                    let server_node = network.node(t.server.ip());
                    let server_socket = network
                        .udp_socket_for_node(server_node.clone(), t.server.port())
                        .unwrap();
                    let server = quic::server::server_endpoint(
                        start,
                        server_node.keylog().clone(),
                        cert.clone(),
                        key.clone_key().into(),
                        server_socket,
                        &quic_configs[server_node.id().as_ref()],
                        &mut quinn_rng,
                    )?;
                    quic::server::server_listen(
                        server,
                        t.response_size_bytes,
                        handled_connections_tx.clone(),
                    );
                }
                TrafficKind::UdpPing(t) => {
                    let server_node = network.node(t.server.ip());
                    let server_socket = network
                        .udp_socket_for_node(server_node.clone(), t.server.port())
                        .unwrap();
                    ping::run_server_forever(server_socket, t.client.ip());
                }
                TrafficKind::UdpOneDirection(_) => {
                    // No background server is used with unidirectional UDP traffic
                }
            }
        }

        println!("--- {main_traffic_pattern} ---");
        if traffic_patterns.len() > 1 {
            println!(
                "(We are simulating multiple traffic patterns and will only display logs for the first one.)"
            );
        }

        // Spawn client tasks
        let mut quic_client_tasks = Vec::new();
        let mut ping_client_tasks = Vec::new();
        let mut udp_client_tasks = Vec::new();
        for (i, traffic_kind) in traffic_patterns.iter().enumerate() {
            let log_writer: Arc<Mutex<dyn Write + Send + Sync>> = if i == 0 {
                Arc::new(Mutex::new(io::stdout()))
            } else {
                Arc::new(Mutex::new(io::empty()))
            };

            match traffic_kind {
                TrafficKind::QuicRequestResponse(t) => {
                    let client_node = network.node(t.client.ip());
                    let client_socket = network
                        .udp_socket_for_node(client_node.clone(), t.client.port())
                        .unwrap();
                    let client = quic::client::client_endpoint(
                        start,
                        client_node.keylog().clone(),
                        cert.clone(),
                        client_socket,
                        &quic_configs[client_node.id().as_ref()],
                        &mut quinn_rng,
                    )?;

                    let task = async_rt::spawn(quic::client::run_traffic_pattern(
                        t.clone(),
                        start,
                        client,
                        log_writer,
                    ));
                    quic_client_tasks.push(task);
                }
                TrafficKind::UdpPing(t) => {
                    let client_node = network.node(t.client.ip());
                    let client_socket = network
                        .udp_socket_for_node(client_node.clone(), t.client.port())
                        .unwrap();
                    let task = ping::run_traffic_pattern(client_socket, t, start);
                    ping_client_tasks.push(task);
                }
                TrafficKind::UdpOneDirection(t) => {
                    udp_client_tasks
                        .push(udp::one_direction::run_traffic_pattern(network.clone(), t));
                }
            }
        }

        // Wait for all ping tasks to finish
        for task in ping_client_tasks {
            task.await.context("ping client task crashed")?;
        } // Wait for all udp tasks to finish
        for task in udp_client_tasks {
            task.await.context("udp client task crashed")?;
        }

        // Wait for all quic traffic tasks to finish
        let mut total_quic_connections = 0;
        for task in quic_client_tasks {
            let connection_count = task
                .await
                .context("quic client task crashed")?
                .context("quic client task errored")?;

            total_quic_connections += connection_count;
        }

        // Ensure background servers are done handling QUIC connections
        let mut handled_quic_connections = 0;
        while handled_quic_connections < total_quic_connections {
            let Some(conn_task_handle) = handled_connections_rx.next().await else {
                break;
            };

            conn_task_handle
                .await
                .context("server connection task crashed")?
                .context("server connection task errored")?;

            handled_quic_connections += 1;
        }

        Ok(())
    }
}

fn socket_pairs_from_traffic(traffic: &[TrafficKind]) -> Vec<(IpAddr, IpAddr)> {
    let mut pairs = HashSet::new();
    for kind in traffic {
        match kind {
            TrafficKind::QuicRequestResponse(t) => {
                // Order to prevent duplicates in `pairs`
                let fst = cmp::min(t.client.ip(), t.server.ip());
                let snd = cmp::max(t.client.ip(), t.server.ip());
                pairs.insert((fst, snd));
            }
            TrafficKind::UdpPing(t) => {
                // Order to prevent duplicates in `pairs`
                let fst = cmp::min(t.client.ip(), t.server.ip());
                let snd = cmp::max(t.client.ip(), t.server.ip());
                pairs.insert((fst, snd));
            }
            TrafficKind::UdpOneDirection(t) => {
                // Order to prevent duplicates in `pairs`
                let fst = cmp::min(t.source.ip(), t.target.ip());
                let snd = cmp::max(t.source.ip(), t.target.ip());
                pairs.insert((fst, snd));
            }
        }
    }

    // Sort to ensure determinism
    let mut pairs = pairs.into_iter().collect::<Vec<_>>();
    pairs.sort();
    pairs
}
