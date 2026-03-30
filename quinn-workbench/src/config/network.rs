use crate::config::quinn::QuinnJsonConfig;
use in_memory_network::network::event::{
    LinkEventPayload, NetworkEvent, NetworkEventKind, NodeEventPayload, UpdateLinkStatus,
    UpdateNodeStatus,
};
use in_memory_network::network::ip::Ipv4Cidr;
use in_memory_network::network::route::IpRange;
use serde::Deserialize;
use serde_with::{DisplayFromStr, serde_as};
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

#[derive(Deserialize, Clone)]
pub struct NetworkSpecJson {
    #[serde(rename = "type")]
    _type: Option<String>,
    nodes: Vec<NetworkNodeJson>,
    links: Vec<NetworkLinkJson>,
}

impl NetworkSpecJson {
    pub fn quic_configs(&self) -> HashMap<String, QuinnJsonConfig> {
        let mut configs = HashMap::new();
        for node in &self.nodes {
            if let NetworkNodeKindJson::Host = &node.kind {
                configs.insert(node.id.clone(), node.quic.clone().unwrap_or_default());
            }
        }

        configs
    }
}

#[derive(Deserialize, Clone)]
struct NetworkNodeJson {
    id: String,
    buffer_size_bytes: u64,
    #[serde(rename = "type")]
    kind: NetworkNodeKindJson,
    quic: Option<QuinnJsonConfig>,
    interfaces: Vec<NetworkInterfaceJson>,
    /// The ratio of packets that will be duplicated upon arriving to the node (the value must be
    /// between 0 and 1)
    #[serde(default)]
    packet_duplication_ratio: f64,
    /// The ratio of packets that will be lost upon arriving to the node (the value must be between
    /// 0 and 1)
    #[serde(default)]
    packet_loss_ratio: f64,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
enum NetworkNodeKindJson {
    Router,
    Host,
}

#[derive(Deserialize, Clone)]
struct NetworkInterfaceJson {
    addresses: Vec<NetworkAddressJson>,
    routes: Vec<NetworkRouteJson>,
}

#[serde_as]
#[derive(Deserialize, Clone)]
struct NetworkAddressJson {
    #[serde_as(as = "DisplayFromStr")]
    address: Ipv4Cidr,
}

#[serde_as]
#[derive(Deserialize, Clone)]
struct NetworkRouteJson {
    #[serde_as(as = "DisplayFromStr")]
    destination: IpRange,
    next: IpAddr,
    cost: u64,
}

#[serde_as]
#[derive(Deserialize, Clone)]
struct NetworkLinkJson {
    id: String,
    #[serde_as(as = "DisplayFromStr")]
    source: IpAddr,
    #[serde_as(as = "DisplayFromStr")]
    target: IpAddr,
    /// The link's bandwidth, in bytes per second
    bandwidth_bps: u64,
    /// The delay of the link, in milliseconds
    delay_ms: u64,
    /// The extra delay of the link, which will be applied at random according to
    /// `extra_delay_ratio`
    #[serde(default)]
    extra_delay_ms: u64,
    /// The ratio of packets that will have an extra delay applied, to simulate packet reordering
    /// (the value must be between 0 and 1)
    #[serde(default)]
    extra_delay_ratio: f64,
    /// The ratio of packets that will be marked with a CE ECN codepoint (the value must be between 0 and 1)
    #[serde(default)]
    congestion_event_ratio: f64,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
enum NetworkLinkStatusJson {
    Up,
    Down,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
enum NetworkNodeStatusJson {
    Up,
    Down,
}

impl From<NetworkSpecJson> for in_memory_network::network::spec::NetworkSpec {
    fn from(json: NetworkSpecJson) -> Self {
        let nodes = json
            .nodes
            .into_iter()
            .map(|n| in_memory_network::network::spec::NetworkNodeSpec {
                id: n.id.into(),
                kind: match n.kind {
                    NetworkNodeKindJson::Router => {
                        in_memory_network::network::spec::NodeKind::Router
                    }
                    NetworkNodeKindJson::Host => in_memory_network::network::spec::NodeKind::Host,
                },
                buffer_size_bytes: n.buffer_size_bytes,
                interfaces: n
                    .interfaces
                    .into_iter()
                    .map(|i| in_memory_network::network::spec::NetworkInterface {
                        addresses: i.addresses.into_iter().map(|a| a.address).collect(),
                        routes: i
                            .routes
                            .into_iter()
                            .map(|r| in_memory_network::network::route::Route {
                                destination: r.destination,
                                next: r.next,
                                cost: r.cost,
                            })
                            .collect(),
                    })
                    .collect(),
                packet_loss_ratio: n.packet_loss_ratio,
                packet_duplication_ratio: n.packet_duplication_ratio,
            })
            .collect();

        let links = json.links.into_iter().map(|l| l.into()).collect();

        Self { nodes, links }
    }
}

impl From<NetworkLinkJson> for in_memory_network::network::spec::NetworkLinkSpec {
    fn from(l: NetworkLinkJson) -> Self {
        in_memory_network::network::spec::NetworkLinkSpec {
            id: l.id.into(),
            source: l.source,
            target: l.target,
            delay: Duration::from_millis(l.delay_ms),
            bandwidth_bps: l.bandwidth_bps,
            congestion_event_ratio: l.congestion_event_ratio,
            extra_delay: Duration::from_millis(l.extra_delay_ms),
            extra_delay_ratio: l.extra_delay_ratio,
        }
    }
}

#[derive(Deserialize)]
pub struct NetworkEventsJson {
    #[serde(rename = "type")]
    _type: Option<String>,
    pub events: Vec<NetworkEventJson>,
}

#[derive(Deserialize, Clone)]
pub struct RawNetworkEventJson {
    relative_time_ms: u64,
    link: Option<LinkEventPayloadJson>,
    node: Option<NodeEventPayloadJson>,
}

#[derive(Deserialize, Clone)]
#[serde(try_from = "RawNetworkEventJson")]
pub struct NetworkEventJson {
    relative_time_ms: u64,
    payload: EventPayloadJson,
}

#[derive(Deserialize, Clone)]
enum EventPayloadJson {
    Node(NodeEventPayloadJson),
    Link(LinkEventPayloadJson),
}

#[derive(Deserialize, Clone)]
struct NodeEventPayloadJson {
    id: String,
    #[serde(default)]
    status: Option<NetworkNodeStatusJson>,
    #[serde(default)]
    clear_buffer: bool,
}

#[derive(Deserialize, Clone)]
struct LinkEventPayloadJson {
    id: String,
    status: Option<NetworkLinkStatusJson>,
    bandwidth_bps: Option<u64>,
    delay_ms: Option<u64>,
    extra_delay_ms: Option<u64>,
    extra_delay_ratio: Option<f64>,
    packet_duplication_ratio: Option<f64>,
    packet_loss_ratio: Option<f64>,
    congestion_event_ratio: Option<f64>,
}

impl TryFrom<RawNetworkEventJson> for NetworkEventJson {
    type Error = String;

    fn try_from(raw: RawNetworkEventJson) -> Result<Self, Self::Error> {
        let payload = match (raw.link, raw.node) {
            (Some(link), None) => EventPayloadJson::Link(link),
            (None, Some(node)) => EventPayloadJson::Node(node),
            (Some(_), Some(_)) => return Err("only one of 'link' or 'node' must be present".into()),
            (None, None) => return Err("one of 'link' or 'node' must be present".into()),
        };

        Ok(NetworkEventJson {
            relative_time_ms: raw.relative_time_ms,
            payload,
        })
    }
}

impl From<NetworkEventJson> for NetworkEvent {
    fn from(json: NetworkEventJson) -> Self {
        let kind = match json.payload {
            EventPayloadJson::Node(n) => NetworkEventKind::Node(NodeEventPayload {
                node_id: n.id.into(),
                status: n.status.map(|s| match s {
                    NetworkNodeStatusJson::Up => UpdateNodeStatus::Up,
                    NetworkNodeStatusJson::Down => UpdateNodeStatus::Down,
                }),
                clear_buffer: n.clear_buffer,
            }),
            EventPayloadJson::Link(l) => NetworkEventKind::Link(LinkEventPayload {
                link_id: l.id.into(),
                status: l.status.map(|s| match s {
                    NetworkLinkStatusJson::Up => UpdateLinkStatus::Up,
                    NetworkLinkStatusJson::Down => UpdateLinkStatus::Down,
                }),
                bandwidth_bps: l.bandwidth_bps,
                delay: l.delay_ms.map(Duration::from_millis),
                extra_delay: l.extra_delay_ms.map(Duration::from_millis),
                extra_delay_ratio: l.extra_delay_ratio,
                packet_duplication_ratio: l.packet_duplication_ratio,
                packet_loss_ratio: l.packet_loss_ratio,
                congestion_event_ratio: l.congestion_event_ratio,
            }),
        };
        NetworkEvent {
            relative_time: Duration::from_millis(json.relative_time_ms),
            kind,
        }
    }
}
