use crate::tracing::simulation_step::{DropReason, SimulationStep, SimulationStepKind};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct SimulationStepper {
    steps: Vec<SimulationStep>,
}

impl SimulationStepper {
    pub fn record(&mut self, step: SimulationStep) {
        self.steps.push(step);
    }

    pub fn steps(self) -> Vec<SimulationStep> {
        self.steps
    }

    pub fn get_packet_hops(&self, id: Uuid) -> Vec<(Duration, Arc<str>)> {
        let mut hops = Vec::new();
        for step in &self.steps {
            match &step.kind {
                SimulationStepKind::PacketInNode(s) if s.packet_id == id => {
                    hops.push((step.relative_time, s.node_id.clone()));
                }
                _ => {}
            }
        }

        hops
    }

    fn get_packet_sender(&self, id: Uuid) -> &str {
        for step in &self.steps {
            match &step.kind {
                SimulationStepKind::PacketInNode(s) if s.packet_id == id => {
                    return &s.node_id;
                }
                _ => {}
            }
        }

        unreachable!()
    }

    pub fn get_packet_path(&self, id: Uuid) -> Vec<Arc<str>> {
        self.get_packet_hops(id)
            .into_iter()
            .map(|(_, node_id)| node_id)
            .collect()
    }

    pub fn get_packet_sent_from(&self, packet_id: Uuid, node_id: &str) -> Option<Duration> {
        self.steps
            .iter()
            .filter_map(|s| match &s.kind {
                SimulationStepKind::PacketInTransit(kind)
                    if kind.packet_id == packet_id && kind.node_id.as_ref() == node_id =>
                {
                    Some(s.relative_time)
                }
                _ => None,
            })
            .next()
    }

    pub fn get_packet_arrived_at(&self, packet_id: Uuid, node_id: &str) -> Option<Duration> {
        self.steps
            .iter()
            .filter_map(|s| match &s.kind {
                SimulationStepKind::PacketInNode(kind)
                    if kind.packet_id == packet_id && kind.node_id.as_ref() == node_id =>
                {
                    Some(s.relative_time)
                }
                _ => None,
            })
            .next()
    }

    pub fn get_dropped_packet_info(&self, source_node: &str) -> DroppedPacketInfo {
        let mut dropped_by_loop = Vec::new();
        let mut dropped_by_random = 0;
        let mut dropped_by_full = 0;
        let mut dropped_by_cleared = 0;

        for step in &self.steps {
            if let SimulationStepKind::PacketDropped(dropped) = &step.kind
                && self.get_packet_sender(dropped.packet_id) == source_node
            {
                match dropped.reason {
                    DropReason::Random => dropped_by_random += 1,
                    DropReason::ZeroTtl => {
                        dropped_by_loop.push(self.get_packet_path(dropped.packet_id))
                    }
                    DropReason::BufferFull => dropped_by_full += 1,
                    DropReason::BufferCleared => dropped_by_cleared += 1,
                }
            }
        }

        DroppedPacketInfo {
            loops: dropped_by_loop,
            random: dropped_by_random,
            buffer_full: dropped_by_full,
            buffer_cleared: dropped_by_cleared,
        }
    }
}

pub struct DroppedPacketInfo {
    pub loops: Vec<Vec<Arc<str>>>,
    pub random: usize,
    pub buffer_full: usize,
    pub buffer_cleared: usize,
}

impl DroppedPacketInfo {
    pub fn total_dropped_packets(&self) -> usize {
        let DroppedPacketInfo {
            loops,
            random,
            buffer_full,
            buffer_cleared,
        } = self;

        loops.len() + random + buffer_full + buffer_cleared
    }
}
