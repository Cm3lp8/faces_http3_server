pub use file_wrtr::{FileWriter, FileWriterChannel};
pub use trait_writable::FileWritable;
pub use writable_type::{FileWriterHandle, WritableItem};
mod event_loop;
mod event_loop_cb;
mod event_loop_types;
mod pending_files_map;

mod file_wrtr {
    use std::{fs::File, sync::Arc};

    use crossbeam_channel::SendError;
    use faces_event_loop_utils::EventLoop;
    use uuid::Uuid;

    use crate::{
        file_writer::{
            event_loop::FileWriterPool,
            event_loop_cb::manage_event,
            event_loop_types::{FileWriterListener, FileWrkrEvEvent, FileWrkrEvLoopId},
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
        pending_files_map: PendingFilesMap,
    }

    impl FileWriter {
        pub fn new() -> Self {
            let pending_files_map = PendingFilesMap::new();

            let file_writer_worker_pool = FileWriterPool::new(8, manage_event);

            Self {
                file_worker_pool: Arc::new(file_writer_worker_pool),
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
        fs::{File, OpenOptions},
        io::{self, BufWriter, ErrorKind, Read, Write},
        sync::{atomic::AtomicUsize, Arc, Condvar, Mutex},
        time::Duration,
    };

    use quiche::Error;
    use uuid::Uuid;

    use crate::file_writer::{
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
                written: self.written.clone(),
            }
        }
    }
    #[derive(Debug)]
    struct State {
        writer: Option<BufWriter<File>>,
        closed: bool,
    }

    #[derive(Debug)]
    pub struct FileWriterHandle {
        associated_worker_index: usize,
        handle_id: Uuid,
        pending_file_map_ref: PendingFilesMap,
        inner: Arc<(Mutex<State>, Condvar)>,
        written: Arc<AtomicUsize>,
    }
    impl FileWriterHandle {
        pub fn new(
            writer: File,
            handle_id: Uuid,
            pending_file_map_ref: &PendingFilesMap,
            associated_worker_index: usize,
        ) -> Self {
            Self {
                associated_worker_index,
                handle_id,
                pending_file_map_ref: pending_file_map_ref.clone(),
                inner: Arc::new((
                    Mutex::new(State {
                        writer: Some(BufWriter::new(writer)),
                        closed: false,
                    }),
                    Condvar::new(),
                )),
                written: Arc::new(AtomicUsize::new(0)),
            }
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
                let state = &mut *self.inner.0.lock().unwrap();

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

            let guard = &mut *self.inner.0.lock().unwrap();

            let mut b_writer: BufWriter<std::fs::File> = BufWriter::new(new_file);

            b_writer.write_all(&prefix)?;
            b_writer.flush()?;
            guard.writer = Some(b_writer);
            self.written
                .store(prefix_len, std::sync::atomic::Ordering::Relaxed);

            warn!("D new file should be writen");
            Ok(())
        }
        pub fn get_file_writer_id(&self) -> Uuid {
            self.handle_id
        }
        pub fn written(&self) -> usize {
            self.written.load(std::sync::atomic::Ordering::Relaxed)
        }
        pub fn flush(&self) -> Result<(), std::io::Error> {
            let guard = &mut *self.inner.0.lock().unwrap();
            if let Some(writer) = &mut guard.writer {
                writer.flush()
            } else {
                Err(io::Error::new(ErrorKind::NotFound, "no bufwriter"))
            }
        }
        pub fn write_on_disk(&self, data: &[u8]) -> Result<usize, ()> {
            let writer = &mut *self.inner.0.lock().unwrap();
            let cdv = &self.inner.1;

            if let Some(wrtr) = &mut writer.writer {
                match wrtr.write_all(data) {
                    Ok(()) => {
                        self.written
                            .fetch_add(data.len(), std::sync::atomic::Ordering::Relaxed);
                        cdv.notify_all();
                        println!(
                            "written [{:?}]",
                            self.written.load(std::sync::atomic::Ordering::Relaxed)
                        );
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
        ///Drop the BufWriter<file> to close it and return the file path.
        pub fn close_file(&mut self, content_length_required: usize) -> Result<(), std::io::Error> {
            let file_h = self.inner.clone();
            let written = self.written.clone();
            std::thread::spawn(move || {
                let mut retry_attemps = 0;

                let cdv = &file_h.1;
                let guard = file_h.0.lock().unwrap();
                if written.load(std::sync::atomic::Ordering::Relaxed) < content_length_required {
                    cdv.wait(guard);
                }
            });
            Ok(())
        }
    }

    impl FileWriterHandle {
        /// [`on_file_written`] trigs a callback when required content is written .
        pub fn on_file_written(
            &self,
            content_length_required: usize,
            cb: impl FnOnce(usize) + Send + Sync + 'static,
        ) {
            let file_h = self.inner.clone();
            let pending_files_map_c = self.pending_file_map_ref.clone();
            let handle_id = self.handle_id;
            let written = self.written.clone();
            // TODO send to threadWorker with a fixed amount of threads
            std::thread::spawn(move || {
                let cdv = &file_h.1;
                {
                    while &written.load(std::sync::atomic::Ordering::Relaxed)
                        < &content_length_required
                    {
                        let mut guard = file_h.0.lock().unwrap();
                        match cdv.wait(guard) {
                            Ok(_) => {
                                info!("Condvar wake")
                            }
                            Err(e) => {}
                        }
                    }
                }

                let guard = &mut *file_h.0.lock().unwrap();
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

                info!("File written !!");
                cb(content_length_required);
                if pending_files_map_c.yeild_writer_by_id(handle_id) {
                    info!("File writer has been drop after complete file write ");
                }
            });
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
