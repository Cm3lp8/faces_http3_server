use crate::file_writer::WritableItem;

#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug)]
pub enum FileWrkrEvLoopId {
    MainLoop,
}

#[derive(Clone)]
pub enum FileWrkrEvEvent {
    WriteOnDisk(WritableItem),
}

pub struct FileWriterListener {
    sender: crossbeam_channel::Sender<FileWrkrEvEvent>,
}
impl FileWriterListener {
    pub fn new(sender: &crossbeam_channel::Sender<FileWrkrEvEvent>) -> Self {
        Self {
            sender: sender.clone(),
        }
    }
    pub fn send_writable_item(&self, writable_item: WritableItem) {
        self.sender
            .send(FileWrkrEvEvent::WriteOnDisk(writable_item));
    }
}
