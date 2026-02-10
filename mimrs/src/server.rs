//! Server mode for the CLI. Only used in the binary.

use base64::Engine;
use mim::types::Blake3Hash;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::{os::unix::net::UnixListener, path::Path};
use tracing::debug;

/// Map from known hashes to .gz.mim index file paths.
type Cache = HashMap<Blake3Hash, PathBuf>;

const BASE64: base64::engine::GeneralPurpose = base64::prelude::BASE64_URL_SAFE_NO_PAD;

/// Requests are simply exactly a single blake3 hash.
#[derive(bincode::Encode, bincode::Decode, Debug)]
enum Request {
    /// Request the content of the .mim file with the given hash.
    Get(Blake3Hash),
    /// Upload the given .mim file with the given hash and content.
    Upload(Vec<u8>),
}

/// Responses are simply the raw bytes of the .mim file, or empty if not found.
#[derive(bincode::Encode, bincode::Decode, Debug)]
enum Response {
    NotFound,
    MimFile(Vec<u8>),

    UploadOk,
    UploadErr(String),
}

pub fn server(socket_path: &Path, dir: &Path) -> std::io::Result<()> {
    let mut server_state = ServerState::new(dir);

    // Create a unix socket.
    // Remove existing socket if present
    let listener = UnixListener::bind(socket_path)?;
    // Iterate synchronously over incoming connections.
    // TODO: Multithreading.
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let request =
            bincode::decode_from_std_read(&mut stream, bincode::config::legacy()).unwrap();
        debug!("Received request: {:?}", request);
        let response = server_state.handle_request(request);
        debug!("Sending response: {:?}", response);

        stream
            .write_all(&bincode::encode_to_vec(&response, bincode::config::legacy()).unwrap())
            .unwrap();
    }
    Ok(())
}

struct ServerState {
    dir: PathBuf,
    cache: Cache,
}

impl ServerState {
    fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
            cache: read_cache(dir),
        }
    }
    fn handle_request(&mut self, request: Request) -> Response {
        match request {
            Request::Get(hash) => {
                debug!("Looking for hash {:?} in cache.", hash);
                if let Some(path) = self.cache.get(&hash) {
                    let mim_data = std::fs::read(path).unwrap();
                    Response::MimFile(mim_data)
                } else {
                    Response::NotFound
                }
            }
            Request::Upload(mim_data) => {
                let hash = mim::types::MimIndex::read_hash_from_std_read(&mim_data[..]).unwrap();
                let hash2 = mim::types::MimIndex::read_reader(&mim_data[..])
                    .unwrap()
                    .input_hash;
                debug!("hash:  {:?}\nhash2: {:?}.", hash, hash2);
                if self.cache.contains_key(&hash) {
                    Response::UploadErr(format!("Hash {:?} already exists in cache.", hash))
                } else {
                    let path = self
                        .dir
                        .join(BASE64.encode(hash))
                        .with_added_extension("mim");
                    eprintln!("uploading to path {:?}", path);
                    std::fs::write(&path, mim_data).unwrap();
                    self.cache.insert(hash, path);
                    Response::UploadOk
                }
            }
        }
    }
}

/// The cache is a directory with `hash.mim` files.
///
/// `hash` is the base64 encoded blake3 hash.
fn read_cache(dir: &Path) -> Cache {
    // Walk over all .mim files in the directory.
    let mut cache = Cache::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().unwrap_or_default() == "mim" {
            let file_name = path.file_stem().unwrap().to_str().unwrap();
            let hash = BASE64.decode(file_name).unwrap();
            assert_eq!(hash.len(), 32, "Invalid hash length in file name.");
            let hash: Blake3Hash = hash.try_into().unwrap();
            debug!("Added hash {:?} to cache with path {:?}.", hash, path);
            assert_eq!(
                cache.insert(hash, path),
                None,
                "Duplicate .mim files with hash {:?}.",
                hash
            );
        }
    }
    cache
}

pub fn download_mim(gz_path: &Path, socket_path: &Path) -> Option<Vec<u8>> {
    let hash = mim::hash_gz_file(gz_path);

    let request = Request::Get(hash);
    let response = make_request(request, socket_path);
    match response {
        Response::NotFound => None,
        Response::MimFile(content) => Some(content),
        _ => panic!("Unexpected response from server."),
    }
}

pub fn upload_mim(index_path: &Path, socket_path: &Path) {
    let mim_data = std::fs::read(index_path).unwrap();
    let request = Request::Upload(mim_data);
    let response = make_request(request, socket_path);
    match response {
        Response::UploadOk => (),
        Response::UploadErr(err) => panic!("Failed to upload .mim file: {}", err),
        _ => panic!("Unexpected response from server."),
    }
}

fn make_request(request: Request, socket_path: &Path) -> Response {
    debug!("Making request: {:?}", request);
    let mut stream = std::os::unix::net::UnixStream::connect(socket_path).unwrap();
    bincode::encode_into_std_write(&request, &mut stream, bincode::config::legacy()).unwrap();
    let response = bincode::decode_from_std_read(&mut stream, bincode::config::legacy()).unwrap();
    debug!("Received response: {:?}", response);
    response
}
