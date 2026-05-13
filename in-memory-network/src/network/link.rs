use crate::InTransitData;
use crate::async_rt;
use crate::async_rt::time::Instant;
use crate::network::PacketAnomalies;
use crate::network::event::UpdateLinkStatus;
use crate::network::inbound_queue::{InboundQueue, NextPacketDelivery};
use crate::network::node::Node;
use crate::network::spec::NetworkLinkSpec;
use crate::tracing::tracer::SimulationStepTracer;
use async_lock::Semaphore;
use futures_util::future::Shared;
use futures_util::{FutureExt, select_biased};
use parking_lot::Mutex;
use quinn::udp::EcnCodepoint;
use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use std::{cmp, mem};

pub struct NetworkLink {
    pub id: Arc<str>,
    pub target: IpAddr,
    tracer: Arc<SimulationStepTracer>,
    // Packets currently in flight from the source to the destination
    in_transit: Arc<Mutex<InboundQueue>>,
    // Packets waiting to be sent (i.e., source node up + link up + enough bandwidth)
    pub(crate) outgoing_queue: futures::channel::mpsc::UnboundedSender<OutgoingPacket>,
    pacer: Mutex<PacketPacer>,
    sleep_until_ready_to_send_semaphore: Arc<Semaphore>,
    status: LinkStatus,
    last_down: Option<Instant>,
    delay: Duration,
    pub(crate) bandwidth_bps: usize,
    pub(crate) congestion_event_ratio: f64,
    pub(crate) extra_delay: Duration,
    pub(crate) extra_delay_ratio: f64,
}

pub(crate) enum LinkStatus {
    Up,
    Down {
        up_tx: futures::channel::oneshot::Sender<()>,
        up_rx: Shared<futures::channel::oneshot::Receiver<()>>,
    },
}

impl LinkStatus {
    pub(crate) fn new_down() -> Self {
        let (up_tx, up_rx) = futures::channel::oneshot::channel();
        LinkStatus::Down {
            up_tx,
            up_rx: up_rx.shared(),
        }
    }

    fn is_down(&self) -> bool {
        match self {
            LinkStatus::Up => false,
            LinkStatus::Down { .. } => true,
        }
    }

    fn notifier_for_link_up(&self) -> Option<Shared<futures::channel::oneshot::Receiver<()>>> {
        match self {
            LinkStatus::Up => None,
            LinkStatus::Down { up_rx, .. } => Some(up_rx.clone()),
        }
    }
}

impl NetworkLink {
    pub(crate) fn new(
        l: NetworkLinkSpec,
        tracer: Arc<SimulationStepTracer>,
    ) -> (
        Self,
        futures::channel::mpsc::UnboundedReceiver<OutgoingPacket>,
    ) {
        let (queue_tx, queue_rx) = futures::channel::mpsc::unbounded();
        let self_ = Self {
            id: l.id,
            status: LinkStatus::Up,
            last_down: None,
            tracer,
            target: l.target,
            in_transit: Arc::new(Mutex::new(InboundQueue::new())),
            outgoing_queue: queue_tx,
            pacer: Mutex::new(PacketPacer::new(Instant::now(), l.bandwidth_bps)),
            sleep_until_ready_to_send_semaphore: Arc::new(Semaphore::new(1)),
            delay: l.delay,
            bandwidth_bps: l.bandwidth_bps as usize,
            congestion_event_ratio: l.congestion_event_ratio,
            extra_delay: l.extra_delay,
            extra_delay_ratio: l.extra_delay_ratio,
        };

        (self_, queue_rx)
    }

    pub(crate) fn was_down_after(&self, instant: Instant) -> bool {
        matches!(self.last_down, Some(down) if down > instant)
    }

    pub(crate) fn status_str(&self) -> &'static str {
        match self.status {
            LinkStatus::Up => "UP",
            LinkStatus::Down { .. } => "DOWN",
        }
    }

    pub fn is_down(&self) -> bool {
        self.status.is_down()
    }

    pub fn current_delay(&self) -> Duration {
        self.delay
    }

    pub(crate) fn update_delay(&mut self, update: Duration) {
        if let LinkStatus::Up = self.status {
            return;
        }

        self.delay = update;
    }

    pub(crate) fn update_status(&mut self, update: UpdateLinkStatus) {
        let status = mem::replace(&mut self.status, LinkStatus::Up);
        match (status, update) {
            (status @ LinkStatus::Down { .. }, UpdateLinkStatus::Down)
            | (status @ LinkStatus::Up, UpdateLinkStatus::Up) => {
                // No update, restore original status
                self.status = status;
            }

            (LinkStatus::Up, UpdateLinkStatus::Down) => {
                // Set status to down
                self.status = LinkStatus::new_down();
                self.last_down = Some(Instant::now());

                // Nothing else to do here, because:
                // 1. already sent packets will be dropped by the forwarding code if they are still in flight
                // 2. packets in the node's outbound buffer will stay there until the link is back up
                // 3. attempting to send new packets will cause them to land in the buffer (if there's space)
            }

            (LinkStatus::Down { up_tx, .. }, UpdateLinkStatus::Up) => {
                // Set status to up and notify anyone waiting that the link is back up
                self.status = LinkStatus::Up;
                up_tx.send(()).ok();
            }
        }
    }

    pub(crate) fn send(
        &mut self,
        src_node: &Node,
        mut data: InTransitData,
        anomalies: PacketAnomalies,
    ) {
        // Sanity checks
        assert!(
            self.pacer
                .lock()
                .can_send(Instant::now(), data.transmit.packet_size())
        );
        assert!(matches!(self.status, LinkStatus::Up));

        // Apply "congestion experience" anomaly if requested
        if anomalies.congestion_experienced {
            // Sanity check: the Quinn-provided transmit must indicate support for ECN
            assert!(
                data.transmit
                    .ecn
                    .is_some_and(|codepoint| codepoint as u8 == 0b10 || codepoint as u8 == 0b01)
            );

            data.transmit.ecn = Some(EcnCodepoint::from_bits(0b11).unwrap())
        }

        // Record
        self.tracer.track_packet_in_transit(src_node, self, &data);

        // Send
        self.pacer
            .lock()
            .track_send(Instant::now(), data.transmit.packet_size());
        src_node
            .outbound_buffer()
            .release(data.transmit.packet_size());
        self.in_transit
            .lock()
            .send(data, self.delay + anomalies.extra_delay);
    }

    pub(crate) async fn sleep_until_ready_to_send(
        src_node: Arc<Node>,
        this: Arc<Mutex<Self>>,
        packet_size_bytes: usize,
        packet_sent_rx: &mut tokio::sync::watch::Receiver<bool>,
    ) {
        assert!(
            !this.lock().has_bandwidth_available(packet_size_bytes),
            "we should only wait when no bandwidth is available"
        );

        // Ensure this method is never executed concurrently, to prevent two callers from waiting
        // at the same time and thinking they are both allowed to send at the end
        let semaphore = this.lock().sleep_until_ready_to_send_semaphore.clone();
        let _permit = semaphore.acquire().await;

        let duration_until_enough_bandwidth = this
            .lock()
            .pacer
            .lock()
            .duration_until_can_send(Instant::now(), packet_size_bytes);

        // Sleep until enough bandwidth or until the packet gets sent, whichever comes first
        select_biased! {
            _ = packet_sent_rx.changed().fuse() => return,
            _ = async_rt::time::sleep(duration_until_enough_bandwidth).fuse() => {}
        }

        loop {
            // Sleep until the link is up, if necessary
            let notifier_for_link_up = this.lock().status.notifier_for_link_up();
            if let Some(notifier_for_link_up) = notifier_for_link_up {
                select_biased! {
                    // Break the sleep if someone else sends the packet in the meantime
                    _ = packet_sent_rx.changed().fuse() => return,
                    _ = notifier_for_link_up.fuse() => {}
                }
            }

            // Sleep until the node is up, if necessary
            // Assumption: a link is always sending from the same node, so we are not blocking other
            // nodes here
            let notifier_for_node_up = src_node.status.lock().notifier_for_node_up();
            if let Some(notifier_for_node_up) = notifier_for_node_up {
                select_biased! {
                    // Break the sleep if someone else sends the packet in the meantime
                    _ = packet_sent_rx.changed().fuse() => return,
                    _ = notifier_for_node_up.fuse() => {}
                }
            }

            // Important: the link could have gone down again while we waited on the node to come up
            if let LinkStatus::Up = this.lock().status {
                // Link is still up, ready to send!
                return;
            }
        }
    }

    pub(crate) fn has_bandwidth_available(&mut self, packet_size_bytes: usize) -> bool {
        // concurrency: note the line below acquires a permit, but drops it right away
        let packets_are_waiting_for_bandwidth = self
            .sleep_until_ready_to_send_semaphore
            .try_acquire()
            .is_none();

        if packets_are_waiting_for_bandwidth || self.status.is_down() {
            return false;
        }

        self.pacer
            .lock()
            .can_send(Instant::now(), packet_size_bytes)
    }

    pub(crate) fn next_delivered_packets(&mut self, max_transmits: usize) -> NextPacketDelivery {
        NextPacketDelivery::new(self.in_transit.clone(), max_transmits)
    }
}

const BATCH_MAX_DELAY: Duration = Duration::from_millis(2);

// Ensures that only a single packet at a time is being sent
struct PacketPacer {
    bandwidth_bps: f64,
    batch: PacketBatch,
}

#[derive(Clone)]
struct PacketBatch {
    created: Instant,
    delay: Duration,
}

impl PacketBatch {
    fn update_and_check_has_room(&mut self, now: Instant, packet_delay: Duration) -> bool {
        // Reset the batch if it got sent already
        if now - self.created >= self.delay {
            self.created = now;
            self.delay = Duration::default();
        }

        if self.delay.is_zero() {
            // The batch is empty, which means it has room for at least one packet, _regardless_ of
            // the packet's delay. This ensures we can send packets over slow connections, where the
            // packet's delay would already exceed a `BATCH_INTERVAL`
            return true;
        }

        self.delay + packet_delay < BATCH_MAX_DELAY
    }

    fn sent_at(&self) -> Instant {
        self.created + self.delay
    }
}

impl PacketPacer {
    fn new(now: Instant, bandwidth_bps: u64) -> Self {
        Self {
            bandwidth_bps: bandwidth_bps as f64,
            batch: PacketBatch {
                created: now,
                delay: Default::default(),
            },
        }
    }

    fn can_send(&mut self, now: Instant, packet_size_bytes: usize) -> bool {
        let delay = packet_delay(self.bandwidth_bps, packet_size_bytes);
        self.batch.update_and_check_has_room(now, delay)
    }

    fn duration_until_can_send(&mut self, now: Instant, packet_size_bytes: usize) -> Duration {
        let delay = packet_delay(self.bandwidth_bps, packet_size_bytes);
        if self.batch.update_and_check_has_room(now, delay) {
            Duration::default()
        } else {
            // No room left in the batch... The next send possibility is after the batch gets sent
            self.batch.sent_at() - now
        }
    }

    fn track_send(&mut self, now: Instant, packet_size_bytes: usize) {
        let delay = packet_delay(self.bandwidth_bps, packet_size_bytes);
        assert!(self.batch.update_and_check_has_room(now, delay));

        let time_since_created = now - self.batch.created;
        self.batch.delay = cmp::max(self.batch.delay, time_since_created);
        self.batch.delay += delay;
    }
}

fn packet_delay(bandwidth_bps: f64, packet_size_bytes: usize) -> Duration {
    let packet_size_bits = packet_size_bytes.saturating_mul(8);
    Duration::from_secs_f64(packet_size_bits as f64 / bandwidth_bps)
}

pub(crate) struct OutgoingPacket {
    pub(crate) in_node_since: Instant,
    pub(crate) src_node: Arc<Node>,
    pub(crate) data: InTransitData,
    pub(crate) anomalies: PacketAnomalies,
    pub(crate) preferred_links: VecDeque<Arc<Mutex<NetworkLink>>>,
    pub(crate) handled_tx: tokio::sync::watch::Sender<bool>,
    pub(crate) handled_rx: tokio::sync::watch::Receiver<bool>,
}

#[derive(Clone)]
pub(crate) struct BufferedPacket {
    pub(crate) in_node_since: Instant,
    pub(crate) data: InTransitData,
}

impl OutgoingPacket {
    pub(crate) fn already_handled(&self) -> bool {
        *self.handled_rx.borrow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BANDWIDTH_10_KB: u64 = 10 * 1000;
    const ONE_KB_BYTES: usize = 1000 / 8;

    #[test]
    fn test_pacer_can_send_bigger_than_bandwidth() {
        let mut pacer = PacketPacer::new(Instant::now(), BANDWIDTH_10_KB);
        let payload_bytes = 20 * ONE_KB_BYTES;

        assert!(packet_delay(BANDWIDTH_10_KB as f64, payload_bytes) > BATCH_MAX_DELAY);

        let now = Instant::now();
        assert!(pacer.can_send(now, payload_bytes));
        pacer.track_send(now, payload_bytes);

        // Can't send right after
        assert!(!pacer.can_send(now, payload_bytes));
        assert!(!pacer.can_send(now + Duration::from_secs(1), payload_bytes));

        // Can send much later
        assert!(pacer.can_send(now + Duration::from_secs(100), payload_bytes));
    }

    #[test]
    fn test_pacer_batches_packets() {
        let now = Instant::now();
        let mut pacer = PacketPacer::new(now, BANDWIDTH_10_KB * 1000);

        assert_eq!(pacer.batch.created, now);
        assert_eq!(pacer.batch.delay, Duration::from_millis(0));

        // Send one
        assert!(pacer.can_send(now, ONE_KB_BYTES));
        pacer.track_send(now, ONE_KB_BYTES);
        assert_eq!(pacer.batch.created, now);
        assert_eq!(pacer.batch.delay, Duration::from_micros(100));

        // Send another
        assert!(pacer.can_send(now, ONE_KB_BYTES));
        pacer.track_send(now, ONE_KB_BYTES);
        assert_eq!(pacer.batch.created, now);
        assert_eq!(pacer.batch.delay, Duration::from_micros(200));

        // Send another, 1ms later resets the batch
        let later = now + Duration::from_millis(1);
        assert!(pacer.can_send(later, ONE_KB_BYTES));
        pacer.track_send(later, ONE_KB_BYTES);
        assert_eq!(pacer.batch.created, later);
        assert_eq!(pacer.batch.delay, Duration::from_micros(100));

        // Fail to send a too-big packet right after
        assert!(!pacer.can_send(later, ONE_KB_BYTES * 100));
    }

    #[test]
    fn test_pacer_can_send_after_wait() {
        let now = Instant::now();
        let mut pacer = PacketPacer::new(now, BANDWIDTH_10_KB * 100);

        // Send one
        pacer.track_send(now, 10);
        assert_eq!(pacer.batch.created, now);
        assert!(pacer.batch.delay < Duration::from_millis(1));

        // Fail to send second
        assert!(!pacer.can_send(now, 1000));
        let delay = pacer.duration_until_can_send(now, 1000);

        // Send after the wait
        let after_wait = now + delay;
        assert!(pacer.can_send(after_wait, 1000));
    }
}
