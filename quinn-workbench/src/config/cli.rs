use clap::{Parser, Subcommand};
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug, Clone)]
pub struct CliOpt {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Run a simulation with traffic patterns from a JSON configuration file
    Simulate(SimulateOpt),
    /// Commands for debugging the workbench
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
    },
    /// Return the identifier of the async runtime used
    Rt,
}

#[derive(Subcommand, Debug, Clone)]
pub enum DebugCommand {
    /// Run a throughput simulation at the UDP level
    Throughput(ThroughputOpt),
}

#[derive(Parser, Debug, Clone)]
pub struct PeerOpt {
    /// The IP address of the node used as a client
    #[arg(long)]
    pub client_ip_address: IpAddr,

    /// The IP address of the node used as a server
    #[arg(long)]
    pub server_ip_address: IpAddr,
}

#[derive(Parser, Debug, Clone)]
pub struct NetworkOpt {
    /// Whether the run should be non-deterministic, i.e. using a non-constant seed for the random
    /// number generators
    #[arg(long)]
    pub non_deterministic: bool,

    /// Quinn's random seed, which you can control to generate deterministic results (Quinn uses
    /// randomness internally)
    #[arg(long, default_value_t = 0)]
    pub quinn_rng_seed: u64,

    /// The random seed used for the simulated network (governing packet loss, duplication and
    /// reordering)
    #[arg(long, default_value_t = 42)]
    pub network_rng_seed: u64,

    /// Path to the JSON file containing the network graph
    #[arg(long, default_value = "topology.json")]
    pub network_graph: PathBuf,

    /// Path to the JSON file containing the network events (defaults to `events.json` if there is a
    /// file at that path, otherwise the simulation runs with no network events)
    #[arg(long)]
    network_events: Option<PathBuf>,

    /// Run the simulation without loading any network events file
    #[arg(long, conflicts_with = "network_events")]
    no_network_events: bool,
}

static DEFAULT_NETWORK_EVENTS_PATH: &str = "events.json";

impl NetworkOpt {
    pub fn network_events(&self) -> Option<&Path> {
        if self.no_network_events {
            None
        } else if let Some(network_events_path) = self.network_events.as_ref() {
            Some(network_events_path)
        } else if fs::exists(DEFAULT_NETWORK_EVENTS_PATH).is_ok_and(|exists| exists) {
            Some(Path::new(DEFAULT_NETWORK_EVENTS_PATH))
        } else {
            None
        }
    }
}

#[derive(Parser, Debug, Clone)]
pub struct RtOpt {
    /// Disable time-warping (making the simulation use real-world delays)
    #[arg(long, default_value_t = false)]
    pub disable_time_warping: bool,

    /// Show stats for each node, not only for the client and server nodes
    #[clap(long)]
    pub verbose_node_stats: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct SimulateOpt {
    /// Path to the JSON file containing the traffic specification (defaults to `traffic.json`,
    /// unless the file is absent _and_ the network graph consists of merely two nodes, in which
    /// case a traffic configuration with a single `quic_request_response` between the nodes is
    /// used).
    #[arg(long)]
    traffic: Option<PathBuf>,

    #[command(flatten)]
    pub rt: RtOpt,

    #[command(flatten)]
    pub network: NetworkOpt,
}

const DEFAULT_TRAFFIC_PATH: &str = "traffic.json";

impl SimulateOpt {
    pub fn traffic_path(&self) -> Option<&Path> {
        if let Some(path) = self.traffic.as_ref() {
            Some(path)
        } else if fs::exists(DEFAULT_TRAFFIC_PATH).is_ok_and(|exists| exists) {
            Some(Path::new(DEFAULT_TRAFFIC_PATH))
        } else {
            None
        }
    }
}

#[derive(Parser, Debug, Clone)]
pub struct ThroughputOpt {
    /// The duration of the run
    #[arg(long)]
    pub duration_ms: u64,

    /// The bitrate at which information should be sent
    ///
    /// If not provided, we find the link with the highest capacity and use its doubled bandwidth
    #[arg(long)]
    pub send_bps: Option<u64>,

    #[command(flatten)]
    pub peers: PeerOpt,

    #[command(flatten)]
    pub network: NetworkOpt,
}
