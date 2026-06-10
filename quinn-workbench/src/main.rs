#![allow(clippy::too_many_arguments)]

mod config;
mod quic;
mod quinn_extensions;
mod simulation;
mod udp;
mod util;

use crate::config::NetworkConfig;
use crate::config::cli::{Command, DebugCommand, NetworkOpt, SimulateOpt};
use crate::config::network::NetworkEventsJson;
use crate::config::traffic::TrafficJson;
use crate::udp::throughput;
use anyhow::{Context, bail};
use clap::Parser;
use config::cli::CliOpt;
use config::traffic;
use in_memory_network::async_rt;
use in_memory_network::async_rt::DelayMode;
use in_memory_network::network::spec::NetworkSpec;
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
        Command::Simulate(quic_traffic_opt) => {
            let delay_mode = if quic_traffic_opt.rt.disable_time_warping {
                DelayMode::Wait
            } else {
                DelayMode::TimeWarp
            };
            let rt = async_rt::new_rt(delay_mode);
            rt.block_on(simulation::run_and_report_stats(quic_traffic_opt))
        }
        Command::Debug {
            command: DebugCommand::Throughput(throughput_opt),
        } => {
            let network_config = load_network_config(&throughput_opt.network)?;
            let network_spec = network_config.network_graph.into();
            let rt = async_rt::new_rt(DelayMode::TimeWarp);
            rt.block_on(throughput::run(
                throughput_opt,
                network_spec,
                network_config.network_events,
            ))
        }
        Command::Rt => {
            println!("tokio");
            Ok(())
        }
    }
}

fn load_traffic(cli: &SimulateOpt, network_spec: &NetworkSpec) -> anyhow::Result<TrafficJson> {
    if let Some(traffic) = cli.traffic_path() {
        return load_json(traffic);
    };

    // No traffic file specified, attempt to infer the pattern instead
    if network_spec.nodes.len() != 2 {
        bail!(
            "attempted to infer traffic pattern based on the network topology, but this can only succeed when the network consists of exactly two nodes! (We are attempting to infer the traffic pattern instead of loading it from disk, because no path was specified through the CLI, and no `traffic.json` file was present at the current working directory)"
        );
    }

    let ips: Vec<_> = network_spec
        .nodes
        .iter()
        .flat_map(|node| node.addresses().into_iter().next())
        .collect();
    if ips.len() != 2 {
        bail!(
            "attempted to infer traffic pattern based on the network topology, but at least one of the nodes has no IP address! (We are attempting to infer the traffic pattern instead of loading it from disk, because no path was specified through the CLI, and no `traffic.json` file was present at the current working directory)"
        );
    }

    Ok(traffic::default_request_response_traffic(ips[0], ips[1]))
}

fn load_network_config(cli: &NetworkOpt) -> anyhow::Result<NetworkConfig> {
    let network_graph = load_json(&cli.network_graph)?;

    let network_events: Option<NetworkEventsJson> = cli
        .network_events()
        .as_ref()
        .map(|p| load_json(p))
        .transpose()?;

    Ok(NetworkConfig {
        network_graph,
        network_events: network_events.map(|e| e.events).unwrap_or_default(),
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
