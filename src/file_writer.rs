pub use file_wrtr::{FileWriter, FileWriterChannel};
pub use trait_writable::FileWritable;
pub use writable_type::WritableItem;
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
        io::{BufWriter, Write},
        sync::{Arc, Mutex},
    };

    use super::trait_writable::FileWritable;

    pub struct WritableItem<W: std::io::Write> {
        packet_id: usize,
        data: Vec<u8>,
        writer: Arc<Mutex<BufWriter<W>>>,
    }

    impl<W: std::io::Write> WritableItem<W> {
        pub fn new(data: Vec<u8>, packet_id: usize, writer: Arc<Mutex<BufWriter<W>>>) -> Self {
            Self {
                packet_id,
                data,
                writer,
            }
        }
    }

    impl<W: Write + Send + 'static> FileWritable for WritableItem<W> {
        fn write_on_disk(&self) -> Result<usize, ()> {
            let mut data_len = self.data.len();
            let writer = &mut *self.writer.lock().unwrap();

            while let Ok(n) = writer.write(&self.data[..data_len]) {
                data_len -= n;
                if data_len == 0 {
                    if let Err(_) = writer.flush() {
                        return Err(());
                    }
                    return Ok(self.data.len());
                }
            }
            Err(())
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
                    std::thread::sleep(Duration::from_micros(20));
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
