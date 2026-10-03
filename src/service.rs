//! Local read-only IPC for the privileged Windows index owner.
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::sync::{atomic::AtomicBool, Arc};

pub mod index_owner;
pub mod windows;
pub const MAX_FRAME: usize = 64 * 1024;
pub const MAX_RESULTS: usize = 100;
pub const PIPE_NAME: &str = r"\\.\pipe\Loci.Search.v1";
pub const SERVICE_NAME: &str = "LociIndex";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Query {
        query: String,
        #[serde(default)]
        filter: Filter,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    Status {},
}
fn default_limit() -> usize {
    20
}
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Filter {
    #[default]
    All,
    Images,
    Documents,
    Videos,
    Audio,
    Archives,
    Folders,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FileResult {
    pub path: String,
    pub name: String,
    pub is_directory: bool,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Response {
    pub ready: bool,
    pub pending: bool,
    pub error: Option<String>,
    pub version: u64,
    pub total: usize,
    #[serde(default)]
    pub total_exact: bool,
    #[serde(default)]
    pub skipped_invalid_names: usize,
    pub files: Vec<FileResult>,
}
impl Response {
    pub fn pending() -> Self {
        Self {
            ready: false,
            pending: true,
            error: None,
            version: 0,
            total: 0,
            total_exact: false,
            skipped_invalid_names: 0,
            files: vec![],
        }
    }
    pub fn error(error: impl Into<String>) -> Self {
        Self {
            pending: false,
            error: Some(error.into()),
            ..Self::pending()
        }
    }
}
/// The owner publishes snapshots; request handling never starts or mutates an index.
pub trait Backend: Send + Sync + 'static {
    fn start(self: Arc<Self>, _stopped: Arc<AtomicBool>) -> Option<std::thread::JoinHandle<()>> {
        None
    }
    fn query(&self, query: &str, filter: Filter, limit: usize) -> Response;
    fn status(&self) -> Response;
}
pub fn dispatch(backend: &dyn Backend, request: Request) -> Response {
    let response = match request {
        Request::Status {} => backend.status(),
        Request::Query {
            query,
            filter,
            limit,
        } => {
            if query.len() > 4096 || query.contains('\0') || !(1..=MAX_RESULTS).contains(&limit) {
                return Response::error("Invalid query or result limit");
            }
            backend.query(&query, filter, limit)
        }
    };
    bounded_response(response)
}
/// Budget complete JSON objects; never truncate a native path or UTF-8 scalar.
fn bounded_response(mut response: Response) -> Response {
    fn bounded_error(error: &mut Option<String>) {
        if let Some(error) = error {
            let mut end = error.len().min(4096);
            while !error.is_char_boundary(end) {
                end -= 1;
            }
            error.truncate(end);
        }
    }
    bounded_error(&mut response.error);
    let files = std::mem::take(&mut response.files);
    // Reserve room for the omission warning before collecting whole items.
    let mut used = serde_json::to_vec(&response).map_or(MAX_FRAME, |bytes| bytes.len()) + 512;
    let mut omitted = 0;
    for file in files {
        let size = serde_json::to_vec(&file).map_or(MAX_FRAME, |bytes| bytes.len() + 1);
        if size > MAX_FRAME.saturating_sub(used) {
            omitted += 1;
            continue;
        }
        used += size;
        response.files.push(file);
    }
    if omitted > 0 {
        response.total_exact = false;
        let warning=format!("{omitted} results omitted because they exceed the IPC byte budget; narrow the query or reduce the result limit");
        response.error = Some(match response.error.take() {
            Some(error) => format!("{warning}; {error}"),
            None => warning,
        });
        bounded_error(&mut response.error);
    }
    // This also makes long backend errors/status responses safe to frame.
    if serde_json::to_vec(&response).map_or(true, |bytes| bytes.len() > MAX_FRAME) {
        return Response::error(
            "Response exceeds the IPC byte budget; narrow the query or reduce the result limit",
        );
    }
    response
}
pub fn read_frame<T: serde::de::DeserializeOwned>(reader: &mut impl Read) -> io::Result<T> {
    let mut size = [0; 4];
    reader.read_exact(&mut size)?;
    let size = u32::from_le_bytes(size) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid frame size",
        ));
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Frame too large",
        ));
    }
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(&bytes)
}
pub fn run(backend: Arc<dyn Backend>, stopped: Arc<AtomicBool>) -> io::Result<()> {
    windows::run(backend, stopped)
}
pub fn request(request: &Request) -> io::Result<Response> {
    windows::request(request)
}
pub fn request_with_timeout(
    request: &Request,
    timeout: std::time::Duration,
) -> io::Result<Response> {
    windows::request_with_timeout(request, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Pending;
    impl Backend for Pending {
        fn query(&self, _: &str, _: Filter, _: usize) -> Response {
            Response::pending()
        }
        fn status(&self) -> Response {
            Response::pending()
        }
    }
    #[test]
    fn frame_bounds_and_read_only_protocol() {
        let mut frame = Vec::new();
        write_frame(&mut frame, &Request::Status {}).unwrap();
        assert!(matches!(
            read_frame::<Request>(&mut frame.as_slice()).unwrap(),
            Request::Status {}
        ));
        assert!(
            read_frame::<Request>(&mut ((MAX_FRAME + 1) as u32).to_le_bytes().as_slice()).is_err()
        );
        assert!(serde_json::from_str::<Request>(r#"{"op":"configure","root":"C:\\"}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"op":"status","execute":"cmd.exe"}"#).is_err());
        assert!(read_frame::<Request>(&mut frame[..frame.len() - 1].as_ref()).is_err());
    }
    #[test]
    fn query_validation_and_pending_do_not_require_scan() {
        assert!(dispatch(&Pending, Request::Status {}).pending);
        assert!(dispatch(
            &Pending,
            Request::Query {
                query: "a".into(),
                filter: Filter::All,
                limit: 101
            }
        )
        .error
        .is_some());
        assert!(
            dispatch(
                &Pending,
                Request::Query {
                    query: "a".into(),
                    filter: Filter::All,
                    limit: 20
                }
            )
            .pending
        );
    }
    #[test]
    fn long_result_pages_fit_frames_without_corrupting_paths() {
        for component in ["a".repeat(1300), "报告".repeat(650)] {
            let path = format!(r"C:\{component}.txt");
            let response = Response {
                ready: true,
                pending: false,
                error: None,
                version: 1,
                total: 50,
                total_exact: true,
                skipped_invalid_names: 0,
                files: (0..50)
                    .map(|_| FileResult {
                        path: path.clone(),
                        name: "file.txt".into(),
                        is_directory: false,
                    })
                    .collect(),
            };
            let response = bounded_response(response);
            assert!(response.ready);
            assert!(!response.pending);
            assert!(!response.total_exact);
            assert_eq!(response.total, 50);
            assert!(!response.files.is_empty());
            assert!(response.files.len() < 50);
            assert!(response.files.iter().all(|file| file.path == path));
            assert!(response.error.as_ref().unwrap().contains("IPC byte budget"));
            let mut frame = Vec::new();
            write_frame(&mut frame, &response).unwrap();
            let decoded: Response = read_frame(&mut frame.as_slice()).unwrap();
            assert_eq!(decoded.files.len(), response.files.len());
        }
    }
    #[test]
    fn oversized_single_result_does_not_drop_following_valid_result() {
        let response = Response {
            ready: true,
            pending: false,
            error: Some("错误".repeat(MAX_FRAME)),
            version: 1,
            total: 2,
            total_exact: true,
            skipped_invalid_names: 0,
            files: vec![
                FileResult {
                    path: format!(r"C:\{}", "报告".repeat(32760)),
                    name: "long".into(),
                    is_directory: false,
                },
                FileResult {
                    path: r"C:\normal.txt".into(),
                    name: "normal.txt".into(),
                    is_directory: false,
                },
            ],
        };
        let response = bounded_response(response);
        assert!(response.ready);
        assert_eq!(response.files.len(), 1);
        assert_eq!(response.files[0].path, r"C:\normal.txt");
        assert_eq!(response.skipped_invalid_names, 0);
        assert_eq!(response.total, 2);
        assert!(!response.total_exact);
        assert!(response.error.as_ref().unwrap().contains("IPC byte budget"));
        write_frame(&mut Vec::new(), &response).unwrap();
    }
}
