use std::{
    fmt::{write, Debug},
    sync::Arc,
};

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
#[derive(Clone)]
pub enum FileFinishingEvEvent {
    FinishingFileWrite {
        cb: Arc<dyn Fn() + Send + Sync + 'static>,
    },
}
impl Debug for FileFinishingEvEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "")
    }
}
