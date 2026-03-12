QUIC Workbench
==============

A command-line application to simulate QUIC connections in different scenarios (network topology and
QUIC parameters). The simulation creates one or more connections, issues a fixed number of requests
from the client to the server, and streams the server's responses back to the client.

## Features

- Pure. No IO operations are made, everything happens in-memory within a single process.
- Time warping. The simulation's internal clock advances automatically to the next event, making the
  simulation complete in an instant (even in the presence of deep-space-like RTTs).
- Deterministic. Two runs with the same parameters yield the same output.
- Inspectable. Next to informative command-line output and statistics, the application generates a
  synthetic pcap file, so you can examine the traffic in more detail using Wireshark.
- Configurable network settings and QUIC parameters through reusable JSON config files (see
  `test-data` and [JSON config details](#json-config-details)).
- Configurable simulation behavior through command-line arguments (see `cargo run --release --
  --help`).

## Getting started

After [installing Rust](https://rustup.rs/), you can get started with:

```bash
cargo run --release --bin quinn-workbench -- \
  quic \
  --network-graph test-data/earth-mars/networkgraph-fullmars.json \
  --network-events test-data/earth-mars/events.json \
  --client-ip-address 192.168.40.1 \
  --server-ip-address 192.168.43.2
```

Here's an example issuing a single request and receiving a 10 MiB response:

```bash
cargo run --release --bin quinn-workbench -- \
  quic \
  --network-graph test-data/earth-mars/networkgraph-fullmars.json \
  --network-events test-data/earth-mars/events.json \
  --client-ip-address 192.168.40.1 \
  --server-ip-address 192.168.43.2 \
  --requests 1 --response-size 10485760
```

Here's an example controlling the random seeds (which otherwise use a hardcoded constant):

```bash
cargo run --release --bin quinn-workbench -- \
  quic \
  --network-graph test-data/earth-mars/networkgraph-fullmars.json \
  --network-events test-data/earth-mars/events.json \
  --client-ip-address 192.168.40.1 \
  --server-ip-address 192.168.43.2 \
  --network-rng-seed 1337 \
  --quinn-rng-seed 1234
```

Here's an example using random seeds derived from a source of entropy:

```bash
cargo run --release --bin quinn-workbench -- \
  quic \
  --network-graph test-data/earth-mars/networkgraph-fullmars.json \
  --network-events test-data/earth-mars/events.json \
  --client-ip-address 192.168.40.1 \
  --server-ip-address 192.168.43.2 \
  --non-deterministic
```

## JSON config details

#### Network topology config

The topology configuration is fairly self-documenting. See for instance
[networkgraph-fullmars.json](test-data/earth-mars/networkgraph-fullmars.json) and
[networkgraph-5nodes.json](test-data/earth-mars/networkgraph-5nodes.json)

Note that links are uni-directional, so two entries are necessary to describe a bidirectional link.
Also, links can be configured individually with the following parameters:

- `link.delay_ms` (required): The delay of the link in milliseconds (i.e. time it takes for a packet
  to arrive to its destination).
- `link.bandwidth_bps` (required): The bandwidth of the link in bits per second.
- `link.extra_delay_ms`: The additional delay of the link in milliseconds, applied randomly
  according to `extra_delay_ratio`.
- `link.extra_delay_ratio`: The ratio of packets that will have an extra delay applied, used to
  artificially introduce packet reordering (the value must be between 0 and 1).
- `link.congestion_event_ratio`: The ratio of packets that will be marked with a CE ECN codepoint
  (the value must be between 0 and 1).

Next to links, nodes can be configured with the following parameters too:

- `node.packet_duplication_ratio`: The ratio of packets that will be duplicated upon arrival to the
  node, (the value must be between 0 and 1).
- `node.packet_loss_ratio`: The ratio of packets that will be lost upon arrival to the node (the
  value must be between 0 and 1).

#### Network events config

Network events are used to bring links up and down at different times of the simulation (e.g. to
simulate an orbiter being unreachable at specific intervals). The format is fairly self-documenting,
as you can see in [events.json](test-data/earth-mars/events.json).

#### QUIC config

Each host node in a network graph's json file may have a `quic` field, specifying the QUIC
parameters used by that node. All fields are optional and fall back to the QUIC implementation's defaults (documented below). **Important**: defaults assume a terrestrial communication scenario, so you will most likely want to change them!

Consider the following example in which all parameters are specified:

```json
{
  "initial_rtt_ms": 100000000,
  "maximum_idle_timeout_ms": 100000000000,
  "packet_threshold": 4294967295,
  "mtu_discovery": false,
  "maximize_send_and_receive_windows": true,
  "ack_frequency_config": {
    "max_ack_delay_ms": 18446744073709551615,
    "ack_eliciting_threshold": 10
  },
  "congestion_controller": "no_cc",
  "initial_congestion_window_packets": 200000
}
```

Here's the meaning of the different parameters:

- `initial_rtt_ms`: The initial Round Trip Time (RTT) of the QUIC connection in milliseconds
  (used before an actual RTT sample is available). For delay-tolerant networking, set this slightly
  higher than the expected real RTT to avoid unnecessary packet retransmissions. Defaults to 333 milliseconds.
- `maximum_idle_timeout_ms`: The maximum idle timeout of the QUIC connection in milliseconds.
  For continuous information exchange, use a small value to detect connection loss quickly. For
  delay-tolerant networking, use a very high value to prevent connection loss due to unexpected
  delays. Defaults to `30000` (30 seconds).
- `packet_threshold`: Maximum reordering in packet numbers before considering a packet lost.
  Should not be less than 3, as per RFC5681. Defaults to `3`.
- `mtu_discovery`: Boolean flag to enable or disable MTU discovery. Defaults to `true`.
- `maximize_send_and_receive_windows`: Boolean flag to maximize send and receive windows,
  allowing an unlimited number of unacknowledged in-flight packets. Defaults to `false`.
- `ack_frequency_config`: Configures the ACK Frequency QUIC extension. When omitted, the ACK
  Frequency extension is disabled. Contains the following sub-fields:
  - `max_ack_delay_ms`: The maximum amount of time, in milliseconds, that an endpoint waits
  before sending an ACK when the ACK-eliciting threshold hasn't been reached. Setting this to a high
  value is useful in combination with a high ACK-eliciting threshold. When omitted, the
    peer's original `max_ack_delay` will be used, as obtained from its transport parameters.
  - `ack_eliciting_threshold`: The number of ACK-eliciting packets an endpoint may receive
    without immediately sending an ACK. A high value is useful when expecting long streams of
    information from the server without sending anything back from the client. Defaults to `1`.
- `congestion_controller`: The congestion control algorithm to use.
  Currently supported options: new_reno, cubic, ecn_reno, no_cc. Defaults to `cubic`.
- `initial_congestion_window_packets`: The initial congestion window is set to this value
  times the base datagram size (1200 bytes). The default depends on the congestion control
  algorithm. For `no_cc`, the default is effectively unlimited. For other algorithms, the default is
  a value suitable for terrestrial communication.

## Command line arguments

The tool is self-documenting, so running it with `--help` will show up-to-date information about
command line arguments.

## Routing

When a node receives a packet that should be forwarded, routing happens as follows: if the node's buffer does not have enough capacity, drop the packet; otherwise, enqueue the packet so it gets sent through the first link that becomes available. Here are some notes to clarify the details:

1. Links handle outgoing packets in a FIFO manner.
2. A link is considered available when it is both up and has enough bandwidth to send a given packet.
3. The outgoing link is not chosen when the packet is received by the node, but when the packet can actually get sent to the next hop (i.e., when a suitable link is found which is up and has enough bandwidth for sending).
4. When multiple links are available at the same time, the cheapest one gets to send the packet (according to the link's `cost` field in the configured network topology).

A side-effect of the simulator's routing mechanism is that we automatically "load balance" packets when one of the links is saturated. Such a link is considered unavailable (not enough bandwidth), so if a second link is available towards the same destination, the packet will be routed through it instead of the saturated one.

## Validation

Simulating an IP network is complex, so we need to ensure the implementation is actually sound. For
that purpose, the simulator records events to a so-called _replay log_, which can be used to
independently replay the network traffic and verify that all network invariants were satisfied. We
automatically run our verifier after every simulation and raise an error if applicable.

At the time of this writing, we are validating the following properties of the network:

- Packets are only created at host nodes
- Packets are only duplicated when a link injects a randomized duplication (see
  `link.packet_duplication_ratio` above)
- When packets are transmitted, they must travel through a link to which both the source and the
  target nodes are connected
- Packets are never transmitted through a link that is known to be offline at that moment
- Packets are lost if the link goes down during transmission (after the packet was sent, but before
  it arrives)
- Packets are received only after enough time passes since they were sent (taking the link's latency
  into account and random delays injected through `link.extra_delay_ms`)
- Nodes never exceed their configured buffer size
- Links never exceed their configured bandwidth

### Acknowledgements

With special thanks to Marc Blanchet ([Viagénie inc.](https://www.viagenie.ca/)) for funding this
work.
