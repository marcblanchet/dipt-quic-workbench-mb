use crate::config::quinn::{CongestionControlAlgorithm, QuinnJsonConfig};
use crate::quinn_extensions::ecn_cc::EcnCcFactory;
use crate::quinn_extensions::no_cc::NoCCConfig;
use in_memory_network::async_rt::time::Instant;
use quinn_proto::congestion::{CubicConfig, NewRenoConfig};
use quinn_proto::{AckFrequencyConfig, EndpointConfig, QlogConfig, TransportConfig, VarInt};
use std::fs::File;
use std::sync::Arc;
use std::time::Duration;

pub mod client;
pub mod server;

fn endpoint_config(rng_seed: [u8; 32]) -> EndpointConfig {
    let mut config = EndpointConfig::default();
    config.rng_seed(Some(rng_seed));

    config
}

fn transport_config(
    start: Instant,
    quinn_config: &QuinnJsonConfig,
    qlog_file: File,
) -> TransportConfig {
    let mut config = TransportConfig::default();

    let mut qlog_config = QlogConfig::default();
    qlog_config.writer(Box::new(qlog_file));
    qlog_config.start_time(start.into());
    let qlog_stream = qlog_config.into_stream().unwrap();
    config.qlog_stream(Some(qlog_stream));

    let mtu_enabled = quinn_config.mtu_discovery.unwrap_or(true);
    if !mtu_enabled {
        config.mtu_discovery_config(None);
    }

    if let Some(timeout) = quinn_config.maximum_idle_timeout_ms {
        config.max_idle_timeout(Some(Duration::from_millis(timeout).try_into().unwrap()));
    }

    if quinn_config
        .maximize_send_and_receive_windows
        .unwrap_or(false)
    {
        config.receive_window(VarInt::MAX);
        config.stream_receive_window(VarInt::MAX);
        config.send_window(u64::MAX);
    }

    if let Some(packet_threshold) = quinn_config.packet_threshold {
        config.packet_threshold(packet_threshold);
    }

    if let Some(time_threshold) = quinn_config.time_threshold {
        config.time_threshold(time_threshold);
    }

    let get_congestion_window_bytes = |packets: u64| packets * BASE_DATAGRAM_SIZE;
    let cc_factory: Arc<dyn quinn_proto::congestion::ControllerFactory + Send + Sync> =
        match quinn_config
            .congestion_controller
            .unwrap_or(CongestionControlAlgorithm::Cubic)
        {
            CongestionControlAlgorithm::Cubic => {
                let mut cfg = CubicConfig::default();
                if let Some(packets) = quinn_config.initial_congestion_window_packets {
                    cfg.initial_window(get_congestion_window_bytes(packets));
                }
                Arc::new(cfg)
            }
            CongestionControlAlgorithm::NewReno => {
                let mut cfg = NewRenoConfig::default();
                if let Some(packets) = quinn_config.initial_congestion_window_packets {
                    cfg.initial_window(get_congestion_window_bytes(packets));
                }
                Arc::new(cfg)
            }
            CongestionControlAlgorithm::NoCc => {
                let congestion_window_bytes =
                    if let Some(packets) = quinn_config.initial_congestion_window_packets {
                        get_congestion_window_bytes(packets)
                    } else {
                        u64::MAX
                    };
                Arc::new(NoCCConfig {
                    initial_window: congestion_window_bytes,
                })
            }
            CongestionControlAlgorithm::EcnReno => {
                let mut cfg = NewRenoConfig::default();
                if let Some(packets) = quinn_config.initial_congestion_window_packets {
                    cfg.initial_window(get_congestion_window_bytes(packets));
                }
                Arc::new(EcnCcFactory::new(cfg))
            }
        };
    config.congestion_controller_factory(cc_factory);

    if let Some(quinn_config_ack_frequency) = &quinn_config.ack_frequency_config {
        let mut ack_frequency_config = AckFrequencyConfig::default();
        if let Some(threshold) = quinn_config_ack_frequency.ack_eliciting_threshold {
            ack_frequency_config.ack_eliciting_threshold(VarInt::from_u32(threshold));
        }

        if let Some(delay) = quinn_config_ack_frequency.max_ack_delay_ms {
            ack_frequency_config.max_ack_delay(Some(Duration::from_millis(delay)));
        }

        // The docs say the recommended value for this is `packet_threshold - 1`
        ack_frequency_config.reordering_threshold(VarInt::from_u32(
            quinn_config.packet_threshold.unwrap_or(3) - 1,
        ));
        config.ack_frequency_config(Some(ack_frequency_config));
    }

    if let Some(initial_rtt) = quinn_config.initial_rtt_ms {
        config.initial_rtt(Duration::from_millis(initial_rtt));
    }

    config
}

const BASE_DATAGRAM_SIZE: u64 = 1200;
