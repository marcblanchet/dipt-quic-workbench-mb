use clap::{Parser, Subcommand};
use std::net::IpAddr;
use std::path::PathBuf;

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
    #[arg(long)]
    pub network_graph: PathBuf,

    /// Path to the JSON file containing the network events
    #[arg(long)]
    pub network_events: PathBuf,
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
    /// Path to the JSON file containing the traffic specification
    #[arg(long)]
    pub traffic: PathBuf,

    #[command(flatten)]
    pub rt: RtOpt,

    #[command(flatten)]
    pub network: NetworkOpt,
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
