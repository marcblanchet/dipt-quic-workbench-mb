use crate::async_rt::time::Instant;
use crate::transmit::OwnedTransmit;
use anyhow::Context;
use parking_lot::Mutex;
use pcap_file::pcapng::blocks::enhanced_packet::EnhancedPacketBlock;
use pcap_file::pcapng::blocks::interface_description::InterfaceDescriptionBlock;
use pcap_file::pcapng::blocks::section_header::SectionHeaderBlock;
use pcap_file::pcapng::{PcapNgReader, PcapNgWriter, RawBlock};
use pcap_file::{DataLink, Endianness};
use pnet_packet::ip::IpNextHeaderProtocol;
use pnet_packet::ipv4::MutableIpv4Packet;
use pnet_packet::udp::MutableUdpPacket;
use pnet_packet::{PacketSize, ipv4, udp};
use rustls::KeyLog;
use std::fmt::{Debug, Formatter};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::{fs, mem};

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

fn pcap_writer(
    writer: Box<dyn Write + Send + Sync + 'static>,
    add_interface_description: bool,
) -> anyhow::Result<PcapNgWriter<Box<dyn Write + Send + Sync + 'static>>> {
    let mut writer = PcapNgWriter::with_section_header(
        writer,
        SectionHeaderBlock {
            endianness: Endianness::Big,
            major_version: 1,
            minor_version: 0,
            section_length: 0,
            options: vec![],
        },
    )?;

    if add_interface_description {
        writer.write_pcapng_block(InterfaceDescriptionBlock {
            linktype: DataLink::IPV4,
            snaplen: 65535,
            options: vec![],
        })?;
    }

    Ok(writer)
}

pub struct PcapExporter {
    capture_start: Instant,
    total_tracked_packets: AtomicU64,
    keylog: Option<Arc<InMemoryKeyLog>>,
    tmp_writer: Mutex<PcapNgWriter<Box<dyn Write + Send + Sync + 'static>>>,
    node_id: Option<Arc<str>>,
}

impl PcapExporter {
    pub fn new(node_name: Arc<str>, keylog: Option<Arc<InMemoryKeyLog>>) -> anyhow::Result<Self> {
        let file_path = Self::tmp_path(&node_name);
        let file = File::create(&file_path)
            .with_context(|| format!("failed to open {} for writing", file_path.display()))?;
        let writer = pcap_writer(Box::new(BufWriter::new(file)), true)?;
        Ok(Self {
            capture_start: Instant::now(),
            tmp_writer: Mutex::new(writer),
            node_id: Some(node_name),
            total_tracked_packets: AtomicU64::new(0),
            keylog,
        })
    }

    pub fn for_node(
        node_id: Arc<str>,
        keylog: Option<Arc<InMemoryKeyLog>>,
    ) -> anyhow::Result<Self> {
        PcapExporter::new(node_id, keylog)
    }

    pub fn noop() -> Self {
        Self {
            capture_start: Instant::now(),
            tmp_writer: Mutex::new(Self::noop_writer()),
            node_id: None,
            total_tracked_packets: AtomicU64::new(0),
            keylog: None,
        }
    }

    fn finish(&self) -> anyhow::Result<()> {
        // Finish writing the tmp file
        let writer = mem::replace(&mut *self.tmp_writer.lock(), Self::noop_writer());
        let mut writer = writer.into_inner();
        writer.flush()?;
        drop(writer);

        // Now let's write the final file if a node id was available

        let Some(node_id) = &self.node_id else {
            // No node id, so no file
            return Ok(());
        };

        let tmp_path = Self::tmp_path(node_id);
        let final_path = Self::final_path(node_id);

        let Some(keylog) = &self.keylog else {
            // No keylog, so the original tmp file can be reused unmodified
            fs::rename(&tmp_path, &final_path)?;
            return Ok(());
        };

        // Copy tmp file to final file, but prepending the keylog block before anything else
        let tmp_file = File::open(&tmp_path)
            .with_context(|| format!("failed to open {}", tmp_path.display()))?;
        let mut tmp_reader = PcapNgReader::new(tmp_file)?;

        let final_file = File::create(&final_path)
            .with_context(|| format!("failed to open {}", final_path.display()))?;
        let mut final_writer = pcap_writer(Box::new(BufWriter::new(final_file)), false)?;

        Self::write_keylog_pcap_block(&mut final_writer, keylog)?;
        while let Some(block) = tmp_reader.next_raw_block() {
            final_writer.write_raw_block(&block?)?;
        }

        // Delete the tmp file
        drop(tmp_reader);
        fs::remove_file(&tmp_path)?;

        final_writer.into_inner().flush()?;

        Ok(())
    }

    fn noop_writer() -> PcapNgWriter<Box<dyn Write + Send + Sync + 'static>> {
        pcap_writer(Box::new(std::io::sink()), true).unwrap()
    }

    fn final_path(node_id: &str) -> PathBuf {
        format!("{node_id}.pcap").into()
    }

    fn tmp_path(node_id: &str) -> PathBuf {
        format!("{node_id}.pcap.tmp").into()
    }

    fn write_keylog_pcap_block(
        writer: &mut PcapNgWriter<Box<dyn Write + Send + Sync + 'static>>,
        keylog: &InMemoryKeyLog,
    ) -> anyhow::Result<()> {
        let mut secrets = keylog.log.lock().join("\n");
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

        block[length_offset..length_offset + 4].copy_from_slice(&total_length.to_be_bytes());

        let (_, raw_block) = RawBlock::from_slice::<byteorder::BigEndian>(&block)?;
        writer.write_raw_block(&raw_block)?;
        Ok(())
    }

    pub fn track_transmit(&self, transmit: &OwnedTransmit) {
        self.write_pcapng_transmit(transmit, Instant::now());
    }

    fn write_pcapng_transmit(&self, transmit: &OwnedTransmit, sent: Instant) {
        let IpAddr::V4(source) = transmit.source.ip() else {
            unreachable!()
        };

        let IpAddr::V4(destination) = transmit.destination.ip() else {
            unreachable!()
        };

        let mut buffer = vec![0; 2000];

        // Wrap the data in a UDP packet
        let mut udp_writer = MutableUdpPacket::new(&mut buffer).unwrap();
        let udp_packet_length = 8 + transmit.contents.len() as u16;
        udp_writer.set_source(transmit.source.port());
        udp_writer.set_destination(transmit.destination.port());
        udp_writer.set_length(udp_packet_length);
        udp_writer.set_payload(&transmit.contents);
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
        ip_writer.set_ttl(transmit.ttl);
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

        self.tmp_writer
            .lock()
            .write_pcapng_block(EnhancedPacketBlock {
                interface_id: 0,
                timestamp: correct_timestamp(sent - self.capture_start),
                original_len: ip_packet.len() as u32,
                data: ip_packet.into(),
                options: Vec::new(),
            })
            .unwrap();
    }
}

impl Drop for PcapExporter {
    fn drop(&mut self) {
        if let Err(e) = self.finish() {
            eprintln!("failed to finish pcap exporter: {e:?}");
        }
    }
}

fn correct_timestamp(d: Duration) -> Duration {
    // Round to the nearest millisecond
    let millis = (d.as_secs_f64() * 1000.0).round();

    // Return the time, an order of magnitude smaller (there seems to be a bug in the library we are
    // using, which multiplies seconds by 1000)
    Duration::from_secs_f64(millis / 1_000_000.0)
}
