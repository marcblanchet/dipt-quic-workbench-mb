use crate::network::event::UpdateNodeStatus;
use crate::network::inbound_queue::InboundQueue;
use crate::network::link::BufferedPacket;
use crate::network::outbound_buffer::OutboundBuffer;
use crate::network::route::{IpRange, Route};
use crate::network::spec::NetworkNodeSpec;
use crate::network::{InMemoryNetwork, PcapOptions};
use crate::pcap_exporter::{InMemoryKeyLog, PcapExporter};
use crate::{InTransitData, async_rt};
use anyhow::bail;
use futures_util::FutureExt;
use futures_util::future::Shared;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::mem;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

pub enum NodeStatus {
    Up,
    Down {
        up_tx: futures::channel::oneshot::Sender<()>,
        up_rx: Shared<futures::channel::oneshot::Receiver<()>>,
    },
}

impl NodeStatus {
    fn new_down() -> Self {
        let (up_tx, up_rx) = futures::channel::oneshot::channel();
        NodeStatus::Down {
            up_tx,
            up_rx: up_rx.shared(),
        }
    }

    pub fn notifier_for_node_up(&self) -> Option<Shared<futures::channel::oneshot::Receiver<()>>> {
        match self {
            NodeStatus::Up => None,
            NodeStatus::Down { up_rx, .. } => Some(up_rx.clone()),
        }
    }
}

pub struct Node {
    pub(crate) id: Arc<str>,
    pub(crate) canonical_address: IpAddr,
    pub(crate) addresses: Vec<IpAddr>,
    pub(crate) routes: Vec<(IpAddr, Route)>,
    pub(crate) injected_failures: NodeInjectedFailures,
    pub(crate) status: Mutex<NodeStatus>,
    pub(crate) last_buffer_clear: Mutex<Option<async_rt::time::Instant>>,
    pub(crate) pcap_exporter: PcapExporter,
    keylog: Arc<InMemoryKeyLog>,
    udp_endpoints: Mutex<HashMap<u16, Arc<UdpEndpoint>>>,
    outbound_buffer: Arc<OutboundBuffer>,
    outbound_tx: futures::channel::mpsc::UnboundedSender<BufferedPacket>,
}

impl Node {
    pub(crate) fn new(
        node: &NetworkNodeSpec,
        links_addr_map: &HashMap<IpAddr, IpAddr>,
        pcap_options: PcapOptions,
    ) -> anyhow::Result<(
        Self,
        futures::channel::mpsc::UnboundedReceiver<BufferedPacket>,
    )> {
        if node.interfaces.is_empty() {
            bail!("Node {} has no interfaces", node.id);
        }
        if node.interfaces[0].addresses.is_empty() {
            bail!("Node {} has an interface without any address", node.id);
        }

        let addresses = node.addresses();
        let canonical_address = addresses[0];
        let mut connected_routes = Vec::new();
        let mut routes = Vec::new();
        for interface in &node.interfaces {
            // Connected routes
            for addr in &interface.addresses {
                // Note: some interfaces have a single incoming link, with no outgoing one
                if let Some(link_target) = links_addr_map.get(&IpAddr::V4(addr.address)) {
                    connected_routes.push((
                        IpAddr::V4(addr.address),
                        Route {
                            destination: IpRange::from_cidr(addr.clone()),
                            next: *link_target,
                            cost: 0,
                        },
                    ));
                }
            }

            // Other routes
            for r in &interface.routes {
                for addr in &interface.addresses {
                    routes.push((IpAddr::V4(addr.address), r.clone()))
                }
            }
        }
        routes.sort_by(|(dest1, r1), (dest2, r2)| r1.cost.cmp(&r2.cost).then(dest1.cmp(dest2)));

        let keylog = Arc::new(InMemoryKeyLog::default());
        let pcap_exporter = match pcap_options {
            PcapOptions::Disabled => PcapExporter::noop(),
            PcapOptions::WithTlsKeys => {
                PcapExporter::for_node(node.id.clone(), Some(keylog.clone()))?
            }
        };

        let (tx, rx) = futures::channel::mpsc::unbounded();
        let node = Self {
            injected_failures: NodeInjectedFailures::from_spec(node),
            id: node.id.clone(),
            canonical_address,
            addresses,
            routes: connected_routes.into_iter().chain(routes).collect(),
            outbound_buffer: Arc::new(OutboundBuffer::new(node.buffer_size_bytes as usize)),
            udp_endpoints: Default::default(),
            outbound_tx: tx,
            status: Mutex::new(NodeStatus::Up),
            last_buffer_clear: Default::default(),
            pcap_exporter,
            keylog,
        };
        Ok((node, rx))
    }

    pub(crate) fn deliver_packet(&self, data: InTransitData) {
        let port = data.transmit.destination.port();
        self.udp_endpoint(port)
            .inbound
            .clone()
            .lock()
            .send(data, Duration::default());
    }

    pub fn udp_endpoint(&self, port: u16) -> Arc<UdpEndpoint> {
        // Lazily create endpoints as needed
        self.udp_endpoints
            .lock()
            .entry(port)
            .or_insert_with(|| {
                Arc::new(UdpEndpoint {
                    inbound: Arc::new(Mutex::new(InboundQueue::new())),
                    addr: SocketAddr::new(self.canonical_address, port),
                })
            })
            .clone()
    }

    pub(crate) fn enqueue_outbound(&self, network: &InMemoryNetwork, packet: BufferedPacket) {
        // Try to enqueue the data on the node's outbound buffer for later sending
        let outbound_buffer = self.outbound_buffer();
        let data_len = packet.data.transmit.packet_size();

        if outbound_buffer.reserve(data_len) {
            // The buffer has capacity!
            self.outbound_tx.clone().unbounded_send(packet).unwrap();
        } else {
            // The buffer is full and the packet is being dropped
            network
                .tracer
                .track_dropped_because_buffer_full(self, &packet.data);
        }
    }

    pub fn id(&self) -> &Arc<str> {
        &self.id
    }

    pub fn keylog(&self) -> &Arc<InMemoryKeyLog> {
        &self.keylog
    }

    pub fn addresses(&self) -> impl Iterator<Item = IpAddr> + use<> {
        self.addresses.clone().into_iter()
    }

    pub fn socket_addr(&self, port: u16) -> SocketAddr {
        SocketAddr::new(self.canonical_address, port)
    }

    pub(crate) fn status_str(&self) -> &'static str {
        match &*self.status.lock() {
            NodeStatus::Up => "UP",
            NodeStatus::Down { .. } => "DOWN",
        }
    }

    pub fn outbound_buffer(&self) -> Arc<OutboundBuffer> {
        self.outbound_buffer.clone()
    }

    pub fn is_down(&self) -> bool {
        matches!(*self.status.lock(), NodeStatus::Down { .. })
    }

    pub fn update_status(&self, update: UpdateNodeStatus) {
        let mut status_locked = self.status.lock();
        let status = mem::replace(&mut *status_locked, NodeStatus::Up);
        match (status, update) {
            (status @ NodeStatus::Down { .. }, UpdateNodeStatus::Down)
            | (status @ NodeStatus::Up, UpdateNodeStatus::Up) => {
                // No update, restore original status
                *status_locked = status;
            }

            (NodeStatus::Up, UpdateNodeStatus::Down) => *status_locked = NodeStatus::new_down(),
            (NodeStatus::Down { up_tx, .. }, UpdateNodeStatus::Up) => {
                *status_locked = NodeStatus::Up;

                // Notify anyone waiting that the node is back up
                up_tx.send(()).ok();
            }
        }
    }

    pub fn clear_buffer(&self) {
        *self.last_buffer_clear.lock() = Some(async_rt::time::Instant::now());
    }
}

pub struct NodeInjectedFailures {
    pub(crate) packet_loss_ratio: f64,
    pub(crate) packet_duplication_ratio: f64,
}

impl NodeInjectedFailures {
    pub(crate) fn from_spec(spec: &NetworkNodeSpec) -> Self {
        Self {
            packet_loss_ratio: spec.packet_loss_ratio,
            packet_duplication_ratio: spec.packet_duplication_ratio,
        }
    }
}

#[derive(Clone)]
pub struct UdpEndpoint {
    pub inbound: Arc<Mutex<InboundQueue>>,
    pub addr: SocketAddr,
}
