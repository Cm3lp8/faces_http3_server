use std::collections::{HashMap, HashSet};

use dashmap::{DashMap, DashSet};

#[derive(Eq, PartialEq, Hash, Clone)]
struct ConnStreamK {
    stream_id: u64,
    conn_id: Vec<u8>,
}

impl ConnStreamK {
    pub fn new(stream_id: u64, conn_id: Vec<u8>) -> Self {
        Self { stream_id, conn_id }
    }
}
#[derive(Eq, PartialEq, Hash, Clone)]
struct ConnPathK {
    path: String,
    conn_id: Vec<u8>,
}

impl ConnPathK {
    pub fn new(path: String, conn_id: Vec<u8>) -> Self {
        Self { path, conn_id }
    }
}

pub struct InFlightStreamsPathVerifier {
    accepted_route_pathes: HashSet<&'static str>,
    accepted_route_stream_pathes: Option<HashSet<String>>,
    stream_map: DashMap<ConnStreamK, ConnPathK>,
    path_map: DashMap<ConnPathK, u64>,
}

impl InFlightStreamsPathVerifier {
    pub fn new(
        route_format: HashSet<&'static str>,
        stream_pathes_set: Option<HashSet<String>>,
    ) -> Self {
        Self {
            accepted_route_pathes: route_format,
            accepted_route_stream_pathes: stream_pathes_set,
            stream_map: DashMap::new(),
            path_map: DashMap::new(),
        }
    }

    #[inline]
    pub fn insert_stream_path_for_conn(
        &self,
        conn_id: Vec<u8>,
        stream_id: u64,
        path: &str,
    ) -> bool {
        if let Some(stream_path_set) = self.accepted_route_stream_pathes.as_ref() {
            if !stream_path_set.contains(path) && !self.accepted_route_pathes.contains(path) {
                return false;
            }
        } else {
            if !self.accepted_route_pathes.contains(path) {
                return false;
            };
        }

        let k_0 = ConnStreamK::new(stream_id, conn_id.clone());
        let k = ConnPathK::new(path.to_string(), conn_id);
        self.stream_map.insert(k_0.clone(), k.clone());
        self.path_map.insert(k, stream_id);
        true
    }

    pub fn is_finished_request_a_valid_path(&self, conn_id: Vec<u8>, stream_id: u64) -> bool {
        if let Some(entry) = self.stream_map.get(&ConnStreamK::new(stream_id, conn_id)) {
            if let Some(stream_id_reg) = self.path_map.get(&entry) {
                if *stream_id_reg == stream_id {
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        }
    }
}
