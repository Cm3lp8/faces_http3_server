use crate::file_writer::{event_loop_types::FileWrkrEvEvent, FileWritable};

pub fn manage_event(event: FileWrkrEvEvent) {
    match event {
        FileWrkrEvEvent::WriteOnDisk(writable_item) => {
            if let Err(e) = writable_item.write_on_disk() {
                error!(" [{:?}]", e)
            }
        }
    };
}
