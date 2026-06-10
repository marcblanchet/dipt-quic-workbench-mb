use serde::de::Error;
use serde::{Deserialize, Deserializer};
use serde_json::json;
use std::net::{IpAddr, SocketAddr};

#[derive(Deserialize)]
pub struct TrafficJson {
    pub traffic_patterns: Vec<TrafficKind>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TrafficKind {
    QuicRequestResponse(QuicRequestResponseTraffic),
    UdpPing(PingTraffic),
    UdpOneDirection(UdpOneDirectionTraffic),
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct QuicRequestResponseTraffic {
    /// The time at which traffic should start, in milliseconds (defaults to 0)
    #[serde(default)]
    pub start_at_ms: u64,
    /// The client's socket address
    #[serde(deserialize_with = "deserialize_socket_addr")]
    pub client: SocketAddr,
    /// The server's socket address
    #[serde(deserialize_with = "deserialize_socket_addr")]
    pub server: SocketAddr,
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
    /// The ping source's socket address
    #[serde(deserialize_with = "deserialize_socket_addr")]
    pub client: SocketAddr,
    /// The ping destination's socket address
    #[serde(deserialize_with = "deserialize_socket_addr")]
    pub server: SocketAddr,
    /// The duration of the run, after which we will stop sending pings
    pub duration_ms: u64,
    /// The interval at which ping packets will be sent
    pub send_interval_ms: u64,
    /// The deadline between sending a ping and receiving a reply (after which the ping itself or
    /// its reply are considered lost)
    #[serde(default = "default_deadline_ms")]
    pub deadline_ms: u64,
}

fn default_deadline_ms() -> u64 {
    10_000
}

#[derive(Deserialize)]
pub struct UdpOneDirectionTraffic {
    /// The time at which traffic should start, in milliseconds (defaults to 0)
    #[serde(default)]
    pub start_at_ms: u64,
    /// The socket address of the sender (not technically a client, but we use the name for consistency with other traffic patterns)
    #[serde(deserialize_with = "deserialize_socket_addr")]
    pub client: SocketAddr,
    /// The socket address of the receiver (not technically a server, but we use the name for consistency with other traffic patterns)
    #[serde(deserialize_with = "deserialize_socket_addr")]
    pub server: SocketAddr,
    /// The size of the payload, which will potentially be split across multiple UDP packets (defaults to 10 KiB)
    #[serde(default = "default_udp_payload_bytes")]
    pub payload_bytes: u64,
    /// The interval at which the payload should be sent (defaults to 10 seconds)
    #[serde(default = "default_udp_send_interval_ms")]
    pub send_interval_ms: u64,
    /// The duration of the run, after which we will stop sending packets (defaults to 10 minutes)
    #[serde(default = "default_udp_duration_ms")]
    pub duration_ms: u64,
}

fn default_udp_payload_bytes() -> u64 {
    1024 * 10
}
fn default_udp_send_interval_ms() -> u64 {
    10000
}
fn default_udp_duration_ms() -> u64 {
    1000 * 60 * 10
}

const DEFAULT_PORT: u16 = 8080;

fn deserialize_socket_addr<'de, D: Deserializer<'de>>(d: D) -> Result<SocketAddr, D::Error> {
    let s = String::deserialize(d).map_err(|e| {
        D::Error::custom(format!(
            "expected socket address or IP address, encoded as a string. Inner error: {e}"
        ))
    })?;
    match (s.parse::<SocketAddr>(), s.parse::<IpAddr>()) {
        (Ok(socket_addr), _) => Ok(socket_addr),
        (_, Ok(ip_addr)) => Ok(SocketAddr::new(ip_addr, DEFAULT_PORT)),
        _ => Err(D::Error::custom(format!(
            "expected socket address or IP address, found string: `{s}`"
        ))),
    }
}

// It would be nice to avoid parsing from JSON here, but since the defaults are specified through
// serde we _have_ to parse (unless we want to build the struct by hand and manually specify the
// default values)
pub fn default_request_response_traffic(source_ip: IpAddr, target_ip: IpAddr) -> TrafficJson {
    let json = json!({
      "traffic_patterns": [
        {
          "type": "quic_request_response",
          "client": source_ip.to_string(),
          "server": target_ip.to_string(),
        }
      ]
    });

    serde_json::from_value(json).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_request_response_traffic() {
        let client = "127.0.0.1".parse().unwrap();
        let server = "127.0.0.2".parse().unwrap();
        let traffic = default_request_response_traffic(client, server);
        assert_eq!(traffic.traffic_patterns.len(), 1);
        let TrafficKind::QuicRequestResponse(traffic) = &traffic.traffic_patterns[0] else {
            unreachable!()
        };

        assert_eq!(traffic.client.ip(), client);
        assert_eq!(traffic.client.port(), 8080);
        assert_eq!(traffic.server.ip(), server);
        assert_eq!(traffic.server.port(), 8080);
    }

    #[test]
    fn test_deserialize_addr_wrong_type() {
        let udp = r#"{"start_at_ms": 1000, "client": "127.0.0.1", "server": 42 }"#;
        let err = serde_json::from_str::<UdpOneDirectionTraffic>(&udp)
            .err()
            .unwrap();
        assert!(
            err.to_string()
                .contains("expected socket address or IP address")
        );
    }

    #[test]
    fn test_deserialize_ip_only() {
        let udp = r#"{"start_at_ms": 1000, "client": "127.0.0.1", "server": "127.0.0.2" }"#;
        let udp: UdpOneDirectionTraffic = serde_json::from_str(&udp).unwrap();
        assert_eq!(udp.start_at_ms, 1000);
        assert_eq!(udp.client.ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(udp.client.port(), DEFAULT_PORT);
        assert_eq!(udp.server.ip(), "127.0.0.2".parse::<IpAddr>().unwrap());
        assert_eq!(udp.server.port(), DEFAULT_PORT);
    }

    #[test]
    fn test_deserialize_full_addr() {
        let udp =
            r#"{"start_at_ms": 1000, "client": "127.0.0.1:1234", "server": "127.0.0.2:1234" }"#;
        let udp: UdpOneDirectionTraffic = serde_json::from_str(&udp).unwrap();
        assert_eq!(udp.start_at_ms, 1000);
        assert_eq!(udp.client.ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(udp.client.port(), 1234);
        assert_eq!(udp.server.ip(), "127.0.0.2".parse::<IpAddr>().unwrap());
        assert_eq!(udp.server.port(), 1234);
    }
}
