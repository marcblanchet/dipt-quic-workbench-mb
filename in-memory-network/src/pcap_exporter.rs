use crate::async_rt::time::Instant;
use crate::transmit::OwnedTransmit;
use anyhow::Context;
use bytes::Bytes;
use parking_lot::Mutex;
use pcap_file::pcapng::blocks::enhanced_packet::EnhancedPacketBlock;
use pcap_file::pcapng::blocks::interface_description::InterfaceDescriptionBlock;
use pcap_file::pcapng::blocks::section_header::SectionHeaderBlock;
use pcap_file::pcapng::{PcapNgWriter, RawBlock};
use pcap_file::{DataLink, Endianness};
use pnet_packet::ip::IpNextHeaderProtocol;
use pnet_packet::ipv4::MutableIpv4Packet;
use pnet_packet::udp::MutableUdpPacket;
use pnet_packet::{PacketSize, ipv4, udp};
use quinn::udp::Transmit;
use rustls::KeyLog;
use std::fmt::{Debug, Formatter};
use std::fs;
use std::io::{BufWriter, Write};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

#[derive(Default)]
pub struct InMemoryKeyLog {
    pub log: Mutex<Vec<String>>,
}

impl Debug for InMemoryKeyLog {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "InMemoryKeyLog")
    }
}

impl KeyLog for InMemoryKeyLog {
    fn log(&self, label: &str, client_random: &[u8], secret: &[u8]) {
        let client_random = hex::encode(client_random);
        let secret = hex::encode(secret);
        self.log
            .lock()
            .push(format!("{label} {client_random} {secret}"));
    }
}

pub struct PcapExporter {
    capture_start: Instant,
    total_tracked_packets: AtomicU64,
    writer: Mutex<PcapNgWriter<BufWriter<Box<dyn Write + Send + Sync + 'static>>>>,
    buffered_packets: Mutex<Vec<(SocketAddr, OwnedTransmit)>>,
    keylog: Option<Arc<InMemoryKeyLog>>,
    keylog_written: AtomicBool,
}

impl PcapExporter {
    pub fn new(
        writer: impl Write + Send + Sync + 'static,
        keylog: Option<Arc<InMemoryKeyLog>>,
    ) -> Self {
        let writer: Box<dyn Write + Send + Sync + 'static> = Box::new(writer);
        let mut writer = PcapNgWriter::with_section_header(
            BufWriter::new(writer),
            SectionHeaderBlock {
                endianness: Endianness::Big,
                major_version: 1,
                minor_version: 0,
                section_length: 0,
                options: vec![],
            },
        )
        .unwrap();

        writer
            .write_pcapng_block(InterfaceDescriptionBlock {
                linktype: DataLink::IPV4,
                snaplen: 65535,
                options: vec![],
            })
            .unwrap();

        Self {
            capture_start: Instant::now(),
            writer: Mutex::new(writer),
            total_tracked_packets: AtomicU64::new(0),
            buffered_packets: Mutex::default(),
            keylog,
            keylog_written: AtomicBool::new(false),
        }
    }

    pub fn for_node(node_id: &str, keylog: Option<Arc<InMemoryKeyLog>>) -> anyhow::Result<Self> {
        let file_name = format!("{node_id}.pcap");
        let pcap_file = fs::File::create(&file_name)
            .with_context(|| format!("failed to open {file_name} for writing"))?;
        Ok(PcapExporter::new(pcap_file, keylog))
    }

    pub fn noop() -> Self {
        Self::new(std::io::sink(), None)
    }

    pub fn track_transmit(&self, source_addr: SocketAddr, transmit: &Transmit) {
        if let Some(keylog) = &self.keylog
            && let log = keylog.log.lock()
            && let keylog_written = self.keylog_written.load(Ordering::Relaxed)
            && !keylog_written
        {
            // Buffer packets internally if not enough key material is present to decrypt them yet
            if log.len() < 5 {
                self.buffered_packets.lock().push((
                    source_addr,
                    OwnedTransmit {
                        destination: transmit.destination,
                        ecn: transmit.ecn,
                        contents: Bytes::copy_from_slice(transmit.contents),
                        segment_size: transmit.segment_size,
                    },
                ));

                return;
            } else {
                self.keylog_written.store(true, Ordering::Relaxed);

                let mut secrets = log.join("\n");
                secrets.push('\n');

                let mut block = Vec::new();

                let block_type: u32 = 0x0000000A;
                block.extend_from_slice(&block_type.to_be_bytes());

                // Save space for length
                let length_offset = block.len();
                block.extend_from_slice(&[0, 0, 0, 0]);

                let secrets_type: u32 = 0x544c534b;
                block.extend_from_slice(&secrets_type.to_be_bytes());

                let secrets_length = secrets.len() as u32;
                block.extend_from_slice(&secrets_length.to_be_bytes());
                block.extend_from_slice(secrets.as_bytes());

                // Pad to 4-byte boundary
                while block.len() % 4 != 0 {
                    block.push(0x00);
                }

                let total_length = block.len() as u32 + 4;
                block.extend_from_slice(&total_length.to_be_bytes());

                block[length_offset..length_offset + 4]
                    .copy_from_slice(&total_length.to_be_bytes());

                let (_, raw_block) = RawBlock::from_slice::<byteorder::BigEndian>(&block).unwrap();

                self.writer.lock().write_raw_block(&raw_block).unwrap();

                // Write previously buffered packets
                for (source_addr, transmit) in std::mem::take(&mut *self.buffered_packets.lock()) {
                    self.write_pcapng_transmit(source_addr, &transmit.as_transmit());
                }
            }
        }

        self.write_pcapng_transmit(source_addr, transmit);
    }

    fn write_pcapng_transmit(&self, source_addr: SocketAddr, transmit: &Transmit) {
        let IpAddr::V4(source) = source_addr.ip() else {
            unreachable!()
        };

        let IpAddr::V4(destination) = transmit.destination.ip() else {
            unreachable!()
        };

        let mut buffer = vec![0; 2000];

        // Wrap the data in a UDP packet
        let mut udp_writer = MutableUdpPacket::new(&mut buffer).unwrap();
        let udp_packet_length = 8 + transmit.contents.len() as u16;
        udp_writer.set_source(source_addr.port());
        udp_writer.set_destination(transmit.destination.port());
        udp_writer.set_length(udp_packet_length);
        udp_writer.set_payload(transmit.contents);
        let checksum = udp::ipv4_checksum(&udp_writer.to_immutable(), &source, &destination);
        udp_writer.set_checksum(checksum);
        drop(udp_writer);
        let udp_packet = buffer[0..udp_packet_length as usize].to_vec();

        // Wrap the UDP packet in an IP packet
        let mut ip_writer = MutableIpv4Packet::new(&mut buffer).unwrap();
        let ip_packet_length = 20 + udp_packet_length;
        ip_writer.set_version(4);
        ip_writer.set_header_length(5); // We don't use options
        ip_writer.set_dscp(0); // Copied from a Wireshark dump
        ip_writer.set_identification(0); // We never fragment
        ip_writer.set_flags(0b010); // We never fragment
        ip_writer.set_fragment_offset(0); // We never fragment
        ip_writer.set_ttl(64);
        ip_writer.set_next_level_protocol(IpNextHeaderProtocol::new(17)); // 17 = UDP
        ip_writer.set_source(source);
        ip_writer.set_destination(destination);
        ip_writer.set_payload(&udp_packet);
        ip_writer.set_total_length(ip_packet_length);
        ip_writer.set_ecn(transmit.ecn.map(|codepoint| codepoint as u8).unwrap_or(0));
        let checksum = ipv4::checksum(&ip_writer.to_immutable());
        ip_writer.set_checksum(checksum);
        let ip_packet_length = ip_writer.packet_size();
        drop(ip_writer);

        let ip_packet = buffer[0..ip_packet_length].to_vec();

        self.total_tracked_packets.fetch_add(1, Ordering::Relaxed);

        self.writer
            .lock()
            .write_pcapng_block(EnhancedPacketBlock {
                interface_id: 0,
                timestamp: correct_timestamp(self.capture_start.elapsed()),
                original_len: ip_packet.len() as u32,
                data: ip_packet.into(),
                options: Vec::new(),
            })
            .unwrap();
    }
}

fn correct_timestamp(d: Duration) -> Duration {
    // Round to the nearest millisecond
    let millis = (d.as_secs_f64() * 1000.0).round();

    // Return the time, an order of magnitude smaller (there seems to be a bug in the library we are
    // using, which multiplies seconds by 1000)
    Duration::from_secs_f64(millis / 1_000_000.0)
}
