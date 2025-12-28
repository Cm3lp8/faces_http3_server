pub use file_wrtr::{FileWriter, FileWriterChannel};
pub use trait_writable::FileWritable;
pub use writable_type::{FileWriterHandle, WritableItem};

mod file_wrtr {
    use std::fs::File;

    use crossbeam_channel::SendError;

    use super::{file_writer_worker, trait_writable::FileWritable, WritableItem};

    pub struct FileWriterChannel {
        sender: crossbeam_channel::Sender<WritableItem<File>>,
    }
    impl Clone for FileWriterChannel {
        fn clone(&self) -> Self {
            Self {
                sender: self.sender.clone(),
            }
        }
    }
    impl FileWriterChannel {
        pub fn send(
            &self,
            writable_item: WritableItem<File>,
        ) -> Result<(), SendError<WritableItem<File>>> {
            self.sender.send(writable_item)
        }
    }
    pub struct FileWriter<T: FileWritable> {
        channel: (crossbeam_channel::Sender<T>, crossbeam_channel::Receiver<T>),
    }

    impl FileWriter<WritableItem<File>> {
        pub fn new() -> Self {
            let channel = crossbeam_channel::unbounded::<WritableItem<File>>();

            file_writer_worker::run(channel.1.clone());

            Self { channel }
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
        fs::File,
        io::{BufWriter, ErrorKind, Write},
        sync::{Arc, Condvar, Mutex},
        time::Duration,
    };

    use quiche::Error;

    use crate::file_writer::FileWriter;

    use super::trait_writable::FileWritable;

    impl<W> Clone for FileWriterHandle<W>
    where
        W: std::io::Write + Send + Sync + 'static,
    {
        fn clone(&self) -> Self {
            Self {
                inner: self.inner.clone(),
            }
        }
    }
    #[derive(Debug)]
    struct State<W>
    where
        W: std::io::Write + Send + Sync + 'static,
    {
        writer: BufWriter<W>,
        written: usize,
        closed: bool,
    }

    #[derive(Debug)]
    pub struct FileWriterHandle<W>
    where
        W: std::io::Write + Sync + Send + 'static,
    {
        inner: Arc<(Mutex<State<W>>, Condvar)>,
    }
    impl<W> FileWriterHandle<W>
    where
        W: std::io::Write + Send + Sync + 'static,
    {
        pub fn new(writer: W) -> Self {
            Self {
                inner: Arc::new((
                    Mutex::new(State {
                        writer: BufWriter::new(writer),
                        written: 0,
                        closed: false,
                    }),
                    Condvar::new(),
                )),
            }
        }
        pub fn written(&self) -> usize {
            let guard = &*self.inner.0.lock().unwrap();

            guard.written
        }
        pub fn flush(&self) -> Result<(), std::io::Error> {
            let guard = &mut *self.inner.0.lock().unwrap();
            guard.writer.flush()
        }
        fn write_on_disk(&self, data: &[u8]) -> Result<usize, ()> {
            let writer = &mut *self.inner.0.lock().unwrap();
            let cdv = &self.inner.1;

            match writer.writer.write_all(data) {
                Ok(()) => {
                    writer.written += data.len();
                    info!("writtent [{:?}]", writer.written);
                    cdv.notify_all();
                    Ok(data.len())
                }
                Err(e) => {
                    error!("[{:?}]", e);
                    Err(())
                }
            }
        }
        ///Drop the BufWriter<file> to close it and return the file path.
        pub fn close_file(&mut self, content_length_required: usize) -> Result<(), std::io::Error> {
            let file_h = self.inner.clone();
            std::thread::spawn(move || {
                let mut retry_attemps = 0;

                let cdv = &file_h.1;
                let guard = file_h.0.lock().unwrap();
                info!("wait to close");
                if guard.written <= content_length_required && !guard.closed {
                    cdv.wait(guard);
                }
            });
            Ok(())
        }
    }

    impl FileWriterHandle<std::fs::File> {
        /// [`on_file_written`] trigs a callback when required content is written .
        pub fn on_file_written(
            &self,
            content_length_required: usize,
            cb: impl FnOnce(usize) + Send + Sync + 'static,
        ) {
            let file_h = self.inner.clone();
            std::thread::spawn(move || {
                let cdv = &file_h.1;
                let guard = file_h.0.lock().unwrap();
                info!("wait to close whyloop ?");
                if guard.written <= content_length_required {
                    match cdv.wait(guard) {
                        Ok(_) => {}
                        Err(e) => {}
                    }
                }

                let guard = &mut *file_h.0.lock().unwrap();
                let mut retry = 0;
                loop {
                    match guard.writer.flush() {
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

                info!("File written !!");
                cb(content_length_required)
            });
        }
    }

    pub struct WritableItem<W: std::io::Write + Send + Sync + 'static> {
        packet_id: usize,
        data: Vec<u8>,
        writer: FileWriterHandle<W>, // (_, bytes_written)
    }

    impl<W: std::io::Write + Send + Sync + 'static> WritableItem<W> {
        pub fn new(data: Vec<u8>, packet_id: usize, writer: FileWriterHandle<W>) -> Self {
            Self {
                packet_id,
                data,
                writer,
            }
        }
    }

    impl<W: Write + Send + 'static + Sync> FileWritable for WritableItem<W> {
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
                    //   std::thread::sleep(Duration::from_micros(20));
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
