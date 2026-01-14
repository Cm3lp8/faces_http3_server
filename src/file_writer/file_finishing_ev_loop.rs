use super::event_loop_types::FileFinishingEvEvent;
pub fn manage_file_finishing_ev(event: FileFinishingEvEvent, context: &()) {
    match event {
        FileFinishingEvEvent::FinishingFileWrite { cb } => (cb)(),
    }
}
