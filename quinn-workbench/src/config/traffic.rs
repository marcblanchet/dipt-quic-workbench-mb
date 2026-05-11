use serde::Deserialize;
use std::net::IpAddr;

#[derive(Deserialize)]
pub struct TrafficJson {
    pub traffic_patterns: Vec<TrafficKind>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TrafficKind {
    QuicRequestResponse(QuicRequestResponseTraffic),
    UdpPing(PingTraffic),
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct QuicRequestResponseTraffic {
    /// The time at which traffic should start, in milliseconds (defaults to 0)
    #[serde(default)]
    pub start_at_ms: u64,
    /// The client's IP address
    pub client_ip: IpAddr,
    /// The server's IP address
    pub server_ip: IpAddr,
    /// The number of requests that should be made (defaults to 10)
    #[serde(default = "default_requests")]
    pub requests: u32,
    /// The number of concurrent connections used when making the requests (defaults to 1)
    #[serde(default = "default_concurrent_connections")]
    pub concurrent_connections: u32,
    /// The number of concurrent streams per connection used when making the requests (defaults to 1)
    #[serde(default = "default_concurrent_streams_per_connection")]
    pub concurrent_streams_per_connection: u32,
    /// The size of each response, in bytes (defaults to 1024)
    #[serde(default = "default_response_size")]
    pub response_size_bytes: usize,
    /// The number of milliseconds to wait between receiving a request's response and sending the
    /// next request (defaults to 0)
    ///
    /// Note 1: when multiple connections are used, this interval is applied per connection (e.g.,
    /// if two connections are active, two requests will be sent in parallel, then each connection
    /// will independently wait for the interval to elapse).
    ///
    /// Note 2: this option is only valid when `concurrent-streams-per-connection` is set to `1`
    #[serde(default)]
    pub request_interval_ms: u64,
}

fn default_requests() -> u32 {
    10
}
fn default_concurrent_connections() -> u32 {
    1
}
fn default_concurrent_streams_per_connection() -> u32 {
    1
}
fn default_response_size() -> usize {
    1024
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct PingTraffic {
    /// The time at which traffic should start, in milliseconds (defaults to 0)
    #[serde(default)]
    pub start_at_ms: u64,
    /// The ping source's IP address
    pub client_ip: IpAddr,
    /// The ping destination's IP address
    pub server_ip: IpAddr,
    /// The duration of the run, after which we will stop sending pings and the program will
    /// terminate
    pub duration_ms: u64,
    /// The interval at which ping packets will be sent
    pub interval_ms: u64,
    /// The deadline between sending a ping and receiving a reply (after which the ping itself or
    /// its reply are considered lost)
    #[serde(default = "default_deadline_ms")]
    pub deadline_ms: u64,
}

fn default_deadline_ms() -> u64 {
    10_000
}
