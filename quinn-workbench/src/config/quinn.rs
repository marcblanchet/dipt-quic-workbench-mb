use serde::Deserialize;

#[derive(Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
pub enum CongestionControlAlgorithm {
    /// Cubic congestion control (Quinn default)
    Cubic,
    /// NewReno congestion control
    NewReno,
    /// Disables congestion control and uses the intial_congestion_window as a fixed window instead
    NoCc,
    /// Configures congestion control to use a variant of `NewReno` that ignores packet
    /// loss and only takes ECN into consideration.
    EcnReno,
}

#[derive(Deserialize, Clone, Default)]
pub struct QuinnJsonConfig {
    /// The initial RTT of the QUIC connection, in milliseconds (used before an RTT sample is
    /// available).
    ///
    /// For delay-tolerant networking, it is recommended to set this to a value slightly higher than
    /// the real RTT. If the value is too low, there will be needless retransmissions of packets
    /// until the endpoint is able to infer the real RTT.
    ///
    /// Defaults to `333`
    pub initial_rtt_ms: Option<u64>,
    /// The maximum idle timeout of the QUIC connection, in milliseconds.
    ///
    /// When expecting a continuous exchange of information, a small idle timeout helps to detect
    /// connection loss. In delay-tolerant networking, it is useful to use a very high timeout, to
    /// ensure the connection never gets lost due to unexpected delays.
    ///
    /// Defaults to `30_000` (30 seconds)
    pub maximum_idle_timeout_ms: Option<u64>,
    /// Maximum reordering in packet numbers before considering a packet lost. Should not be less
    /// than 3, per RFC5681.
    ///
    /// Defaults to `3`
    pub packet_threshold: Option<u32>,
    /// Whether MTU discovery should be enabled
    ///
    /// Defaults to `false`
    pub mtu_discovery: Option<bool>,
    /// Whether the send and receive windows should be maximized, allowing an unbounded number of
    /// unacknowledged in-flight packets
    ///
    /// Defaults to `false`
    pub maximize_send_and_receive_windows: Option<bool>,
    /// Configures the ACK Frequency QUIC extension
    ///
    /// Defaults to `None`, meaning that the ACK Frequency extension is disabled
    pub ack_frequency_config: Option<AckFrequencyConfig>,
    /// Which congestion control algorithm to use
    ///
    /// Defaults to [CongestionControlAlgorithm::Cubic]
    pub congestion_controller: Option<CongestionControlAlgorithm>,
    /// The initial congestion window size in multiples of base datagram size.
    ///
    /// The default value depends on the congestion control algorithm's. For 'NoCc', that default is
    /// `u64::MAX`. For other algorithms, the default is a value suitable for terrestrial
    /// communication.
    pub initial_congestion_window_packets: Option<u64>,
}

#[derive(Deserialize, Clone, Default)]
pub struct AckFrequencyConfig {
    /// The number of ACK-eliciting packets an endpoint may receive without immediately sending an
    /// ACK.
    ///
    /// Setting this threshold to a high value is particularly useful when we expect to receive long
    /// streams of information from the server, without sending anything back from the client.
    ///
    /// Defaults to `1`
    pub ack_eliciting_threshold: Option<u32>,
    /// The maximum amount of time that an endpoint waits before sending an ACK when the
    /// ACK-eliciting threshold hasn't been reached.
    ///
    /// Setting this to a high value is particularly useful in combination with a high ACK-eliciting
    /// threshold.
    ///
    /// Defaults to `None`, in which case the peer’s original `max_ack_delay` will be used, as
    /// obtained from its transport parameters.
    pub max_ack_delay_ms: Option<u64>,
}
