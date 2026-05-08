use crate::InTransitData;
use crate::async_rt::time::Instant;
use crate::network::event::{LinkEventPayload, NodeEventPayload};
use crate::network::link::NetworkLink;
use crate::network::node::Node;
use crate::network::spec::NetworkSpec;
use crate::tracing::simulation_step;
use crate::tracing::simulation_step::{
    DropReason, GenericPacketEvent, PacketDropped, PacketHasExtraDelay, PacketInNode,
    PacketInTransit, PacketLostInTransit, SimulationStep, SimulationStepKind,
};
use crate::tracing::simulation_stepper::SimulationStepper;
use crate::tracing::simulation_verifier::SimulationVerifier;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

pub struct SimulationStepTracer {
    simulation_start: Instant,
    recorded_steps: Mutex<SimulationStepper>,
    network_spec: NetworkSpec,
    already_warned_dropped_from_buffer: Mutex<HashSet<Arc<str>>>,
    enable_warnings: bool,
}

impl SimulationStepTracer {
    pub fn new(spec: NetworkSpec) -> Self {
        Self {
            simulation_start: Instant::now(),
            recorded_steps: Default::default(),
            network_spec: spec,
            already_warned_dropped_from_buffer: Mutex::default(),
            enable_warnings: true,
        }
    }

    pub fn mute_warnings(mut self) -> Self {
        self.enable_warnings = false;
        self
    }

    fn warn(&self, message: &str) {
        if self.enable_warnings {
            println!(
                "{:.2}s WARN {message}",
                self.simulation_start.elapsed().as_secs_f64(),
            );
        }
    }

    pub fn is_fresh(&self) -> bool {
        self.simulation_start.elapsed().is_zero()
    }

    pub fn stepper(&self) -> SimulationStepper {
        self.recorded_steps.lock().clone()
    }

    pub fn verifier(&self) -> anyhow::Result<SimulationVerifier> {
        let steps = self.recorded_steps.lock().clone().steps();
        SimulationVerifier::new(steps, &self.network_spec)
    }

    fn record(&self, kind: SimulationStepKind) {
        self.recorded_steps.lock().record(SimulationStep {
            relative_time: self.simulation_start.elapsed(),
            kind,
        });
    }

    pub fn track_link_event(&self, event: LinkEventPayload) {
        self.record(SimulationStepKind::NetworkEvent(
            simulation_step::NetworkEvent {
                link: Some(event),
                node: None,
            },
        ));
    }

    pub fn track_node_event(&self, event: NodeEventPayload) {
        self.record(SimulationStepKind::NetworkEvent(
            simulation_step::NetworkEvent {
                link: None,
                node: Some(event),
            },
        ));
    }

    pub fn track_packet_in_node(&self, node: &Node, packet: &InTransitData) {
        self.record(SimulationStepKind::PacketInNode(PacketInNode {
            packet_id: packet.id,
            packet_number: packet.number,
            packet_size_bytes: packet.transmit.packet_size(),
            node_id: node.id().clone(),
            dropped_on_arrival: false,
        }));
    }

    pub fn track_packet_in_transit(&self, node: &Node, link: &NetworkLink, packet: &InTransitData) {
        self.record(SimulationStepKind::PacketInTransit(PacketInTransit {
            packet_id: packet.id,
            node_id: node.id().clone(),
            link_id: link.id.clone(),
        }));
    }

    pub fn track_dropped_on_arrival(&self, node: &Node, packet: &InTransitData) {
        self.record(SimulationStepKind::PacketInNode(PacketInNode {
            packet_id: packet.id,
            packet_number: packet.number,
            packet_size_bytes: packet.transmit.packet_size(),
            node_id: node.id().clone(),
            dropped_on_arrival: true,
        }));
    }

    pub fn track_dropped_by_buffer_clear_event(&self, node: &Node, packet: &InTransitData) {
        self.record(SimulationStepKind::PacketDropped(PacketDropped {
            packet_id: packet.id,
            node_id: node.id().clone(),
            reason: DropReason::BufferCleared,
        }));
    }

    pub fn track_dropped_randomly(&self, node: &Node, packet: &InTransitData) {
        self.record(SimulationStepKind::PacketDropped(PacketDropped {
            packet_id: packet.id,
            node_id: node.id().clone(),
            reason: DropReason::Random,
        }));

        self.warn(&format!(
            "{} packet lost (#{})!",
            packet.source_id, packet.number,
        ));
    }

    pub fn track_dropped_because_buffer_full(&self, node: &Node, packet: &InTransitData) {
        self.record(SimulationStepKind::PacketDropped(PacketDropped {
            packet_id: packet.id,
            node_id: node.id().clone(),
            reason: DropReason::BufferFull,
        }));

        let first_dropped = self
            .already_warned_dropped_from_buffer
            .lock()
            .insert(node.id.clone());
        if first_dropped {
            self.warn(&format!(
                "packet #{} dropped by node `{}` because its outbound buffer is full! (Note: further warnings for this link will be omitted to avoid cluttering the output)",
                packet.number,
                node.id(),
            ));
        }
    }

    pub fn track_lost_in_transit(&self, link: &NetworkLink, data: &InTransitData) {
        self.record(SimulationStepKind::PacketLostInTransit(
            PacketLostInTransit {
                packet_id: data.id,
                link_id: link.id.clone(),
            },
        ));
    }

    pub fn track_injected_failures(
        &self,
        node: &Node,
        packet: &InTransitData,
        duplicate: bool,
        extra_delay: Duration,
        congestion_experienced: bool,
    ) {
        if !extra_delay.is_zero() {
            self.record(SimulationStepKind::PacketExtraDelay(PacketHasExtraDelay {
                packet_id: packet.id,
                node_id: node.id().clone(),
                extra_delay,
            }));
        }

        if duplicate {
            self.record(SimulationStepKind::PacketDuplicated(GenericPacketEvent {
                packet_id: packet.id,
                packet_number: packet.number,
                packet_size_bytes: packet.transmit.packet_size(),
                node_id: node.id().clone(),
            }));

            self.warn(&format!(
                "{} sent duplicate packet (#{})!",
                node.id(),
                packet.number,
            ));
        }

        if congestion_experienced {
            self.record(SimulationStepKind::PacketCongestionEvent(
                GenericPacketEvent {
                    packet_id: packet.id,
                    packet_number: packet.number,
                    packet_size_bytes: packet.transmit.packet_size(),
                    node_id: node.id().clone(),
                },
            ));

            self.warn(&format!(
                "{} marked packet with CE ECN (#{})!",
                node.id(),
                packet.number,
            ));
        }
    }

    pub fn track_read_by_application(&self, node_id: Arc<str>, data: &InTransitData) {
        self.record(SimulationStepKind::PacketDeliveredToApplication(
            GenericPacketEvent {
                packet_id: data.id,
                packet_number: data.number,
                packet_size_bytes: data.transmit.packet_size(),
                node_id,
            },
        ));
    }
}
