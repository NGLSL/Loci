//! Unprivileged, read-only client. The Windows service is the sole index owner.
use crate::{install, kite_plugin_sdk};
use loci_experiment::service::{self, Filter, Request, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

type Client = Arc<dyn Fn(&Request) -> io::Result<Response> + Send + Sync>;
#[derive(Deserialize)]
struct FileRequest {
    query: String,
    #[serde(default)]
    filter: Filter,
}
pub struct Service {
    client: Client,
}
impl Service {
    pub fn start(data_dir: PathBuf) -> io::Result<Self> {
        if !data_dir.is_absolute() {
            return Err(io::Error::other("data_dir must be absolute"));
        }
        Ok(Self {
            client: Arc::new(|request| {
                service::request_with_timeout(request, Duration::from_millis(500))
            }),
        })
    }
    pub fn execute(&self, action: &str, payload: &Value) -> Result<Value, String> {
        install::validate_action(action, payload)?;
        match action {
            "install_service" => install::start(),
            "show_status" => Ok(self.query("status")),
            _ => unreachable!(),
        }
    }
    pub fn query(&self, query: &str) -> Value {
        let query = query.trim();
        if query.is_empty()
            || query == "status"
            || query == "save"
            || query == "rebuild"
            || query.starts_with("root ")
        {
            let response = (self.client)(&Request::Status {});
            let text = match &response {
                Ok(response) => describe(response),
                Err(error) => format!("Loci 后台服务不可用：{error}"),
            };
            let mut actions = vec![
                json!({"id":"show_status","label":"查看状态","action":{"type":"plugin","action_id":"show_status","payload":{}}}),
            ];
            if response.is_err() {
                actions.insert(0, json!({"id":"install_service","label":"安装服务（管理员授权）","default":true,"action":{"type":"plugin","action_id":"install_service","payload":{}}}));
            } else {
                actions[0]["default"] = json!(true);
                actions.push(json!({"id":"install_service","label":"更新服务（管理员授权）","default":false,"action":{"type":"plugin","action_id":"install_service","payload":{}}}));
            }
            let installation = install::status();
            return kite_plugin_sdk::panel_response(
                json!({"blocks":[{"type":"text","text":format!("{text}\n{installation}\n文件搜索由独立安装的 Loci Windows 后台服务提供。导入 Loci 插件包后，点击“安装服务（管理员授权）”；导入新版后可点击“更新服务（管理员授权）”。完成一次管理员授权安装后，点击“查看状态”。Kite 以普通权限查询。\n插件包和更新：https://github.com/NGLSL/Loci/releases\n后台服务索引 NTFS 卷；插件不自行扫描目录。"),"style":"normal"}],"actions":actions}),
            );
        }
        self.query_files(&json!({"query":query,"filter":"all"}).to_string())
    }
    pub fn query_files(&self, raw: &str) -> Value {
        let query = match serde_json::from_str::<FileRequest>(raw) {
            Ok(query) => query,
            Err(error) => return status_response(&format!("文件查询参数无效：{error}")),
        };
        let response = match (self.client)(&Request::Query {
            query: query.query,
            filter: query.filter,
            limit: 50,
        }) {
            Ok(response) => response,
            Err(error) => {
                eprintln!("Loci service: {error}");
                return status_response(&format!("Loci 后台服务不可用：{error}；点击查看安装指引"));
            }
        };
        let mut items = Vec::new();
        if response.pending
            || !response.ready
            || response.error.is_some()
            || response.skipped_invalid_names > 0
        {
            let mut status = status_item(&describe(&response));
            if response.pending && response.error.is_none() {
                status["id"] = json!("loci-pending");
            }
            items.push(status);
        }
        for (index, file) in response.files.iter().enumerate() {
            items.push(json!({"id":format!("file-{index}"),"title":file.name,"subtitle":file.path,"priority":50,"action":{"type":"open_file","path":file.path}}));
        }
        kite_plugin_sdk::list_response(json!(items))
    }
}
fn describe(response: &Response) -> String {
    if let Some(error) = &response.error {
        let detail = if response.pending {
            "旧结果可能过时"
        } else {
            "结果可能不完整"
        };
        format!("Loci 文件搜索：{error}；{detail}")
    } else if response.pending {
        "Loci 正在建立或校正索引，结果可能不完整".into()
    } else if response.skipped_invalid_names > 0 {
        format!(
            "Loci 跳过了 {} 个无法安全显示的名称，结果不完整",
            response.skipped_invalid_names
        )
    } else if response.ready {
        let qualifier = if response.total_exact { "" } else { "至少 " };
        format!(
            "Loci 已就绪：{qualifier}{} 个目录项，索引版本 {}",
            response.total, response.version
        )
    } else {
        "Loci 索引尚未就绪，点击查看后台服务状态".into()
    }
}
fn status_item(text: &str) -> Value {
    json!({"id":"loci-status","title":text,"subtitle":"点击打开 Loci 服务状态","priority":100,"action":{"type":"plugin_action","action_id":"enter_provider","payload":{"provider":"files","command_id":"open"}}})
}
fn status_response(text: &str) -> Value {
    kite_plugin_sdk::list_response(json!([status_item(text)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use loci_experiment::service::FileResult;
    use std::sync::Mutex;
    use std::time::Instant;
    #[test]
    fn first_query_returns_ready_service_files_and_forwards_filter_reserved_words() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let calls = captured.clone();
        let service = Service {
            client: Arc::new(move |request| {
                calls
                    .lock()
                    .unwrap()
                    .push(serde_json::to_value(request).unwrap());
                Ok(Response {
                    ready: true,
                    pending: false,
                    error: None,
                    version: 7,
                    total: 1_000_000,
                    total_exact: true,
                    skipped_invalid_names: 0,
                    files: vec![FileResult {
                        path: r"C:\Fixture\status.png".into(),
                        name: "status.png".into(),
                        is_directory: false,
                    }],
                })
            }),
        };
        let raw = json!({"query":"status","filter":"images"}).to_string();
        assert_eq!(service.query_files(&raw)["items"][0]["title"], "status.png");
        assert_eq!(
            captured.lock().unwrap()[0],
            json!({"op":"query","query":"status","filter":"images","limit":50})
        );
        assert!(service.query("status").to_string().contains("1000000"));
        assert!(service
            .execute("configure", &json!({"root":"C:\\"}))
            .is_err());
    }
    #[test]
    fn unavailable_service_is_explicit_without_local_index_or_config() {
        let service = Service {
            client: Arc::new(|_| {
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "service not installed",
                ))
            }),
        };
        let before = Instant::now();
        let result = service.query_files(&json!({"query":"root C:\\","filter":"all"}).to_string());
        assert!(before.elapsed() < Duration::from_millis(100));
        assert!(result["items"][0]["title"]
            .as_str()
            .unwrap()
            .contains("后台服务不可用"));
        assert!(service
            .query("status")
            .to_string()
            .contains("service not installed"));
        let guide = service.query("status").to_string();
        assert!(guide.contains("独立安装"));
        assert!(guide.contains("https://github.com/NGLSL/Loci/releases"));
        let panel = service.query("status");
        assert_eq!(
            panel["panel"]["actions"][0]["action"]["action_id"],
            "install_service"
        );
        assert_eq!(panel["panel"]["actions"][0]["action"]["payload"], json!({}));
        assert!(panel["panel"]["actions"][0]["label"]
            .as_str()
            .unwrap()
            .contains("管理员授权"));
        assert_eq!(
            panel["panel"]["actions"][1]["action"]["action_id"],
            "show_status"
        );
        assert_eq!(
            service.execute("show_status", &json!({})).unwrap()["type"],
            "panel"
        );
        assert!(service
            .execute("show_status", &json!({"execute":"cmd.exe"}))
            .is_err());
    }
    #[test]
    fn pending_and_failed_service_views_are_labeled_with_old_files() {
        let service = Service {
            client: Arc::new(|_| {
                Ok(Response {
                    ready: true,
                    pending: true,
                    error: Some("journal lost".into()),
                    version: 3,
                    total: 42,
                    total_exact: false,
                    skipped_invalid_names: 0,
                    files: vec![FileResult {
                        path: r"C:\old.txt".into(),
                        name: "old.txt".into(),
                        is_directory: false,
                    }],
                })
            }),
        };
        let result = service.query_files(&json!({"query":"old","filter":"documents"}).to_string());
        assert_eq!(result["items"].as_array().unwrap().len(), 2);
        assert_eq!(result["items"][0]["id"], "loci-status");
        assert!(result["items"][0]["title"]
            .as_str()
            .unwrap()
            .contains("旧结果可能过时"));
        assert_eq!(result["items"][1]["action"]["type"], "open_file");
        let warning = Response {
            ready: true,
            pending: false,
            error: Some("IPC response byte budget omitted 1 result".into()),
            ..Response::pending()
        };
        assert!(describe(&warning).contains("结果可能不完整"));
        assert!(!describe(&warning).contains("旧结果可能过时"));
    }

    #[test]
    fn lower_bound_counts_and_skipped_names_are_not_presented_as_complete() {
        let mut response = Response::pending();
        response.ready = true;
        response.pending = false;
        response.total = 50;
        assert!(describe(&response).contains("至少 50"));
        response.skipped_invalid_names = 2;
        let service = Service {
            client: Arc::new(move |_| Ok(response.clone())),
        };
        let result = service.query_files(&json!({"query":"file","filter":"all"}).to_string());
        assert_eq!(result["items"][0]["id"], "loci-status");
        assert!(result["items"][0]["title"]
            .as_str()
            .unwrap()
            .contains("结果不完整"));
    }

    #[test]
    fn running_service_panel_offers_default_refresh_and_explicit_service_update() {
        let service = Service {
            client: Arc::new(|_| {
                Ok(Response {
                    ready: true,
                    pending: false,
                    ..Response::pending()
                })
            }),
        };
        let panel = service.query("status");
        assert_eq!(panel["panel"]["actions"].as_array().unwrap().len(), 2);
        assert_eq!(
            panel["panel"]["actions"][0]["action"]["action_id"],
            "show_status"
        );
        assert_eq!(panel["panel"]["actions"][0]["default"], true);
        let update = &panel["panel"]["actions"][1];
        assert_eq!(update["action"]["action_id"], "install_service");
        assert_eq!(update["action"]["payload"], json!({}));
        assert_eq!(update["label"], "更新服务（管理员授权）");
        assert_eq!(update["default"], false);
    }
}
