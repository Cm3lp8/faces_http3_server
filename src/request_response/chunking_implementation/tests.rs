use super::{run_worker, ChunkableBody, CHUNK_SIZE};
use crate::request_response::{ChunksDispatchChannel, QueuedRequest};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use std::{
    io::{self, BufRead, Read},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const SCID: &[u8] = b"chunk-worker-fixture";
const QUEUE_LIMIT: usize = 150;
const TIMEOUT: Duration = Duration::from_secs(3);

struct ReadPause {
    entered: Sender<()>,
    resume: Receiver<()>,
}

fn pause_first_read() -> (ReadPause, Receiver<()>, Sender<()>) {
    let (entered, observed) = crossbeam_channel::bounded(1);
    let (release, resume) = crossbeam_channel::bounded(1);
    (ReadPause { entered, resume }, observed, release)
}

struct ObservedReader {
    inner: Box<dyn BufRead + Send + Sync>,
    pause: Option<ReadPause>,
    bytes_read: Arc<AtomicUsize>,
    // Dropped after the actual reader and its backing data.
    _alive: Sender<()>,
}

impl Read for ObservedReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(out)?;
        self.bytes_read.fetch_add(n, Ordering::Relaxed);
        if let Some(pause) = self.pause.take() {
            pause.entered.send(()).unwrap();
            pause
                .resume
                .recv_timeout(TIMEOUT)
                .expect("test did not release the reader");
        }
        Ok(n)
    }
}

impl BufRead for ObservedReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
        self.bytes_read.fetch_add(amount, Ordering::Relaxed);
    }
}

struct ReaderProbe {
    bytes_read: Arc<AtomicUsize>,
    alive: Receiver<()>,
}

impl ReaderProbe {
    fn assert_released(&self) {
        assert_eq!(
            self.alive.recv_timeout(TIMEOUT),
            Err(RecvTimeoutError::Disconnected),
            "worker retained the response reader"
        );
    }
}

struct Worker {
    poll: mio::Poll,
    jobs: Sender<ChunkableBody>,
    queues: ChunksDispatchChannel,
}

impl Worker {
    fn new() -> Self {
        let poll = mio::Poll::new().unwrap();
        let waker = Arc::new(mio::Waker::new(poll.registry(), mio::Token(0)).unwrap());
        let (jobs, receiver) = crossbeam_channel::unbounded();
        let queues = ChunksDispatchChannel::new();
        run_worker(
            &Arc::new(Mutex::new(Duration::ZERO)),
            &waker,
            receiver,
            jobs.clone(),
            Arc::default(),
            queues.clone(),
        );
        Self { poll, jobs, queues }
    }

    fn body(
        &self,
        stream: u64,
        data: Vec<u8>,
        pause: Option<ReadPause>,
    ) -> (ChunkableBody, ReaderProbe) {
        self.queues.insert_new_channel(stream, SCID);
        let sender = self.queues.get_low_priority_sender(stream, SCID).unwrap();
        let mut body =
            ChunkableBody::new_data(sender, stream, SCID, "fixture".into(), data, None).unwrap();
        let (keep_alive, alive) = crossbeam_channel::bounded(0);
        let bytes_read = Arc::new(AtomicUsize::new(0));
        body.reader = Box::new(ObservedReader {
            inner: body.reader,
            pause,
            bytes_read: bytes_read.clone(),
            _alive: keep_alive,
        });
        (body, ReaderProbe { bytes_read, alive })
    }

    fn enqueue(&self, stream: u64, data: Vec<u8>) -> ReaderProbe {
        let (body, probe) = self.body(stream, data, None);
        self.jobs.send(body).unwrap();
        probe
    }

    fn wait_for_work(&mut self, deadline: Instant) {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("chunk worker deadline");
        self.poll
            .poll(&mut mio::Events::with_capacity(1), Some(remaining))
            .unwrap();
    }

    fn wait_for_full_queue(&mut self, stream: u64, reader: &ReaderProbe) {
        let deadline = Instant::now() + TIMEOUT;
        while self.queues.in_queue(stream, SCID).unwrap() != QUEUE_LIMIT {
            self.wait_for_work(deadline);
        }
        assert_eq!(
            reader.bytes_read.load(Ordering::Relaxed),
            QUEUE_LIMIT * CHUNK_SIZE
        );
        assert!(matches!(
            reader.alive.try_recv(),
            Err(crossbeam_channel::TryRecvError::Empty)
        ));
    }

    fn receive_all(&mut self, stream: u64, expected: &[u8]) {
        let deadline = Instant::now() + TIMEOUT;
        let mut offset = 0;
        let mut sequence = 0;
        while offset < expected.len() {
            let Ok(request) = self.queues.try_pop(stream, SCID) else {
                self.wait_for_work(deadline);
                continue;
            };
            let QueuedRequest::Body(mut chunk) = request else {
                panic!("expected body data");
            };
            assert_eq!(chunk.stream_id(), stream);
            assert_eq!(chunk.packet_id(), sequence);
            let bytes = chunk.take_data();
            assert!(!bytes.is_empty());
            let end = offset + bytes.len();
            assert!(
                expected.get(offset..end) == Some(bytes.as_slice()),
                "wrong payload at offset {offset}"
            );
            assert_eq!(chunk.is_end(), end == expected.len());
            offset = end;
            sequence += 1;
        }
    }
}

fn large_payload() -> Vec<u8> {
    (0..(QUEUE_LIMIT + 8) * CHUNK_SIZE + 3)
        .map(|index| (index % 251) as u8)
        .collect()
}

#[test]
fn removing_a_full_stream_releases_its_reader_and_other_streams_deliver() {
    let mut worker = Worker::new();
    let abandoned = worker.enqueue(0, large_payload());
    worker.wait_for_full_queue(0, &abandoned);
    worker.queues.remove_stream(0, SCID);
    let healthy = worker.enqueue(4, b"still active".to_vec());
    worker.receive_all(4, b"still active");
    healthy.assert_released();
    abandoned.assert_released();
    assert_eq!(
        abandoned.bytes_read.load(Ordering::Relaxed),
        QUEUE_LIMIT * CHUNK_SIZE
    );
}

#[test]
fn draining_a_full_live_stream_resumes_the_reader_without_losing_bytes() {
    let mut worker = Worker::new();
    let expected = large_payload();
    let reader = worker.enqueue(0, expected.clone());
    worker.wait_for_full_queue(0, &reader);
    worker.receive_all(0, &expected);
    reader.assert_released();
    assert_eq!(reader.bytes_read.load(Ordering::Relaxed), expected.len());
}

#[test]
fn removing_a_stream_before_its_job_starts_skips_the_entire_reader() {
    let worker = Worker::new();
    let (body, reader) = worker.body(0, large_payload(), None);
    worker.queues.remove_stream(0, SCID);
    worker.jobs.send(body).unwrap();
    reader.assert_released();
    assert_eq!(reader.bytes_read.load(Ordering::Relaxed), 0);
}

#[test]
fn disconnect_during_read_drops_the_job_on_send_error_without_requeue() {
    let mut worker = Worker::new();
    let (pause, entered, release) = pause_first_read();
    let (body, reader) = worker.body(0, large_payload(), Some(pause));
    worker.jobs.send(body).unwrap();
    entered.recv_timeout(TIMEOUT).unwrap();
    // Read passed the worker's connectivity and capacity checks. The first
    // chunk is in its buffer now, but it has not been sent.
    worker.queues.remove_stream(0, SCID);

    // Put a witness at the head of the job queue. While it is reading, an
    // incorrectly rescheduled body would still be alive behind this job.
    let (pause, witness_entered, release_witness) = pause_first_read();
    let (witness, healthy) = worker.body(4, b"next stream".to_vec(), Some(pause));
    worker.jobs.send(witness).unwrap();
    release.send(()).unwrap();
    witness_entered.recv_timeout(TIMEOUT).unwrap();
    let original_lifetime = reader.alive.try_recv();
    release_witness.send(()).unwrap();
    assert_eq!(
        original_lifetime,
        Err(crossbeam_channel::TryRecvError::Disconnected),
        "failed send retained or requeued the reader"
    );
    assert_eq!(reader.bytes_read.load(Ordering::Relaxed), CHUNK_SIZE);
    worker.receive_all(4, b"next stream");
    healthy.assert_released();
}
