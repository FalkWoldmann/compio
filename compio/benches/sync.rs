//! Benchmarks for the code paths that rely on channels, oneshots and events:
//! blocking-pool completions, the dispatcher, actor mailboxes and cancel
//! tokens.

use std::{convert::Infallible, num::NonZeroUsize};

use compio::{
    actor::{Actor, Call, Cluster, Handler, Mailbox},
    dispatcher::Dispatcher,
    driver::{DriverType, ProactorBuilder},
    runtime::{CancelToken, FutureExt, Runtime},
};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use futures_util::future::join_all;

criterion_group!(sync, spawn_blocking, dispatcher, actor, cancel);
criterion_main!(sync);

const BATCH: usize = 64;

fn runtimes() -> Vec<(&'static str, Runtime)> {
    let mut rts = vec![];
    for (name, ty) in [
        ("io-uring", DriverType::IoUring),
        ("poll", DriverType::Poll),
    ] {
        let mut proactor = ProactorBuilder::new();
        proactor.driver_type(ty);
        let rt = Runtime::builder().with_proactor(proactor).build().unwrap();
        assert_eq!(rt.driver_type(), ty);
        rts.push((name, rt));
    }
    rts
}

fn spawn_blocking(c: &mut Criterion) {
    let mut group = c.benchmark_group("spawn_blocking");
    for (name, rt) in runtimes() {
        group.throughput(Throughput::Elements(1));
        group.bench_function(format!("{name}/single"), |b| {
            b.to_async(&rt)
                .iter(|| async { compio::runtime::spawn_blocking(|| 1usize).await.unwrap() })
        });
        group.throughput(Throughput::Elements(BATCH as u64));
        group.bench_function(format!("{name}/batch"), |b| {
            b.to_async(&rt).iter(|| async {
                join_all((0..BATCH).map(|i| compio::runtime::spawn_blocking(move || i))).await
            })
        });
    }
    group.finish();
}

fn dispatcher(c: &mut Criterion) {
    let mut group = c.benchmark_group("dispatcher");
    let rt = Runtime::new().unwrap();
    let dispatcher = Dispatcher::builder()
        .worker_threads(NonZeroUsize::new(2).unwrap())
        .build()
        .unwrap();
    group.throughput(Throughput::Elements(1));
    group.bench_function("single", |b| {
        b.to_async(&rt).iter(|| async {
            dispatcher
                .dispatch(|| async { 1usize })
                .unwrap()
                .await
                .unwrap()
        })
    });
    group.throughput(Throughput::Elements(BATCH as u64));
    group.bench_function("batch", |b| {
        b.to_async(&rt).iter(|| async {
            let rxs = (0..BATCH)
                .map(|i| dispatcher.dispatch(move || async move { i }).unwrap())
                .collect::<Vec<_>>();
            join_all(rxs).await
        })
    });
    group.finish();
    rt.block_on(dispatcher.join()).unwrap();
}

struct Counter;

struct Add(usize);

#[derive(Debug)]
struct Read;

impl Actor for Counter {
    type Arguments = ();
    type Error = Infallible;
    type State = usize;

    async fn pre_start(&self, _: &Mailbox<Self>, _: ()) -> Result<usize, Infallible> {
        Ok(0)
    }
}

impl Handler<Add> for Counter {
    async fn handle(
        &self,
        _: &Mailbox<Self>,
        Add(v): Add,
        s: &mut usize,
    ) -> Result<(), Infallible> {
        *s += v;
        Ok(())
    }
}

impl Handler<Call<Read, usize>> for Counter {
    async fn handle(
        &self,
        _: &Mailbox<Self>,
        call: Call<Read, usize>,
        s: &mut usize,
    ) -> Result<(), Infallible> {
        call.reply(*s).ok();
        Ok(())
    }
}

fn actor(c: &mut Criterion) {
    let mut group = c.benchmark_group("actor");
    let rt = Runtime::new().unwrap();
    let cluster = rt.block_on(async { Cluster::new().unwrap() });
    let counter = rt.block_on(async {
        cluster
            .spawn(|| Counter, ())
            .with_capacity(NonZeroUsize::new(1024).unwrap())
            .await
            .unwrap()
            .0
    });
    group.throughput(Throughput::Elements(1));
    group.bench_function("call", |b| {
        b.to_async(&rt)
            .iter(|| async { counter.call(Read).await.unwrap() })
    });
    group.throughput(Throughput::Elements(BATCH as u64 + 1));
    group.bench_function("send_batch_then_call", |b| {
        b.to_async(&rt).iter(|| async {
            for _ in 0..BATCH {
                counter.send(Add(1)).ok().unwrap();
            }
            counter.call(Read).await.unwrap()
        })
    });
    group.finish();
    counter.stop();
    rt.block_on(cluster.join()).unwrap();
}

fn cancel(c: &mut Criterion) {
    let mut group = c.benchmark_group("cancel");
    let rt = Runtime::new().unwrap();
    group.bench_function("with_cancel_ready", |b| {
        b.to_async(&rt).iter(|| async {
            let token = CancelToken::new();
            std::future::ready(1usize).with_cancel(token).await
        })
    });
    group.bench_function("fail_fast_ready", |b| {
        b.to_async(&rt).iter(|| async {
            let token = CancelToken::new();
            std::future::ready(1usize)
                .with_cancel(token)
                .fail_fast()
                .await
                .unwrap()
        })
    });
    group.bench_function("fail_fast_cancel_pending", |b| {
        b.to_async(&rt).iter(|| async {
            let token = CancelToken::new();
            let mut fut = std::pin::pin!(
                std::future::pending::<()>()
                    .with_cancel(token.clone())
                    .fail_fast()
            );
            std::future::poll_fn(|cx| {
                assert!(fut.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            token.cancel();
            fut.await.unwrap_err()
        })
    });
    group.bench_function("cancel_then_wait", |b| {
        b.to_async(&rt).iter(|| async {
            let token = CancelToken::new();
            let wait = token.clone().wait();
            token.cancel();
            wait.await
        })
    });
    group.finish();
}
