use std::{fs, sync::Arc};

use dashmap::DashMap;
use uuid::Uuid;

use crate::FileWriterHandle;

#[derive(Clone, Debug)]
pub struct PendingFilesMap {
    file_writer_map: Arc<DashMap<Uuid, FileWriterHandle>>,
}

impl PendingFilesMap {
    pub fn new() -> Self {
        Self {
            file_writer_map: Arc::new(DashMap::new()),
        }
    }

    pub fn insert_pending_writer(&self, handle_id: Uuid, file_writer_handle: &FileWriterHandle) {
        self.file_writer_map
            .insert(handle_id, file_writer_handle.clone());
    }
    pub fn yeild_writer_by_id(&self, handle_id: Uuid) -> bool {
        self.file_writer_map.remove(&handle_id).is_some()
    }
}
