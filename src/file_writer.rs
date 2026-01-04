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
            atomic::{AtomicBool, AtomicUsize},
            Arc, Condvar, Mutex,
        },
        time::Duration,
    };

    use faces_event_loop_utils::{EventObserver, PublicEventListener};
    use quiche::Error;
    use uuid::Uuid;

    use crate::file_writer::{
        event_loop_types::{FileFinishingEvEvent, FileWrkrEvLoopId},
        pending_files_map::{self, PendingFilesMap},
        FileWriter,
    };

    use super::trait_writable::FileWritable;

    impl Clone for FileWriterHandle {
        fn clone(&self) -> Self {
            Self {
                associated_worker_index: self.associated_worker_index.clone(),
                pending_file_map_ref: self.pending_file_map_ref.clone(),
                handle_id: self.handle_id.clone(),
                inner: self.inner.clone(),
                file_length: self.file_length.clone(),
                written: self.written.clone(),
                finishing_callback: self.finishing_callback.clone(),
                file_finishing_listener: self.file_finishing_listener.clone(),
            }
        }
    }
    #[derive(Debug)]
    struct State {
        writer: Option<BufWriter<File>>,
        can_close: bool,
    }
    impl State {
        pub fn close_file(&mut self) {
            let mut retry = 0;
            // FLushing
            loop {
                info!("Before fflush");
                if let Some(writer) = &mut self.writer {
                    match writer.flush() {
                        Ok(_) => break,
                        Err(e) if e.kind() == ErrorKind::Interrupted => {
                            retry += 1;

                            if retry >= 5 {
                                break;
                            }
                        }
                        _ => {
                            break;
                        }
                    }
                }
            }

            self.can_close = true;

            info!("File written !!");
        }
    }

    impl Debug for FileWriterHandle {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, " File Writer handle id [{:?}]", self.handle_id)
        }
    }
    pub struct FileWriterHandle {
        associated_worker_index: usize,
        handle_id: Uuid,
        pending_file_map_ref: PendingFilesMap,
        inner: Arc<Mutex<State>>,
        file_length: Arc<(AtomicUsize, AtomicBool)>,
        written: Arc<AtomicUsize>,
        finishing_callback: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync + 'static>>>>,
        file_finishing_listener: Arc<PublicEventListener<FileFinishingEvEvent, FileWrkrEvLoopId>>,
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
            let file_length = if let Some(file_length) = file_length {
                warn!("filewriter => new set file_length [{:?}]", file_length);
                Arc::new((AtomicUsize::new(file_length), AtomicBool::new(true)))
            } else {
                Arc::new((AtomicUsize::new(0), AtomicBool::new(false)))
            };
            Self {
                associated_worker_index,
                handle_id,
                pending_file_map_ref: pending_file_map_ref.clone(),
                inner: Arc::new(Mutex::new(State {
                    writer: Some(BufWriter::new(writer)),
                    can_close: false,
                })),
                file_length,
                written: Arc::new(AtomicUsize::new(0)),
                finishing_callback: Arc::new(Mutex::new(None)),
                file_finishing_listener: file_finishing_listener.clone(),
            }
        }
        pub fn set_file_length(&self, file_length: usize) {
            if self
                .file_length
                .1
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return;
            };

            self.file_length
                .0
                .store(file_length, std::sync::atomic::Ordering::Release);
            self.file_length
                .1
                .store(true, std::sync::atomic::Ordering::Release);
        }
        pub fn register_finishing_callback(
            &self,
            finishing_cb: Arc<impl Fn() + Send + Sync + 'static>,
        ) {
            *self.finishing_callback.lock().unwrap() = Some(finishing_cb);
        }
        pub fn take_callback(&self) -> Option<Arc<dyn Fn() + Send + Sync + 'static>> {
            self.finishing_callback.lock().unwrap().take()
        }
        pub fn is_file_written(&self) -> Result<bool, String> {
            if !self
                .file_length
                .1
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(String::from("No file_length set yet"));
            }

            if self
                .file_length
                .0
                .load(std::sync::atomic::Ordering::Acquire)
                == self.written.load(std::sync::atomic::Ordering::Acquire)
            {
                Ok(true)
            } else {
                Ok(false)
            }
        }
        pub fn close_file(&self) {
            let guard = &mut *self.inner.lock().unwrap();
            let mut retry = 0;
            // FLushing
            loop {
                info!("Before fflush");
                if let Some(writer) = &mut guard.writer {
                    match writer.flush() {
                        Ok(_) => break,
                        Err(e) if e.kind() == ErrorKind::Interrupted => {
                            retry += 1;

                            if retry >= 5 {
                                break;
                            }
                        }
                        _ => {
                            break;
                        }
                    }
                }
            }

            guard.can_close = true;

            info!("File written !!");
        }
        pub fn get_associated_worker_index(&self) -> usize {
            self.associated_worker_index
        }
        pub fn transfert_bytes_from_temp_file(
            &self,
            storage_path: &str,
            bytes: &[u8],
        ) -> Result<(), io::Error> {
            {
                // TODO see how to manage this with a temp file , if necesarry
                let state = &mut *self.inner.lock().unwrap();

                /*
                     *
                let temp_path = format!("{}.tmp", path);

                let mut tmp_file = File::create(path)?;

                tmp_file.write_all(bytes)?;
                     *
                     * */

                if let Some(writer) = &mut state.writer {
                    writer.flush()?;
                }

                {
                    state.writer.take();
                }
            }
            let mut temp_buf: Vec<u8> = vec![];

            let mut original_file = File::open(storage_path)?;

            original_file.read_to_end(&mut temp_buf)?;

            let mut prefix = bytes.to_vec();
            prefix.extend(temp_buf);
            let prefix_len = prefix.len();

            let new_file = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(storage_path)?;

            let guard = &mut *self.inner.lock().unwrap();

            let mut b_writer: BufWriter<std::fs::File> = BufWriter::new(new_file);

            b_writer.write_all(&prefix)?;
            b_writer.flush()?;
            guard.writer = Some(b_writer);
            self.written
                .store(prefix_len, std::sync::atomic::Ordering::Release);

            warn!("D new file should be writen");
            Ok(())
        }
        pub fn get_file_writer_id(&self) -> Uuid {
            self.handle_id
        }
        pub fn written(&self) -> usize {
            self.written.load(std::sync::atomic::Ordering::Acquire)
        }
        pub fn flush(&self) -> Result<(), std::io::Error> {
            let guard = &mut *self.inner.lock().unwrap();
            if let Some(writer) = &mut guard.writer {
                writer.flush()
            } else {
                Err(io::Error::new(ErrorKind::NotFound, "no bufwriter"))
            }
        }
        pub fn write_on_disk(&self, data: &[u8]) -> Result<usize, ()> {
            let writer = &mut *self.inner.lock().unwrap();

            if let Some(wrtr) = &mut writer.writer {
                match wrtr.write_all(data) {
                    Ok(()) => {
                        self.written
                            .fetch_add(data.len(), std::sync::atomic::Ordering::Release);
                        println!(
                            "written [{:?}]",
                            self.written.load(std::sync::atomic::Ordering::Acquire)
                        );

                        match self.is_file_written() {
                            Ok(true) => {
                                warn!("File  Written !!");
                                writer.close_file();
                                // try end of file cb signaling
                                if let Some(cb) = self.take_callback() {
                                    (cb)();
                                } else {
                                    warn!("No written cb");
                                }
                            }
                            Ok(false) => {
                                warn!("File Not Written Yet")
                            }
                            Err(e) => {
                                warn!("Content_length not set yet e [{:?}]", e)
                            }
                        }
                        Ok(data.len())
                    }
                    Err(e) => {
                        error!("[{:?}]", e);
                        Err(())
                    }
                }
            } else {
                Err(())
            }
        }
    }

    impl FileWriterHandle {
        /// [`on_file_written`] trigs a callback when required content is written .
        pub fn on_file_written(&self, cb: impl Fn(usize) + Send + Sync + 'static) {
            let file_h = self.inner.clone();
            let pending_files_map_c = self.pending_file_map_ref.clone();
            let handle_id = self.handle_id;
            let written = self.written.clone();
            let file_len = self.file_length.clone();
            // TODO send to threadWorker with a fixed amount of threads
            //

            let cb_sendable = Arc::new(cb);
            match self.is_file_written() {
                Ok(true) => {
                    warn!("on_file_written => file written");
                    //flush bufwriter !!!
                    self.close_file();
                    let cb = move || {
                        // FLushing

                        let content_length_required =
                            file_len.0.load(std::sync::atomic::Ordering::Acquire);
                        info!("File written cb executed !!");
                        cb_sendable(content_length_required);
                        if pending_files_map_c.yield_writer_by_id(handle_id) {
                            info!("File writer has been drop after complete file write ");
                        }
                    };

                    let sendable_cb = Arc::new(cb);

                    warn!("Will send event");
                    if let Err(e) = self
                        .file_finishing_listener
                        .0
                        .send(FileFinishingEvEvent::FinishingFileWrite { cb: sendable_cb })
                    {
                        error!("e[{:?}", e)
                    } else {
                        warn!("Sucesfully send to faces_event_loop_utils");
                    }
                }
                Ok(false) => {
                    //register callback to call it when required
                    warn!("on_file_written => not written");

                    let file_finishing_listener = self.file_finishing_listener.clone();

                    let cb_2 = move || {
                        let cb_sendable_clone = cb_sendable.clone();
                        let file_len = file_len.clone();
                        let pending_files_map_c_2 = pending_files_map_c.clone();
                        let cb = move || {
                            // FLushing

                            let content_length_required = file_len
                                .clone()
                                .0
                                .load(std::sync::atomic::Ordering::Acquire);
                            info!("yield 2 File written cb executed !!");
                            cb_sendable_clone(content_length_required);
                            if pending_files_map_c_2.yield_writer_by_id(handle_id) {
                                info!(
                                    "Yield 2 File writer has been drop after complete file write "
                                );
                            }
                        };

                        let sendable_cb = Arc::new(cb);

                        if let Err(e) = file_finishing_listener.send_event(
                            FileFinishingEvEvent::FinishingFileWrite { cb: sendable_cb },
                        ) {
                            error!("e[{:?}", e)
                        }
                    };

                    let cb_2_sendable = Arc::new(cb_2);
                    self.register_finishing_callback(cb_2_sendable);
                }
                Err(e) => {
                    warn!("on_file_written => content length not set  [{:?}]", e);
                    let file_finishing_listener = self.file_finishing_listener.clone();

                    let cb_2 = move || {
                        let cb_sendable_clone = cb_sendable.clone();
                        let file_len = file_len.clone();
                        let pending_files_map_c_2 = pending_files_map_c.clone();
                        let cb = move || {
                            // FLushing

                            let content_length_required = file_len
                                .clone()
                                .0
                                .load(std::sync::atomic::Ordering::Acquire);
                            info!("yield 3 File written cb executed !!");
                            cb_sendable_clone(content_length_required);
                            if pending_files_map_c_2.yield_writer_by_id(handle_id) {
                                info!(
                                    "Yield 3 File writer has been drop after complete file write "
                                );
                            }
                        };

                        let sendable_cb = Arc::new(cb);

                        if let Err(e) = file_finishing_listener.send_event(
                            FileFinishingEvEvent::FinishingFileWrite { cb: sendable_cb },
                        ) {
                            error!("e[{:?}", e)
                        }
                    };

                    let cb_2_sendable = Arc::new(cb_2);
                    self.register_finishing_callback(cb_2_sendable);
                }
            }
        }
    }

    #[derive(Clone)]
    pub struct WritableItem {
        packet_id: usize,
        data: Vec<u8>,
        writer: FileWriterHandle, // (_, bytes_written)
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
