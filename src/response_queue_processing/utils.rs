use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::Mutex;
use std::{fs::File, sync::Arc};

use quiche::h3::{self, NameValue};
use uuid::Uuid;

use crate::file_writer::{self, FileWriter, FileWriterHandle, WritableItem};
use crate::{server_config, BodyStorage, DataManagement, ServerConfig};

pub fn build_temp_stage_file_storage_path(
    server_config: &ServerConfig,
    headers: &[h3::Header],
    stream_id: u64,
    conn_id: String,
    content_length: Option<usize>,
    data_management_type: &Option<DataManagement>,
    file_writer_manager: &Arc<FileWriter>,
) -> Option<(PathBuf, FileWriterHandle)> {
    let Some(data_management_type) = data_management_type else {
        return None;
    };
    if let Some(body_storage) = data_management_type.is_body_storage() {
        match body_storage {
            BodyStorage::File => {
                let mut path = server_config.get_storage_path();

                let mut extension: Option<String> = None;

                if let Some(found_content_type) =
                    headers.iter().find(|hdr| hdr.name() == b"content-type")
                {
                    match found_content_type.value() {
                        b"text/plain" => {
                            extension = Some(String::from(".txt"));
                        }
                        _ => {}
                    }
                };

                let uuid = Uuid::new_v4();
                let mut uuid = uuid.to_string();

                if let Some(ext) = extension {
                    uuid = format!("{}{}", uuid, ext);
                }

                path.push(uuid);
                file_writer_manager.associate_stream_with_next_listener(stream_id, conn_id.clone());

                let file_open = if let Ok(file) = File::create(path.clone()) {
                    file_writer_manager.create_file_writer_handle(
                        file,
                        stream_id,
                        conn_id,
                        content_length,
                    )
                } else {
                    error!("Failed creating [{:?}] file", path);
                    return None;
                };

                Some((
                    path,
                    match file_open {
                        Ok(fw) => fw,
                        Err(e) => {
                            error!("[{:?}]", e);
                            return None;
                        }
                    },
                ))
            }
            BodyStorage::InMemory => None,
        }
    } else {
        None
    }
}
