use super::{
    event_loop_types::{FileFinishingEvEvent, FileWrkrEvLoopId},
    pending_files_map::PendingFilesMap,
    FileWriterHandle,
};
use faces_event_loop_utils::{ExtEventListener, PublicEventListener};
use std::{
    fs::{File, OpenOptions},
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    },
    time::Duration,
};

struct Fixture {
    path: PathBuf,
    writer: FileWriterHandle,
    pending: PendingFilesMap,
    events: crossbeam_channel::Receiver<FileFinishingEvEvent>,
}
impl Fixture {
    fn new(length: Option<usize>, read_only: bool) -> Self {
        let path = std::env::temp_dir().join(format!("fragment-writer-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, []).unwrap();
        let file = if read_only {
            File::open(&path).unwrap()
        } else {
            OpenOptions::new().write(true).open(&path).unwrap()
        };
        let (sender, events) = crossbeam_channel::unbounded();
        let listener = Arc::new(PublicEventListener(ExtEventListener::new(
            sender,
            FileWrkrEvLoopId::MainLoop,
        )));
        let pending = PendingFilesMap::new();
        let id = uuid::Uuid::new_v4();
        let writer = FileWriterHandle::new(file, id, &pending, 0, &listener, length);
        pending.insert_pending_writer(id, &writer);
        Self {
            path,
            writer,
            pending,
            events,
        }
    }
    fn finish_callback(&self) {
        let FileFinishingEvEvent::FinishingFileWrite { cb } =
            self.events.recv_timeout(Duration::from_secs(2)).unwrap();
        cb();
        // Drain the actual finishing queue, including the old implementation's
        // intermediate callback event. Assertions count user callbacks below.
        while let Ok(FileFinishingEvEvent::FinishingFileWrite { cb }) = self.events.try_recv() {
            cb();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[test]
fn flush_failure_is_sticky_and_never_calls_a_success_callback() {
    let fixture = Fixture::new(Some(1), true);
    fixture
        .writer
        .on_file_written(|_| panic!("failed flush reported as success"));
    assert!(fixture.writer.write_on_disk(&[7]).is_err());
    assert!(fixture.writer.is_file_written().is_err());
    assert!(fixture.writer.flush().is_err());
    assert!(fixture.writer.write_on_disk(&[8]).is_err());
    assert!(fixture.events.try_recv().is_err());
    assert_eq!(std::fs::metadata(&fixture.path).unwrap().len(), 0);
}

#[test]
fn write_failure_remains_visible_even_if_a_later_empty_flush_would_succeed() {
    let fixture = Fixture::new(Some(16384), true);
    assert!(fixture.writer.write_on_disk(&[1; 16384]).is_err());
    assert!(fixture.writer.is_file_written().is_err());
    assert!(fixture.writer.flush().is_err());
    assert_eq!(fixture.writer.written(), 0);
    fixture
        .writer
        .on_file_written(|_| panic!("failed write reported as success"));
    assert!(fixture.events.try_recv().is_err());
}

#[test]
fn callbacks_before_during_and_after_completion_run_once_outside_the_writer_lock() {
    for iteration in 0..64 {
        let fixture = Fixture::new(Some(3), false);
        let writer = fixture.writer.clone();
        let observed = Arc::new(AtomicUsize::new(0));
        let count = observed.clone();
        let inspect = writer.clone();
        let callback = move |length| {
            assert_eq!(length, 3);
            inspect.flush().unwrap();
            assert!(inspect.is_file_written().unwrap());
            count.fetch_add(1, Ordering::SeqCst);
        };
        match iteration % 3 {
            0 => {
                writer.on_file_written(callback);
                writer.write_on_disk(b"abc").unwrap();
            }
            1 => {
                writer.write_on_disk(b"abc").unwrap();
                writer.on_file_written(callback);
            }
            _ => {
                let barrier = Arc::new(Barrier::new(2));
                let ready = barrier.clone();
                let writing = writer.clone();
                let task = std::thread::spawn(move || {
                    ready.wait();
                    writing.write_on_disk(b"abc").unwrap();
                });
                barrier.wait();
                writer.on_file_written(callback);
                task.join().unwrap();
            }
        }
        fixture.finish_callback();
        assert_eq!(observed.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(&fixture.path).unwrap(), b"abc");
    }
}

#[test]
fn zero_bytes_and_a_length_learned_after_the_last_write_complete() {
    for bytes in [b"".as_slice(), b"abc".as_slice()] {
        let fixture = Fixture::new(None, false);
        let expected = bytes.len();
        let count = Arc::new(AtomicUsize::new(0));
        let completed = count.clone();
        fixture.writer.on_file_written(move |length| {
            assert_eq!(length, expected);
            completed.fetch_add(1, Ordering::SeqCst);
        });
        if !bytes.is_empty() {
            fixture.writer.write_on_disk(bytes).unwrap();
        }
        assert!(fixture.events.try_recv().is_err());
        fixture.writer.set_file_length(expected);
        fixture.finish_callback();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(&fixture.path).unwrap(), bytes);
    }
}

#[test]
fn prefix_failure_is_sticky_and_prefixing_preserves_all_bytes() {
    let fixture = Fixture::new(Some(6), false);
    fixture.writer.write_on_disk(b"def").unwrap();
    fixture
        .writer
        .transfert_bytes_from_temp_file(fixture.path.to_str().unwrap(), b"abc")
        .unwrap();
    fixture.writer.flush().unwrap();
    assert_eq!(fixture.writer.written(), 6);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"abcdef");
    let failed = Fixture::new(None, false);
    let missing = failed.path.with_extension("missing");
    assert!(failed
        .writer
        .transfert_bytes_from_temp_file(missing.to_str().unwrap(), b"abc")
        .is_err());
    assert!(failed.writer.flush().is_err());
    assert!(failed.writer.is_file_written().is_err());
    assert!(failed.writer.write_on_disk(b"z").is_err());
}

#[test]
fn closed_finishing_queue_does_not_lose_the_callback() {
    for finished_first in [false, true] {
        let mut fixture = Fixture::new(Some(3), false);
        let (_, unrelated_receiver) = crossbeam_channel::unbounded();
        drop(std::mem::replace(&mut fixture.events, unrelated_receiver));
        let count = Arc::new(AtomicUsize::new(0));
        let completed = count.clone();
        let inspect = fixture.writer.clone();
        let callback = move |length| {
            assert_eq!(length, 3);
            inspect.flush().unwrap();
            completed.fetch_add(1, Ordering::SeqCst);
        };
        if finished_first {
            fixture.writer.write_on_disk(b"abc").unwrap();
            fixture.writer.on_file_written(callback);
        } else {
            fixture.writer.on_file_written(callback);
            fixture.writer.write_on_disk(b"abc").unwrap();
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn terminal_writers_leave_the_pending_registry_even_without_a_callback() {
    for read_only in [false, true] {
        let fixture = Fixture::new(Some(1), read_only);
        assert_eq!(fixture.writer.write_on_disk(b"a").is_err(), read_only);
        assert!(!fixture
            .pending
            .yield_writer_by_id(fixture.writer.get_file_writer_id()));
    }
}

#[test]
fn prefixing_and_queued_appends_preserve_byte_order_under_concurrency() {
    for _ in 0..64 {
        let fixture = Fixture::new(None, false);
        fixture.writer.write_on_disk(b"middle").unwrap();
        let ready = Arc::new(Barrier::new(2));
        let go = ready.clone();
        let writer = fixture.writer.clone();
        let path = fixture.path.clone();
        let prefix = std::thread::spawn(move || {
            go.wait();
            writer
                .transfert_bytes_from_temp_file(path.to_str().unwrap(), b"prefix")
                .unwrap();
        });
        ready.wait();
        fixture.writer.write_on_disk(b"suffix").unwrap();
        prefix.join().unwrap();
        fixture.writer.set_file_length(18);
        fixture.writer.flush().unwrap();
        assert_eq!(fixture.writer.written(), 18);
        assert_eq!(std::fs::read(&fixture.path).unwrap(), b"prefixmiddlesuffix");
    }
}

#[test]
fn length_overrun_fails_and_completed_bytes_cannot_be_modified() {
    let fixture = Fixture::new(Some(2), false);
    assert!(fixture.writer.write_on_disk(b"abc").is_err());
    assert!(fixture.writer.flush().is_err());
    assert!(fixture.writer.is_file_written().is_err());
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"");

    let complete = Fixture::new(Some(3), false);
    complete.writer.write_on_disk(b"abc").unwrap();
    assert!(complete.writer.write_on_disk(b"d").is_err());
    assert!(complete
        .writer
        .transfert_bytes_from_temp_file(complete.path.to_str().unwrap(), b"z")
        .is_err());
    assert_eq!(complete.writer.written(), 3);
    assert!(complete.writer.is_file_written().unwrap());
    assert_eq!(std::fs::read(&complete.path).unwrap(), b"abc");
}

#[test]
fn fin_must_match_the_declared_length_even_if_all_received_bytes_were_written() {
    let fixture = Fixture::new(Some(3), false);
    fixture.writer.write_on_disk(b"ab").unwrap();
    assert_eq!(fixture.writer.written(), 2);
    fixture.writer.finish_received(2);
    assert!(fixture.writer.flush().is_err());
    assert!(fixture.writer.is_file_written().is_err());
    fixture
        .writer
        .on_file_written(|_| panic!("truncated request reported as complete"));
    assert!(fixture.events.try_recv().is_err());
}
