pub use file_wrtr::{FileWriter, FileWriterChannel};
pub use trait_writable::FileWritable;
pub use writable_type::{FileWriterHandle, WritableItem};
mod pending_files_map;

mod file_wrtr {
    use std::fs::File;

    use crossbeam_channel::SendError;
    use uuid::Uuid;

    use crate::{file_writer::pending_files_map::PendingFilesMap, FileWriterHandle};

    use super::{file_writer_worker, trait_writable::FileWritable, WritableItem};

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
    pub struct FileWriter<T: FileWritable> {
        channel: (crossbeam_channel::Sender<T>, crossbeam_channel::Receiver<T>),
        pending_files_map: PendingFilesMap,
    }

    impl FileWriter<WritableItem> {
        pub fn new() -> Self {
            let channel = crossbeam_channel::unbounded::<WritableItem>();

            file_writer_worker::run(channel.1.clone());
            let pending_files_map = PendingFilesMap::new();

            Self {
                channel,
                pending_files_map,
            }
        }
        pub fn create_file_writer_handle(&self, file: std::fs::File) -> FileWriterHandle {
            let file_write_uuid = Uuid::now_v7();

            let file_writer_handle =
                FileWriterHandle::new(file, file_write_uuid, &self.pending_files_map);

            self.pending_files_map
                .insert_pending_writer(file_write_uuid, &file_writer_handle);
            file_writer_handle
        }
        pub fn get_file_writer_sender(&self) -> FileWriterChannel {
            let sender = self.channel.0.clone();
            FileWriterChannel { sender }
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
        sync::{Arc, Condvar, Mutex},
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
                pending_file_map_ref: self.pending_file_map_ref.clone(),
                handle_id: self.handle_id.clone(),
                inner: self.inner.clone(),
            }
        }
    }
    #[derive(Debug)]
    struct State {
        writer: Option<BufWriter<File>>,
        written: usize,
        closed: bool,
    }

    #[derive(Debug)]
    pub struct FileWriterHandle {
        handle_id: Uuid,
        pending_file_map_ref: PendingFilesMap,
        inner: Arc<(Mutex<State>, Condvar)>,
    }
    impl FileWriterHandle {
        pub fn new(writer: File, handle_id: Uuid, pending_file_map_ref: &PendingFilesMap) -> Self {
            Self {
                handle_id,
                pending_file_map_ref: pending_file_map_ref.clone(),
                inner: Arc::new((
                    Mutex::new(State {
                        writer: Some(BufWriter::new(writer)),
                        written: 0,
                        closed: false,
                    }),
                    Condvar::new(),
                )),
            }
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
            guard.written = prefix_len;

            warn!("D new file should be writen");
            Ok(())
        }
        pub fn get_file_writer_id(&self) -> Uuid {
            self.handle_id
        }
        pub fn written(&self) -> usize {
            let guard = &*self.inner.0.lock().unwrap();

            guard.written
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
                        writer.written += data.len();
                        cdv.notify_all();
                        println!("written [{:?}]", writer.written);
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
            std::thread::spawn(move || {
                let mut retry_attemps = 0;

                let cdv = &file_h.1;
                let guard = file_h.0.lock().unwrap();
                if guard.written < content_length_required {
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
            std::thread::spawn(move || {
                let cdv = &file_h.1;
                {
                    let guard = file_h.0.lock().unwrap();
                    if guard.written < content_length_required {
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

mod file_writer_worker {
    use std::time::Duration;

    use super::trait_writable::FileWritable;

    pub fn run<T: FileWritable>(receiver: crossbeam_channel::Receiver<T>) {
        if let Err(_) = std::thread::Builder::new()
            .stack_size(1024 * 1024 * 2)
            .spawn(move || {
                while let Ok(writable_item) = receiver.recv() {
                    if let Err(_) = writable_item.write_on_disk() {
                        error!("Failed to write data on disk");
                    }
                }
            })
        {
            error!("failed to run file writer worker")
        };
    }
}
