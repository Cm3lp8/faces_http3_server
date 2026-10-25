pub use file_wrtr::{FileWriter, FileWriterChannel};
pub use trait_writable::FileWritable;
pub use writable_type::{FileWriterHandle, WritableItem};
mod event_loop;
mod event_loop_cb;
mod event_loop_types;
mod file_finishing_ev_loop;
mod pending_files_map;

/// [`FileWriter`] count on 'content-length' header send by client for finishing writing process
mod file_wrtr {
    use std::{fs::File, sync::Arc};

    use crossbeam_channel::SendError;
    use faces_event_loop_utils::{EventLoop, PublicEventListener};
    use uuid::Uuid;

    use crate::{
        file_writer::{
            event_loop::FileWriterPool,
            event_loop_cb::manage_event,
            event_loop_types::{
                FileFinishingEvEvent, FileWriterListener, FileWrkrEvEvent, FileWrkrEvLoopId,
            },
            file_finishing_ev_loop::manage_file_finishing_ev,
            pending_files_map::PendingFilesMap,
        },
        FileWriterHandle,
    };

    use super::{trait_writable::FileWritable, WritableItem};

    pub struct FileWriterChannel {
        sender: crossbeam_channel::Sender<WritableItem>,
    }
    impl Clone for FileWriterChannel {
        fn clone(&self) -> Self {
            Self {
                sender: self.sender.clone(),
            }
        }
    }
    impl FileWriterChannel {
        pub fn send(&self, writable_item: WritableItem) -> Result<(), SendError<WritableItem>> {
            self.sender.send(writable_item)
        }
    }
    #[derive(Clone)]
    pub struct FileWriter {
        file_worker_pool: Arc<FileWriterPool>,
        file_finishing_worker_pool: Arc<EventLoop<(), FileFinishingEvEvent, FileWrkrEvLoopId>>,
        file_finishing_listener: Arc<PublicEventListener<FileFinishingEvEvent, FileWrkrEvLoopId>>,
        pending_files_map: PendingFilesMap,
    }

    impl FileWriter {
        pub fn new() -> Self {
            let pending_files_map = PendingFilesMap::new();

            let file_writer_worker_pool = FileWriterPool::new(8, manage_event);
            let file_finishing_worker_pool =
                EventLoop::new(FileWrkrEvLoopId::MainLoop).set_thread_count(8);
            let file_finishing_listener = file_finishing_worker_pool.get_event_listener();
            file_finishing_worker_pool.run(&(), manage_file_finishing_ev);

            Self {
                file_worker_pool: Arc::new(file_writer_worker_pool),
                file_finishing_worker_pool: Arc::new(file_finishing_worker_pool),
                file_finishing_listener,
                pending_files_map,
            }
        }
        pub fn associate_stream_with_next_listener(&self, stream_id: u64, conn_id: String) {
            self.file_worker_pool
                .associate_stream_with_next_listener(stream_id, conn_id);
        }
        pub fn create_file_writer_handle(
            &self,
            file: std::fs::File,
            stream_id: u64,
            conn_id: String,
            file_length: Option<usize>,
        ) -> Result<FileWriterHandle, String> {
            let file_write_uuid = Uuid::now_v7();

            let associated_worker_index = self
                .file_worker_pool
                .get_worker_index_for_this_stream(stream_id, conn_id);

            if let Some(associated_worker_index) = associated_worker_index {
                let file_writer_handle = FileWriterHandle::new(
                    file,
                    file_write_uuid,
                    &self.pending_files_map,
                    associated_worker_index,
                    &self.file_finishing_listener,
                    file_length,
                );

                self.pending_files_map
                    .insert_pending_writer(file_write_uuid, &file_writer_handle);
                Ok(file_writer_handle)
            } else {
                Err("faces_quic_server error ! no filewriter created ".to_string())
            }
        }

        pub fn get_file_writer_sender_by_index(&self, index: usize) -> Option<FileWriterListener> {
            let file_writer_listener = self.file_worker_pool.get_worker_listener_by_index(index);
            file_writer_listener
        }
    }
}

mod trait_writable {
    pub trait FileWritable: Send + 'static {
        fn write_on_disk(&self) -> Result<usize, ()>;
    }
}

mod writable_type {
    use std::{
        fmt::Debug,
        fs::{File, OpenOptions},
        io::{self, BufWriter, ErrorKind, Read, Write},
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };

    use super::trait_writable::FileWritable;
    use crate::file_writer::{
        event_loop_types::{FileFinishingEvEvent, FileWrkrEvLoopId},
        pending_files_map::PendingFilesMap,
    };
    use faces_event_loop_utils::PublicEventListener;
    use uuid::Uuid;

    type FinishingCallback = Arc<dyn Fn() + Send + Sync + 'static>;

    #[derive(Debug)]
    struct State {
        writer: Option<BufWriter<File>>,
        can_close: bool,
        // Keep the first failure: a later successful flush cannot repair a
        // partially failed write or justify publishing this file.
        failure: Option<(ErrorKind, String)>,
    }
    impl State {
        fn error(&self) -> Option<io::Error> {
            self.failure
                .as_ref()
                .map(|(kind, message)| io::Error::new(*kind, message.clone()))
        }
        fn fail(&mut self, error: io::Error) -> io::Error {
            let failure = self
                .failure
                .get_or_insert_with(|| (error.kind(), error.to_string()));
            io::Error::new(failure.0, failure.1.clone())
        }
        fn flush(&mut self) -> io::Result<()> {
            if let Some(error) = self.error() {
                return Err(error);
            }
            let result = self
                .writer
                .as_mut()
                .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "no bufwriter"))
                .and_then(Write::flush);
            result.map_err(|error| self.fail(error))
        }
    }

    #[derive(Clone)]
    pub struct FileWriterHandle {
        associated_worker_index: usize,
        handle_id: Uuid,
        pending_file_map_ref: PendingFilesMap,
        inner: Arc<Mutex<State>>,
        file_length: Arc<(AtomicUsize, AtomicBool)>,
        written: Arc<AtomicUsize>,
        finishing_callback: Arc<Mutex<Option<FinishingCallback>>>,
        file_finishing_listener: Arc<PublicEventListener<FileFinishingEvEvent, FileWrkrEvLoopId>>,
    }
    impl Debug for FileWriterHandle {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "FileWriterHandle({})", self.handle_id)
        }
    }
    impl FileWriterHandle {
        pub fn new(
            writer: File,
            handle_id: Uuid,
            pending_file_map_ref: &PendingFilesMap,
            associated_worker_index: usize,
            file_finishing_listener: &Arc<
                PublicEventListener<FileFinishingEvEvent, FileWrkrEvLoopId>,
            >,
            file_length: Option<usize>,
        ) -> Self {
            Self {
                associated_worker_index,
                handle_id,
                pending_file_map_ref: pending_file_map_ref.clone(),
                inner: Arc::new(Mutex::new(State {
                    writer: Some(BufWriter::new(writer)),
                    can_close: false,
                    failure: None,
                })),
                file_length: Arc::new((
                    AtomicUsize::new(file_length.unwrap_or(0)),
                    AtomicBool::new(file_length.is_some()),
                )),
                written: Arc::new(AtomicUsize::new(0)),
                finishing_callback: Arc::new(Mutex::new(None)),
                file_finishing_listener: file_finishing_listener.clone(),
            }
        }

        // Called only under inner, which also serializes callback registration.
        // Completion is based on an exact byte count and a successful flush.
        fn complete_if_ready(&self, state: &mut State) -> io::Result<bool> {
            if let Some(error) = state.error() {
                return Err(error);
            }
            if state.can_close {
                return Ok(true);
            }
            if !self.file_length.1.load(Ordering::Acquire) {
                return Ok(false);
            }
            let expected = self.file_length.0.load(Ordering::Acquire);
            match self.written().cmp(&expected) {
                std::cmp::Ordering::Less => Ok(false),
                std::cmp::Ordering::Greater => Err(state.fail(io::Error::new(
                    ErrorKind::InvalidData,
                    "file exceeds expected length",
                ))),
                std::cmp::Ordering::Equal => {
                    state.flush()?;
                    state.can_close = true;
                    Ok(true)
                }
            }
        }

        // Run after releasing inner. A terminal failure discards a success-only
        // callback. Removing this map entry does not remove the file or worker binding.
        fn settle(&self, result: &io::Result<bool>, callback: Option<FinishingCallback>) {
            if !matches!(result, Ok(false)) {
                self.pending_file_map_ref.yield_writer_by_id(self.handle_id);
            }
            if let Some(cb) = callback {
                if self
                    .file_finishing_listener
                    .0
                    .send(FileFinishingEvEvent::FinishingFileWrite { cb: cb.clone() })
                    .is_err()
                {
                    error!("Finishing queue unavailable; running callback on the caller outside writer locks");
                    cb();
                }
            }
        }
        fn completion_callback(&self, result: &io::Result<bool>) -> Option<FinishingCallback> {
            match result {
                Ok(true) => self.take_callback(),
                Err(_) => {
                    self.take_callback();
                    None
                }
                Ok(false) => None,
            }
        }

        /// FIN supplies the actual received size, independent of Content-Length.
        pub(crate) fn finish_received(&self, received: usize) {
            let failure = {
                let mut state = self.inner.lock().unwrap();
                if self.file_length.1.load(Ordering::Acquire)
                    && self.file_length.0.load(Ordering::Acquire) != received
                {
                    Some(state.fail(io::Error::new(
                        ErrorKind::InvalidData,
                        "received length differs from declared length",
                    )))
                } else {
                    None
                }
            };
            if let Some(error) = failure {
                let callback = self.take_callback();
                self.settle(&Err(error), None);
                drop(callback);
            } else {
                self.set_file_length(received);
            }
        }

        pub fn set_file_length(&self, file_length: usize) {
            let (result, callback) = {
                let mut state = self.inner.lock().unwrap();
                if !self.file_length.1.load(Ordering::Acquire) {
                    self.file_length.0.store(file_length, Ordering::Release);
                    self.file_length.1.store(true, Ordering::Release);
                }
                let result = self.complete_if_ready(&mut state);
                let callback = self.completion_callback(&result);
                (result, callback)
            };
            self.settle(&result, callback);
            if let Err(error) = result {
                error!("Failed to finish file: {error}");
            }
        }
        pub fn register_finishing_callback(
            &self,
            finishing_cb: Arc<impl Fn() + Send + Sync + 'static>,
        ) {
            let (result, callback) = {
                let mut state = self.inner.lock().unwrap();
                *self.finishing_callback.lock().unwrap() = Some(finishing_cb);
                let result = self.complete_if_ready(&mut state);
                let callback = self.completion_callback(&result);
                (result, callback)
            };
            self.settle(&result, callback);
            if let Err(error) = result {
                error!("File completion failed: {error}");
            }
        }
        pub fn take_callback(&self) -> Option<FinishingCallback> {
            self.finishing_callback.lock().unwrap().take()
        }
        pub fn is_file_written(&self) -> Result<bool, String> {
            let state = self.inner.lock().unwrap();
            if let Some(error) = state.error() {
                return Err(error.to_string());
            }
            if !self.file_length.1.load(Ordering::Acquire) {
                return Err("No file_length set yet".to_owned());
            }
            Ok(state.can_close)
        }
        pub fn close_file(&self) {
            if let Err(error) = self.flush() {
                error!("Failed to close file: {error}");
            }
        }
        pub fn get_associated_worker_index(&self) -> usize {
            self.associated_worker_index
        }
        pub fn get_file_writer_id(&self) -> Uuid {
            self.handle_id
        }
        pub fn written(&self) -> usize {
            self.written.load(Ordering::Acquire)
        }

        pub fn transfert_bytes_from_temp_file(
            &self,
            storage_path: &str,
            bytes: &[u8],
        ) -> io::Result<()> {
            let (result, callback) = {
                let mut state = self.inner.lock().unwrap();
                // Keep the same lock across flush/read/rewrite so queued appends
                // cannot enter the old file between its snapshot and replacement.
                let result = (|| {
                    if let Some(error) = state.error() {
                        return Err(error);
                    }
                    if bytes.is_empty() {
                        return self.complete_if_ready(&mut state);
                    }
                    if state.can_close {
                        return Err(io::Error::new(
                            ErrorKind::InvalidInput,
                            "file already completed",
                        ));
                    }
                    state.flush()?;
                    let mut prefix = bytes.to_vec();
                    File::open(storage_path)?.read_to_end(&mut prefix)?;
                    if self.file_length.1.load(Ordering::Acquire)
                        && prefix.len() > self.file_length.0.load(Ordering::Acquire)
                    {
                        return Err(io::Error::new(
                            ErrorKind::InvalidData,
                            "file exceeds expected length",
                        ));
                    }
                    let file = OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open(storage_path)?;
                    let mut writer = BufWriter::new(file);
                    writer.write_all(&prefix)?;
                    writer.flush()?;
                    state.writer = Some(writer);
                    self.written.store(prefix.len(), Ordering::Release);
                    self.complete_if_ready(&mut state)
                })();
                let result = result.map_err(|error| {
                    if state.can_close {
                        error
                    } else {
                        state.fail(error)
                    }
                });
                let callback = self.completion_callback(&result);
                (result, callback)
            };
            self.settle(&result, callback);
            result.map(|_| ())
        }
        pub fn flush(&self) -> io::Result<()> {
            let (result, callback) = {
                let mut state = self.inner.lock().unwrap();
                let result = state
                    .flush()
                    .and_then(|()| self.complete_if_ready(&mut state));
                let callback = self.completion_callback(&result);
                (result, callback)
            };
            self.settle(&result, callback);
            result.map(|_| ())
        }
        pub fn write_on_disk(&self, data: &[u8]) -> Result<usize, ()> {
            let (result, callback) = {
                let mut state = self.inner.lock().unwrap();
                let result = (|| {
                    if let Some(error) = state.error() {
                        return Err(error);
                    }
                    if state.can_close && !data.is_empty() {
                        return Err(io::Error::new(
                            ErrorKind::InvalidInput,
                            "file already completed",
                        ));
                    }
                    let next = self.written().checked_add(data.len()).ok_or_else(|| {
                        io::Error::new(ErrorKind::InvalidData, "file length overflow")
                    })?;
                    if self.file_length.1.load(Ordering::Acquire)
                        && next > self.file_length.0.load(Ordering::Acquire)
                    {
                        return Err(io::Error::new(
                            ErrorKind::InvalidData,
                            "file exceeds expected length",
                        ));
                    }
                    let writer = state
                        .writer
                        .as_mut()
                        .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "no bufwriter"))?;
                    writer.write_all(data)?;
                    self.written.store(next, Ordering::Release);
                    self.complete_if_ready(&mut state)
                })();
                let result = result.map_err(|error| {
                    if state.can_close {
                        error
                    } else {
                        state.fail(error)
                    }
                });
                let callback = self.completion_callback(&result);
                (result, callback)
            };
            self.settle(&result, callback);
            result.map(|_| data.len()).map_err(|error| {
                error!("File write failed: {error}");
            })
        }
        /// Invoke the success callback after an exact byte count and successful
        /// flush. Registration and completion share the writer lock. As before,
        /// only the latest pending callback is retained; this is not a broadcast API.
        /// I/O failure remains observable through flush/is_file_written.
        pub fn on_file_written(&self, cb: impl Fn(usize) + Send + Sync + 'static) {
            let written = self.written.clone();
            self.register_finishing_callback(Arc::new(move || cb(written.load(Ordering::Acquire))));
        }
    }

    #[derive(Clone)]
    pub struct WritableItem {
        packet_id: usize,
        data: Vec<u8>,
        writer: FileWriterHandle,
    }
    impl WritableItem {
        pub fn new(data: Vec<u8>, packet_id: usize, writer: FileWriterHandle) -> Self {
            Self {
                packet_id,
                data,
                writer,
            }
        }
    }
    impl FileWritable for WritableItem {
        fn write_on_disk(&self) -> Result<usize, ()> {
            self.writer.write_on_disk(&self.data)
        }
    }
}

#[cfg(test)]
mod tests;
