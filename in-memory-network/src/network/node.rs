use crate::network::InMemoryNetwork;
use crate::network::event::UpdateNodeStatus;
use crate::network::inbound_queue::InboundQueue;
use crate::network::link::BufferedPacket;
use crate::network::outbound_buffer::OutboundBuffer;
use crate::network::spec::NetworkNodeSpec;
use crate::{HOST_PORT, async_rt};
use anyhow::bail;
use futures_util::FutureExt;
use futures_util::future::Shared;
use parking_lot::Mutex;
use std::mem;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

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
    pub(crate) addresses: Vec<IpAddr>,
    pub(crate) id: Arc<str>,
    pub(crate) udp_endpoint: Arc<UdpEndpoint>,
    pub(crate) injected_failures: NodeInjectedFailures,
    pub(crate) status: Mutex<NodeStatus>,
    pub(crate) last_buffer_clear: Mutex<Option<async_rt::time::Instant>>,
    outbound_buffer: Arc<OutboundBuffer>,
    outbound_tx: futures::channel::mpsc::UnboundedSender<BufferedPacket>,
}

impl Node {
    pub(crate) fn new(
        node: &NetworkNodeSpec,
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

        // The QUIC endpoint is bound to the node's address in the first network interface
        //
        // If there are nodes with multiple network interfaces, routing tables must be properly set
        // up so packets addressed to the QUIC endpoint can be transmitted over any of the links
        let quic_address = addresses[0];
        let quinn_endpoint = Arc::new(UdpEndpoint {
            addr: SocketAddr::new(quic_address, HOST_PORT),
            inbound: Arc::new(Mutex::new(InboundQueue::new())),
        });

        let (tx, rx) = futures::channel::mpsc::unbounded();
        let node = Self {
            injected_failures: NodeInjectedFailures::from_spec(node),
            id: node.id.clone(),
            addresses,
            outbound_buffer: Arc::new(OutboundBuffer::new(node.buffer_size_bytes as usize)),
            udp_endpoint: quinn_endpoint.clone(),
            outbound_tx: tx,
            status: Mutex::new(NodeStatus::Up),
            last_buffer_clear: Mutex::default(),
        };
        Ok((node, rx))
    }

    pub(crate) fn enqueue_outbound(&self, network: &Arc<InMemoryNetwork>, packet: BufferedPacket) {
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

    pub fn quic_addr(&self) -> SocketAddr {
        self.udp_endpoint.addr
    }

    pub fn id(&self) -> &Arc<str> {
        &self.id
    }

    pub fn addresses(&self) -> impl Iterator<Item = IpAddr> + use<> {
        self.addresses.clone().into_iter()
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
