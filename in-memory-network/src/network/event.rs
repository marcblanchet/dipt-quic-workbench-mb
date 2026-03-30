use crate::network::spec::{NetworkLinkSpec, NetworkNodeSpec};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct NetworkEvents {
    pub(crate) sorted_events: Vec<NetworkEvent>,
    pub(crate) initial_link_events: Vec<LinkEventPayload>,
    pub(crate) initial_node_events: Vec<NodeEventPayload>,
}

impl NetworkEvents {
    pub fn new(
        mut events: Vec<NetworkEvent>,
        nodes: &[NetworkNodeSpec],
        links: &[NetworkLinkSpec],
    ) -> Self {
        events.sort_by_key(|e| e.relative_time);
        let initial_link_statuses = get_initial_status_for_links_with_events(&events, links);
        let initial_node_statuses = get_initial_status_for_nodes_with_events(&events, nodes);
        Self {
            sorted_events: events,
            initial_link_events: initial_link_statuses,
            initial_node_events: initial_node_statuses,
        }
    }
}

#[derive(Clone)]
pub struct NetworkEvent {
    pub relative_time: Duration,
    pub kind: NetworkEventKind,
}

#[derive(Clone)]
pub enum NetworkEventKind {
    Link(LinkEventPayload),
    Node(NodeEventPayload),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct NodeEventPayload {
    #[serde(with = "crate::util::serde_arc_str")]
    pub node_id: Arc<str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<UpdateNodeStatus>,
    #[serde(skip_serializing_if = "crate::util::is_false")]
    pub clear_buffer: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkEventPayload {
    #[serde(with = "crate::util::serde_arc_str")]
    pub link_id: Arc<str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<UpdateLinkStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bandwidth_bps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<Duration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_delay: Option<Duration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_delay_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packet_duplication_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packet_loss_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub congestion_event_ratio: Option<f64>,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateNodeStatus {
    Up,
    Down,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateLinkStatus {
    Up,
    Down,
}

fn get_initial_status_for_links_with_events(
    sorted_events: &[NetworkEvent],
    links: &[NetworkLinkSpec],
) -> Vec<LinkEventPayload> {
    let mut seen_links = HashSet::new();
    let mut initial_events = Vec::new();
    for event in sorted_events {
        if let NetworkEventKind::Link(link_payload) = &event.kind
            && let Some(updated_status) = link_payload.status
        {
            let newly_inserted = seen_links.insert(link_payload.link_id.clone());
            if !newly_inserted {
                // We are only interested in events for links we haven't seen yet
                continue;
            }

            let initial_status = match updated_status {
                UpdateLinkStatus::Up => UpdateLinkStatus::Down,
                UpdateLinkStatus::Down => UpdateLinkStatus::Up,
            };

            initial_events.push(LinkEventPayload {
                link_id: link_payload.link_id.clone(),
                status: Some(initial_status),
                bandwidth_bps: None,
                delay: None,
                extra_delay: None,
                extra_delay_ratio: None,
                packet_duplication_ratio: None,
                packet_loss_ratio: None,
                congestion_event_ratio: None,
            });
        }
    }

    // Links that have no events at all are always up
    for link in links {
        if !seen_links.contains(&link.id) {
            initial_events.push(LinkEventPayload {
                link_id: link.id.clone(),
                status: Some(UpdateLinkStatus::Up),
                bandwidth_bps: None,
                delay: None,
                extra_delay: None,
                extra_delay_ratio: None,
                packet_duplication_ratio: None,
                packet_loss_ratio: None,
                congestion_event_ratio: None,
            });
        }
    }

    initial_events
}

fn get_initial_status_for_nodes_with_events(
    sorted_events: &[NetworkEvent],
    nodes: &[NetworkNodeSpec],
) -> Vec<NodeEventPayload> {
    let mut seen_nodes = HashSet::new();
    let mut initial_events = Vec::new();
    for event in sorted_events {
        if let NetworkEventKind::Node(node_payload) = &event.kind
            && let Some(updated_status) = node_payload.status
        {
            let newly_inserted = seen_nodes.insert(node_payload.node_id.clone());
            if !newly_inserted {
                // We are only interested in events for nodes we haven't seen yet
                continue;
            }

            let initial_status = match updated_status {
                UpdateNodeStatus::Up => UpdateNodeStatus::Down,
                UpdateNodeStatus::Down => UpdateNodeStatus::Up,
            };

            initial_events.push(NodeEventPayload {
                node_id: node_payload.node_id.clone(),
                status: Some(initial_status),
                clear_buffer: false,
            });
        }
    }

    // Nodes that have no events at all are always up
    for node in nodes {
        if !seen_nodes.contains(&node.id) {
            initial_events.push(NodeEventPayload {
                node_id: node.id.clone(),
                status: Some(UpdateNodeStatus::Up),
                clear_buffer: false,
            });
        }
    }

    initial_events
}
