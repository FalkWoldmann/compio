//! Compares io_uring ring setups on latency-bound workloads.
//!
//! Each workload runs once per ring setup, so the setups are compared within a
//! single run:
//!
//! - `plain`: no task-run flags.
//! - `coop`: `COOP_TASKRUN | TASKRUN_FLAG`.
//! - `defer`: `SINGLE_ISSUER | DEFER_TASKRUN | TASKRUN_FLAG`.
//!
//! The workloads exchange small messages, so the cost of getting completions
//! back to the runtime dominates rather than copying data.

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        net::Ipv4Addr,
        time::{Duration, Instant},
    };

    use compio::{
        driver::{DriverType, ProactorBuilder},
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream, UdpSocket},
        runtime::{Runtime, spawn, spawn_blocking},
    };
    use criterion::{BenchmarkId, Criterion, Throughput};

    const MESSAGE_SIZE: usize = 64;
    const CONNECTIONS: usize = 16;

    fn setups() -> [(&'static str, ProactorBuilder); 3] {
        let mut plain = ProactorBuilder::new();
        plain
            .driver_type(DriverType::IoUring)
            .single_issuer(false)
            .coop_taskrun(false)
            .taskrun_flag(false)
            .defer_taskrun(false);

        let mut coop = plain.clone();
        coop.coop_taskrun(true).taskrun_flag(true);

        let mut defer = plain.clone();
        defer
            .single_issuer(true)
            .defer_taskrun(true)
            .taskrun_flag(true);

        [("plain", plain), ("coop", coop), ("defer", defer)]
    }

    fn runtime(builder: &ProactorBuilder) -> Runtime {
        let runtime = Runtime::builder()
            .with_proactor(builder.clone())
            .build()
            .unwrap();
        assert!(runtime.driver_type().is_iouring());
        runtime
    }

    async fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (client, (server, _)) =
            futures_util::try_join!(TcpStream::connect(addr), listener.accept()).unwrap();
        client.set_nodelay(true).unwrap();
        server.set_nodelay(true).unwrap();
        (client, server)
    }

    /// Sends `rounds` messages from `client` to `server` and back.
    async fn tcp_ping_pong(mut client: TcpStream, mut server: TcpStream, rounds: u64) {
        let echo = spawn(async move {
            let mut buf = vec![0u8; MESSAGE_SIZE];
            for _ in 0..rounds {
                (_, buf) = server.read_exact(buf).await.unwrap();
                (_, buf) = server.write_all(buf).await.unwrap();
            }
        });
        let mut buf = vec![0u8; MESSAGE_SIZE];
        for _ in 0..rounds {
            (_, buf) = client.write_all(buf).await.unwrap();
            (_, buf) = client.read_exact(buf).await.unwrap();
        }
        echo.await.unwrap();
    }

    fn tcp(c: &mut Criterion) {
        let mut group = c.benchmark_group("iouring/tcp_ping_pong");
        group.throughput(Throughput::Elements(1));
        for (name, builder) in setups() {
            let runtime = runtime(&builder);
            group.bench_function(BenchmarkId::new(name, 1), |b| {
                b.to_async(&runtime).iter_custom(|iter| async move {
                    let (client, server) = tcp_pair().await;
                    let start = Instant::now();
                    tcp_ping_pong(client, server, iter).await;
                    start.elapsed()
                })
            });
        }
        group.finish();

        let mut group = c.benchmark_group("iouring/tcp_ping_pong");
        group.throughput(Throughput::Elements(CONNECTIONS as u64));
        for (name, builder) in setups() {
            let runtime = runtime(&builder);
            group.bench_function(BenchmarkId::new(name, CONNECTIONS), |b| {
                b.to_async(&runtime).iter_custom(|iter| async move {
                    let mut pairs = Vec::with_capacity(CONNECTIONS);
                    for _ in 0..CONNECTIONS {
                        pairs.push(tcp_pair().await);
                    }
                    let start = Instant::now();
                    let tasks = pairs
                        .into_iter()
                        .map(|(client, server)| spawn(tcp_ping_pong(client, server, iter)))
                        .collect::<Vec<_>>();
                    for task in tasks {
                        task.await.unwrap();
                    }
                    start.elapsed()
                })
            });
        }
        group.finish();
    }

    fn udp(c: &mut Criterion) {
        let mut group = c.benchmark_group("iouring/udp_ping_pong");
        group.throughput(Throughput::Elements(1));
        for (name, builder) in setups() {
            let runtime = runtime(&builder);
            group.bench_function(name, |b| {
                b.to_async(&runtime).iter_custom(|iter| async move {
                    let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
                    let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
                    client.connect(server.local_addr().unwrap()).await.unwrap();
                    server.connect(client.local_addr().unwrap()).await.unwrap();

                    let start = Instant::now();
                    let echo = spawn(async move {
                        let mut buf = Vec::with_capacity(MESSAGE_SIZE);
                        for _ in 0..iter {
                            (_, buf) = server.recv(buf).await.unwrap();
                            (_, buf) = server.send(buf).await.unwrap();
                        }
                    });
                    let mut buf = vec![0u8; MESSAGE_SIZE];
                    for _ in 0..iter {
                        (_, buf) = client.send(buf).await.unwrap();
                        buf.clear();
                        (_, buf) = client.recv(buf).await.unwrap();
                    }
                    echo.await.unwrap();
                    start.elapsed()
                })
            });
        }
        group.finish();
    }

    /// Round trips through the blocking thread pool, which wakes the ring from
    /// another thread.
    fn blocking(c: &mut Criterion) {
        let mut group = c.benchmark_group("iouring/spawn_blocking");
        group.throughput(Throughput::Elements(1));
        for (name, builder) in setups() {
            let runtime = runtime(&builder);
            group.bench_function(name, |b| {
                b.to_async(&runtime).iter_custom(|iter| async move {
                    let start = Instant::now();
                    for _ in 0..iter {
                        spawn_blocking(|| ()).await.unwrap();
                    }
                    start.elapsed()
                })
            });
        }
        group.finish();
    }

    pub fn benches() {
        let mut c = Criterion::default()
            .configure_from_args()
            .warm_up_time(Duration::from_secs(1))
            .measurement_time(Duration::from_secs(3));
        tcp(&mut c);
        udp(&mut c);
        blocking(&mut c);
        c.final_summary();
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    linux::benches();
}
