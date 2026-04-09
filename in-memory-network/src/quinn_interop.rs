use crate::network::InMemoryNetwork;
use crate::network::inbound_queue::NextPacketDelivery;
use crate::network::node::{Node, UdpEndpoint};
use crate::transmit::{DEFAULT_TTL, OwnedTransmit};
use bytes::Bytes;
use parking_lot::Mutex;
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpSender};
use std::fmt::{Debug, Formatter};
use std::io;
use std::io::IoSliceMut;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

pub struct InMemoryUdpSocket {
    network: Arc<InMemoryNetwork>,
    endpoint: Arc<UdpEndpoint>,
    node: Arc<Node>,
    next_packet_delivery: Mutex<Option<Pin<Box<NextPacketDelivery>>>>,
}

impl Debug for InMemoryUdpSocket {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("InMemoryUdpSocket")
    }
}

impl InMemoryUdpSocket {
    pub fn node_id(&self) -> &str {
        self.node.id()
    }

    pub fn from_node(network: Arc<InMemoryNetwork>, node: Arc<Node>, port: u16) -> Self {
        InMemoryUdpSocket {
            endpoint: node.udp_endpoint(port),
            node,
            network: network.clone(),
            next_packet_delivery: Mutex::new(None),
        }
    }
}

impl AsyncUdpSocket for InMemoryUdpSocket {
    fn create_sender(&self) -> Pin<Box<dyn UdpSender>> {
        Box::pin(self.create_sender_concrete())
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let node = self.node.clone();
        let max_transmits = meta.len();
        assert!(meta.len() <= bufs.len());

        let mut lock = self.next_packet_delivery.lock();
        let delivery = lock.get_or_insert(Box::pin(NextPacketDelivery::new(
            self.endpoint.inbound.clone(),
            max_transmits,
        )));
        let delivered = ready!(delivery.as_mut().poll(cx));
        let delivered_len = delivered.len();

        let out = meta.iter_mut().zip(bufs);
        for (in_transit, (meta, buf)) in delivered.into_iter().zip(out) {
            self.network
                .tracer
                .track_read_by_application(node.id.clone(), &in_transit.data);

            let transmit = in_transit.data.transmit;

            // Meta
            meta.addr = transmit.source;
            meta.ecn = transmit.ecn;
            meta.dst_ip = Some(transmit.destination.ip());
            meta.len = transmit.contents.len();
            meta.stride = transmit.segment_size.unwrap_or(meta.len);

            // Buffer
            buf[..transmit.contents.len()].copy_from_slice(&transmit.contents);

            // Track in pcap
            self.node.pcap_exporter.track_transmit(&transmit);
        }

        Poll::Ready(Ok(delivered_len))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.endpoint.addr)
    }

    fn may_fragment(&self) -> bool {
        false
    }
}

impl InMemoryUdpSocket {
    pub fn create_sender_concrete(&self) -> InMemoryUdpSender {
        InMemoryUdpSender {
            network: self.network.clone(),
            socket_addr: self.endpoint.addr,
            node: self.node.clone(),
        }
    }

    pub fn send(&self, transmit: &Transmit) {
        send(
            self.node.clone(),
            &self.network,
            self.endpoint.addr,
            transmit,
        );
    }

    pub async fn receive<'a>(
        &mut self,
        bufs_and_meta: &'a mut BufsAndMeta,
    ) -> io::Result<Vec<UdpPacket<'a>>> {
        let packets = self.receive_raw(bufs_and_meta).await?;

        let mut result = Vec::with_capacity(packets);
        for i in 0..packets {
            let meta = &bufs_and_meta.meta[i];
            let source_addr = meta.addr;
            let payload = &bufs_and_meta.bufs[i][..meta.len];

            result.push(UdpPacket {
                source_addr,
                payload,
            });
        }

        Ok(result)
    }

    pub async fn receive_raw(&mut self, bufs_and_meta: &mut BufsAndMeta) -> io::Result<usize> {
        let receive = UdpReceive {
            socket: self,
            result: bufs_and_meta,
        };

        receive.await
    }
}

fn send(node: Arc<Node>, network: &Arc<InMemoryNetwork>, source: SocketAddr, transmit: &Transmit) {
    // We don't have code to handle GSO, so let's ensure transmits are always a single UDP
    // packet
    assert!(transmit.segment_size.is_none());

    let transmit = OwnedTransmit {
        source,
        destination: transmit.destination,
        ecn: transmit.ecn,
        contents: Bytes::copy_from_slice(transmit.contents),
        segment_size: transmit.segment_size,
        ttl: DEFAULT_TTL,
    };

    // Track in pcap
    node.pcap_exporter.track_transmit(&transmit);

    let data = network.in_transit_data(node.id.clone(), transmit);

    network.forward(node, data);
}

pub struct InMemoryUdpSender {
    network: Arc<InMemoryNetwork>,
    socket_addr: SocketAddr,
    node: Arc<Node>,
}

impl Debug for InMemoryUdpSender {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("InMemoryUdpSender")
    }
}

impl UdpSender for InMemoryUdpSender {
    fn poll_send(
        self: Pin<&mut Self>,
        transmit: &Transmit<'_>,
        _cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        self.send(transmit);
        Poll::Ready(Ok(()))
    }
}

impl InMemoryUdpSender {
    pub fn send(&self, transmit: &Transmit) {
        send(self.node.clone(), &self.network, self.socket_addr, transmit);
    }
}

pub struct UdpPacket<'a> {
    pub source_addr: SocketAddr,
    pub payload: &'a [u8],
}

pub struct UdpReceive<'a, 'b> {
    socket: &'a mut dyn AsyncUdpSocket,
    result: &'b mut BufsAndMeta,
}

pub struct BufsAndMeta {
    pub bufs: Vec<Vec<u8>>,
    pub meta: Vec<RecvMeta>,
}

impl BufsAndMeta {
    pub fn new(max_packet_size: usize, max_packets_per_read: usize) -> Self {
        Self {
            bufs: vec![vec![0u8; max_packet_size]; max_packets_per_read],
            meta: vec![RecvMeta::default(); max_packets_per_read],
        }
    }
}

impl Future for UdpReceive<'_, '_> {
    type Output = io::Result<usize>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;

        let socket = &mut this.socket;
        let bufs = &mut this.result.bufs;
        let meta = &mut this.result.meta;

        let mut bufs: Vec<_> = bufs.iter_mut().map(|b| IoSliceMut::new(b)).collect();
        socket.poll_recv(cx, &mut bufs, meta)
    }
}
