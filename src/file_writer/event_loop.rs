use std::{
    sync::{
        atomic::{AtomicBool, AtomicU8},
        Arc,
    },
    thread::JoinHandle,
};

use dashmap::DashMap;

use crate::file_writer::event_loop_types::{FileWriterListener, FileWrkrEvEvent};

type Chan = (
    crossbeam_channel::Sender<FileWrkrEvEvent>,
    crossbeam_channel::Receiver<FileWrkrEvEvent>,
);

pub struct FileWriterPool {
    stream_map: DashMap<(u64, String), usize>,
    atomic_switch: AtomicBool,
    workers_amount: usize,
    current_channel_available: AtomicU8,
    workers: Vec<(Chan, FileWriterWorker)>,
}

impl FileWriterPool {
    pub fn new(
        workers_amount: usize,
        manage_event: impl Fn(FileWrkrEvEvent) + Send + Sync + 'static,
    ) -> Self {
        let m_ev = Arc::new(manage_event);
        let mut workers: Vec<(Chan, FileWriterWorker)> = (0..workers_amount)
            .into_iter()
            .map(|i| {
                let chan = crossbeam_channel::unbounded();

                (chan.clone(), FileWriterWorker::new(chan.1, &m_ev))
            })
            .collect();

        Self {
            stream_map: DashMap::new(),
            atomic_switch: AtomicBool::new(false),
            workers_amount,
            current_channel_available: AtomicU8::new(0),
            workers,
        }
    }
    pub fn get_worker_listener_by_index(&self, index: usize) -> Option<FileWriterListener> {
        if let Some(w) = self.workers.get(index) {
            Some(FileWriterListener::new(&w.0 .0))
        } else {
            None
        }
    }
    pub fn get_worker_index_for_this_stream(
        &self,
        stream_id: u64,
        conn_id: String,
    ) -> Option<usize> {
        if let Some(entry) = self.stream_map.get(&(stream_id, conn_id)) {
            let index = *entry;
            return Some(index);
        }
        None
    }
    pub fn associate_stream_with_next_listener(&self, stream_id: u64, conn_id: String) {
        let key = (stream_id, conn_id);
        if self.stream_map.contains_key(&key) {
            return;
        }
        let worker_index = self
            .current_channel_available
            .load(std::sync::atomic::Ordering::Relaxed);
        self.current_channel_available.store(
            (worker_index + 1) % self.workers_amount as u8,
            std::sync::atomic::Ordering::Relaxed,
        );
        self.stream_map.entry(key).or_insert(worker_index as usize);
    }
    pub fn remove_completed_stream_from_map(&self, stream_id: u64, conn_id: String) {
        self.stream_map.remove(&(stream_id, conn_id));
    }
}

pub struct FileWriterWorker {
    handle: JoinHandle<()>,
}

impl FileWriterWorker {
    pub fn new(
        receiver: crossbeam_channel::Receiver<FileWrkrEvEvent>,
        manage_ev: &Arc<impl Fn(FileWrkrEvEvent) + Send + Sync + 'static>,
    ) -> Self {
        let manage_event_cb = manage_ev.clone();
        let h = std::thread::spawn(move || {
            while let Ok(msg) = receiver.recv() {
                (manage_event_cb)(msg)
            }
        });
        Self { handle: h }
    }
}
