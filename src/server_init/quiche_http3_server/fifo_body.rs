#![allow(warnings)]

pub use queue_implementation::{BodyReqQueue, QueueTrackableItem};

mod queue_implementation {
    use std::collections::{HashMap, VecDeque};

    type StreamIDs = (String, u64);
    type LastSendedIndex = usize;
    ///____________________________
    ///Fifo queue. Push back and pop front, keep track of packet index / conn, stream_id
    pub struct BodyReqQueue<T> {
        queue: VecDeque<T>,
        packet_progression: HashMap<StreamIDs, Option<LastSendedIndex>>,
    }
    impl<T: QueueTrackableItem> BodyReqQueue<T> {
        pub fn new() -> Self {
            Self {
                queue: VecDeque::new(),
                packet_progression: HashMap::new(),
            }
        }
        ///_______________________________
        ///Insert a new element in the fifo, update the HashMap with the (conn_id, stream_id) and
        ///the given packet index if
        ///doesn't exist yet in the table
        pub fn insert_first(&mut self, item: T) {
            let item_index = item.item_index();
            let stream_ids = (item.conn_id(), item.stream_id());

            self.packet_progression.entry(stream_ids).or_insert(None);

            self.queue.push_back(item);
        }

        pub fn is_empty(&self) -> bool {
            self.queue.is_empty()
        }
        ///__________________________________________
        ///Pop what is sendable or pushback the front item if its index is == last_index + 1
        ///Increment the last index send by 1.
        pub fn pop_front_sendable(&mut self) -> Option<T> {
            if let Some(popped) = self.queue.pop_front() {
                let stream_ids: StreamIDs = (popped.conn_id(), popped.stream_id());
                let popped_index = popped.item_index();

                if let Some(last_index) = self
                    .packet_progression
                    .entry(stream_ids.clone())
                    .or_default()
                {
                    if *last_index + 1 == popped_index {
                        if popped.is_last_item() {
                            self.packet_progression.remove(&stream_ids);
                        } else {
                            self.packet_progression
                                .entry(stream_ids)
                                .and_modify(|e| *e = Some(popped_index));
                        }
                        return Some(popped);
                    }

                    self.queue.push_back(popped);
                } else {
                    if popped_index == 0 {
                        if popped.is_last_item() {
                            self.packet_progression.remove(&stream_ids);
                        } else {
                            self.packet_progression
                                .entry(stream_ids)
                                .and_modify(|e| *e = Some(popped_index));
                        }
                        return Some(popped);
                    }
                    self.queue.push_back(popped);
                }
            }
            None
        }
        ///____________________________
        ///Push back item if stream is not writtable. Reset last_item_send to previous pos
        pub fn push_back(&mut self, item: T) {
            let stream_ids: StreamIDs = (item.conn_id(), item.stream_id());
            let popped_index = item.item_index();

            if popped_index == 0 {
                self.packet_progression
                    .entry(stream_ids)
                    .and_modify(|e| *e = None);
            } else {
                self.packet_progression
                    .entry(stream_ids)
                    .and_modify(|e| {
                        if let Some(index) = e.as_mut() {
                            *index -= 1;
                        }
                    })
                    .or_insert(Some(popped_index - 1));
            }
            self.queue.push_back(item);
        }
    }

    pub trait QueueTrackableItem {
        fn item_index(&self) -> usize;
        fn conn_id(&self) -> String;
        fn stream_id(&self) -> u64;
        fn is_last_item(&self) -> bool;
    }
}
