use event_listener::Event;
use in_memory_network::network::InMemoryNetwork;
use in_memory_network::tracing::simulation_verifier::VerifiedSimulation;
use in_memory_network::tracing::stats::NodeStats;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub fn print_max_buffer_usage_per_node(verified_simulation: &VerifiedSimulation) {
    println!("--- Max buffer usage per node ---");
    let mut buffer_usage: Vec<_> = verified_simulation.stats.stats_by_node.iter().collect();
    buffer_usage.sort_unstable_by(|t1, t2| {
        t1.1.max_buffer_usage
            .cmp(&t2.1.max_buffer_usage)
            .then(t2.0.cmp(t1.0))
    });
    for (node_id, stats) in buffer_usage.into_iter().rev() {
        println!(
            "* {node_id}: {} bytes ({} packets dropped due to buffer being full)",
            stats.max_buffer_usage, stats.dropped_buffer_full.packets
        );
    }
}

pub fn print_link_stats(verified_simulation: &VerifiedSimulation, network: &InMemoryNetwork) {
    if !verified_simulation.stats.stats_by_link.is_empty() {
        println!("--- Link stats ---");
    }
    let mut link_stats: Vec<_> = verified_simulation.stats.stats_by_link.iter().collect();
    link_stats.sort_unstable_by_key(|(id, _)| *id);
    for (link_id, stats) in link_stats {
        println!("* {link_id}:");
        println!(
            "|-> Lost in transit {} packets ({} bytes)",
            stats.dropped_in_transit.packets, stats.dropped_in_transit.bytes
        );

        let bandwidth_bps = network.get_link_bandwidth_bps(link_id);
        let usage_ratio = stats.max_used_bandwidth_bps as f64 / bandwidth_bps as f64 * 100.0;
        println!(
            "|-> Max used bandwidth (bps): {} ({usage_ratio:.2}% of the link's bandwidth)",
            stats.max_used_bandwidth_bps
        );
    }
}

pub fn print_node_stats(
    node_ids: &[Arc<str>],
    verified_simulation: &VerifiedSimulation,
    node_ids_by_role: &HashMap<&str, Vec<&str>>,
    verbose: bool,
) {
    let mut roles = node_ids_by_role.keys().copied().collect::<Vec<_>>();
    roles.sort();

    let mut already_reported_ids = HashSet::<&str>::new();

    for role in roles {
        let ids = &node_ids_by_role[role];
        for &id in ids {
            let stats = &verified_simulation.stats.stats_by_node[id];
            println!("* {id} ({role})");
            print_single_node_stats(stats);
        }

        already_reported_ids.extend(ids);
    }

    if verbose {
        for id in node_ids {
            if already_reported_ids.contains(&id.as_ref()) {
                continue;
            }

            let stats = &verified_simulation.stats.stats_by_node[id];
            println!("* {id}");
            print_single_node_stats(stats);
        }
    }
}

fn print_single_node_stats(stats: &NodeStats) {
    println!(
        "  * Sent packets: {} ({} bytes)",
        stats.sent.packets, stats.sent.bytes,
    );
    println!(
        "    | {} packets duplicated ({} bytes)",
        stats.duplicates.packets, stats.duplicates.bytes
    );
    println!(
        "    | {} packets marked with the CE ECN codepoint ({} bytes)",
        stats.congestion_experienced.packets, stats.congestion_experienced.bytes
    );
    let dropped = stats
        .dropped_injected
        .join(&stats.dropped_buffer_full)
        .join(&stats.dropped_on_arrival)
        .join(&stats.dropped_buffer_cleared);
    print!(
        "    | {} packets dropped ({} bytes)",
        dropped.packets, dropped.bytes
    );
    if dropped.packets != 0 {
        println!(". Caused by:");
        if stats.dropped_on_arrival.packets != 0 {
            println!(
                "      | node was down when the packet arrived ({} packets, {} bytes)",
                stats.dropped_on_arrival.packets, stats.dropped_on_arrival.bytes
            );
        }
        if stats.dropped_buffer_full.packets != 0 {
            println!(
                "      | buffer was full when the packet arrived ({} packets, {} bytes)",
                stats.dropped_buffer_full.packets, stats.dropped_buffer_full.bytes
            );
        }
        if stats.dropped_buffer_cleared.packets != 0 {
            println!(
                "      | buffer got cleared before the packet was sent ({} packets, {} bytes)",
                stats.dropped_buffer_cleared.packets, stats.dropped_buffer_cleared.bytes
            );
        }
        if stats.dropped_injected.packets != 0 {
            println!(
                "      | randomly dropped ({} packets, {} bytes)",
                stats.dropped_injected.packets, stats.dropped_injected.bytes
            );
        }
    } else {
        println!();
    }
    println!(
        "  * Received packets: {} ({} bytes)",
        stats.received.packets, stats.received.bytes
    );
    println!(
        "    | {} packets received out of order ({} bytes)",
        stats.received_out_of_order.packets, stats.received_out_of_order.bytes
    );
}

pub struct CancellationToken {
    cancelled: AtomicBool,
    event: Event,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: false.into(),
            event: Event::new(),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.event.notify(usize::MAX);
    }

    pub async fn cancelled(&self) {
        if !self.cancelled.load(Ordering::Relaxed) {
            self.event.listen().await;
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

pub fn duplicates<'a>(items: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut seen = HashSet::new();
    let mut duplicates = Vec::new();
    for item in items {
        let new = seen.insert(item);
        if !new {
            duplicates.push(item);
        }
    }

    duplicates
}
