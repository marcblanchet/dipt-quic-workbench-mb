use crate::config::quinn::QuinnJsonConfig;
use crate::config::traffic::QuicRequestResponseTraffic;
use anyhow::{Context, bail};
use async_lock::Semaphore;
use fastrand::Rng;
use in_memory_network::async_rt;
use in_memory_network::async_rt::time::Instant;
use in_memory_network::pcap_exporter::InMemoryKeyLog;
use in_memory_network::quinn_interop::InMemoryUdpSocket;
use parking_lot::Mutex;
use quinn::Endpoint;
use quinn_proto::crypto::rustls::QuicClientConfig;
use quinn_proto::{ClientConfig, VarInt};
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use std::fs::File;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

pub async fn run_traffic_pattern(
    traffic: QuicRequestResponseTraffic,
    simulation_start: Instant,
    client: Endpoint,
    log_writer: Arc<Mutex<dyn Write + Sync + Send>>,
) -> anyhow::Result<usize> {
    let max_connections = b'Z' - b'A';
    if traffic.concurrent_connections > max_connections as u32 {
        bail!(
            "The maximum number of concurrent connections is {max_connections}, but {} were configured",
            traffic.concurrent_connections
        );
    }

    // Don't start until the specified moment
    let time_until_start =
        Duration::from_millis(traffic.start_at_ms).saturating_sub(simulation_start.elapsed());
    if !time_until_start.is_zero() {
        async_rt::time::sleep(time_until_start).await;
    }

    // Make requests, potentially using concurrent connections
    let connections_semaphore = Arc::new(Semaphore::new(traffic.concurrent_connections as usize));
    let mut connection_tasks = Vec::new();
    let requests_left = Arc::new(Mutex::new(traffic.requests));
    for i in 0..traffic.concurrent_connections {
        let client = client.clone();
        let requests_left = requests_left.clone();
        let request_interval = Duration::from_millis(traffic.request_interval_ms);
        let connection_name = (i as u8 + b'A') as char;
        let connections_semaphore = connections_semaphore.clone();
        let concurrent_streams = traffic.concurrent_streams_per_connection;
        let log_writer = log_writer.clone();
        connection_tasks.push(async_rt::spawn(async move {
            let _permit = connections_semaphore.acquire().await;
            run_connection(
                client,
                traffic.server,
                connection_name.to_string(),
                requests_left,
                request_interval,
                concurrent_streams,
                simulation_start,
                log_writer,
            )
            .await
        }));

        // Wait 1 ms before starting the next connection
        async_rt::time::sleep(Duration::from_millis(1)).await;
    }

    let total_connections = connection_tasks.len();
    for task in connection_tasks {
        task.await
            .context("client connection task crashed")?
            .context("client connection errored")?;
    }

    let total_time_sec = simulation_start.elapsed().as_secs_f64();
    _ = writeln!(
        log_writer.lock(),
        "{:.2}s All connections closed",
        total_time_sec
    );

    Ok(total_connections)
}

pub async fn run_connection(
    client: Endpoint,
    server_addr: SocketAddr,
    connection_name: String,
    requests_left: Arc<Mutex<u32>>,
    request_interval: Duration,
    concurrent_streams: u32,
    start: Instant,
    log_writer: Arc<Mutex<dyn Write + Sync + Send>>,
) -> anyhow::Result<()> {
    _ = writeln!(
        log_writer.lock(),
        "{:.2}s CONNECT (conn = {connection_name})",
        start.elapsed().as_secs_f64()
    );
    let connection = client
        .connect(server_addr, "server-name")
        .context("failed to start connecting to server")?
        .await
        .context("client failed to connect to server")?;
    _ = writeln!(
        log_writer.lock(),
        "{:.2}s CONNECTED (conn = {connection_name}){}",
        start.elapsed().as_secs_f64(),
        if connection.extended_key_update_negotiated() {
            " [extended key update negotiated]"
        } else {
            ""
        },
    );

    let requests_semaphore = Arc::new(Semaphore::new(concurrent_streams as usize));
    let mut request_tasks = Vec::new();
    let mut requests_made = 0;
    loop {
        // Break once there are no more requests left to make
        {
            let mut requests_left = requests_left.lock();
            if *requests_left == 0 {
                break;
            }

            *requests_left -= 1;
        }

        let permit = requests_semaphore.clone().acquire_arc().await;
        if requests_made > 0 && !request_interval.is_zero() {
            _ = writeln!(
                log_writer.lock(),
                "{:.2}s SLEEP for {} ms (connection = {connection_name})",
                start.elapsed().as_secs_f64(),
                request_interval.as_millis()
            );
            async_rt::time::sleep(request_interval).await;
        }

        requests_made += 1;

        // Actually make the request
        let connection = connection.clone();
        let connection_name = connection_name.clone();
        let log_writer_cp = log_writer.clone();
        let request_task = async_rt::spawn(async move {
            let request = "GET /index.html";
            _ = writeln!(
                log_writer_cp.lock(),
                "{:.2}s {request} (stream = {connection_name}{requests_made})",
                start.elapsed().as_secs_f64()
            );

            let (mut tx, mut rx) = connection.open_bi().await?;
            tx.write_all(request.as_bytes()).await?;
            tx.finish()?;

            rx.read_to_end(usize::MAX).await.with_context(|| {
                format!(
                    "failed to read response from server at {:.2}s",
                    start.elapsed().as_secs_f64()
                )
            })?;

            drop(permit);
            Result::<_, anyhow::Error>::Ok(())
        });

        request_tasks.push(request_task);
    }

    for task in request_tasks {
        task.await
            .context("client stream task crashed")?
            .context("client stream task errored")?;
    }

    _ = writeln!(
        log_writer.lock(),
        "{:.2}s DONE (conn = {connection_name}, request/response amount = {requests_made})",
        start.elapsed().as_secs_f64()
    );

    connection.close(VarInt::from_u32(0), &[]);
    Ok(())
}

pub fn client_endpoint(
    start: Instant,
    keylog: Arc<InMemoryKeyLog>,
    server_cert: CertificateDer<'_>,
    client_socket: InMemoryUdpSocket,
    quinn_config: &QuinnJsonConfig,
    quinn_rng: &mut Rng,
) -> anyhow::Result<Endpoint> {
    let mut seed = [0; 32];
    quinn_rng.fill(&mut seed);

    let qlog_file = File::create(format!("{}.qlog", client_socket.node_id()))?;
    let endpoint = Endpoint::new_with_abstract_socket(
        crate::quic::endpoint_config(seed),
        None,
        Box::new(client_socket),
        async_rt::active_rt(),
    )
    .context("failed to create client endpoint")?;

    endpoint.set_default_client_config(client_config(
        start,
        keylog,
        server_cert,
        quinn_config,
        qlog_file,
    )?);

    Ok(endpoint)
}

fn client_config(
    start: Instant,
    keylog: Arc<InMemoryKeyLog>,
    server_cert: CertificateDer<'_>,
    quinn_config: &QuinnJsonConfig,
    qlog_file: File,
) -> anyhow::Result<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.add(server_cert)?;

    let default_provider = rustls::crypto::ring::default_provider();
    let provider = rustls::crypto::CryptoProvider {
        cipher_suites: vec![rustls::crypto::ring::cipher_suite::TLS13_AES_128_GCM_SHA256],
        ..default_provider
    };

    let mut crypto = rustls::ClientConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();

    crypto.key_log = keylog;

    // Offer the QUIC/TLS Extended Key Update extension if enabled.
    if quinn_config.extended_key_update.unwrap_or(false) {
        crypto.extended_key_update = true;
    }

    let mut client_config = ClientConfig::new(Arc::new(QuicClientConfig::try_from(crypto)?));
    client_config.transport_config(Arc::new(crate::quic::transport_config(
        start,
        quinn_config,
        qlog_file,
    )));

    Ok(client_config)
}
