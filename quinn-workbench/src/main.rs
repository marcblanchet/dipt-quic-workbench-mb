mod config;
mod quic;
mod quinn_extensions;
mod udp;
mod util;

use crate::config::NetworkConfig;
use crate::config::cli::{Command, NetworkOpt, QuicTrafficOpt};
use crate::config::network::NetworkEventsJson;
use crate::config::traffic::{QuicRequestResponseTraffic, TrafficJson, TrafficKind};
use crate::udp::{ping, throughput};
use anyhow::Context;
use clap::Parser;
use config::cli::CliOpt;
use in_memory_network::async_rt;
use in_memory_network::async_rt::DelayMode;
use serde::de::DeserializeOwned;
use std::collections::HashSet;
use std::fs::File;
use std::path::Path;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::Subscriber;

fn main() -> anyhow::Result<()> {
    Subscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .with_ansi(false)
        .without_time()
        .init();

    let opt = CliOpt::parse();

    match &opt.command {
        Command::Quic(quic_opt) => {
            let delay_mode = if quic_opt.rt.disable_time_warping {
                DelayMode::Wait
            } else {
                DelayMode::TimeWarp
            };
            let rt = async_rt::new_rt(delay_mode);
            let traffic = vec![TrafficKind::QuicRequestResponse(
                QuicRequestResponseTraffic {
                    start_at_ms: 0,
                    client_ip: quic_opt.peers.client_ip_address,
                    server_ip: quic_opt.peers.server_ip_address,
                    requests: quic_opt.requests,
                    concurrent_connections: quic_opt.concurrent_connections,
                    concurrent_streams_per_connection: quic_opt.concurrent_streams_per_connection,
                    response_size: quic_opt.response_size,
                    request_interval_ms: quic_opt.request_interval_ms,
                },
            )];

            rt.block_on(quic::run_and_report_stats(
                &quic_opt.rt,
                &quic_opt.network,
                traffic,
            ))
        }
        Command::QuicTraffic(quic_traffic_opt) => {
            let delay_mode = if quic_traffic_opt.rt.disable_time_warping {
                DelayMode::Wait
            } else {
                DelayMode::TimeWarp
            };
            let rt = async_rt::new_rt(delay_mode);
            let traffic = load_traffic(quic_traffic_opt)?;
            rt.block_on(quic::run_and_report_stats(
                &quic_traffic_opt.rt,
                &quic_traffic_opt.network,
                traffic.traffic,
            ))
        }
        Command::Ping(ping_opt) => {
            let network_config = load_network_config(&ping_opt.network)?;
            let rt = async_rt::new_rt(DelayMode::TimeWarp);
            rt.block_on(ping::run(ping_opt, network_config))
        }
        Command::Throughput(throughput_opt) => {
            let network_config = load_network_config(&throughput_opt.network)?;
            let rt = async_rt::new_rt(DelayMode::TimeWarp);
            rt.block_on(throughput::run(throughput_opt, network_config))
        }
        Command::Rt => {
            println!("tokio");
            Ok(())
        }
    }
}

fn load_traffic(cli: &QuicTrafficOpt) -> anyhow::Result<TrafficJson> {
    load_json(&cli.traffic)
}

fn load_network_config(cli: &NetworkOpt) -> anyhow::Result<NetworkConfig> {
    let network_graph = load_json(&cli.network_graph)?;
    let network_events: NetworkEventsJson = load_json(&cli.network_events)?;

    Ok(NetworkConfig {
        network_graph,
        network_events: network_events.events,
    })
}

fn load_json<T: DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let file =
        File::open(path).with_context(|| format!("unable to open file at `{}`", path.display()))?;
    let deserializer = &mut serde_json::Deserializer::from_reader(file);
    let mut unused = HashSet::new();
    let parsed = serde_ignored::deserialize(deserializer, |path| {
        unused.insert(path.to_string());
    })
    .with_context(|| format!("error parsing JSON from `{}`", path.display()))?;

    let mut unused: Vec<_> = unused.into_iter().collect();
    unused.sort_unstable();

    if !unused.is_empty() {
        let fields = unused.join("\n- ");
        println!(
            "WARN: the JSON file at `{}` contains the following unknown fields:\n- {fields}",
            path.display()
        );
    }

    Ok(parsed)
}
