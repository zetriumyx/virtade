use process_wrap::tokio::{CommandWrap, KillOnDrop};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;
use virt::Context;
use virt_protocol::Capability;

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ResourceRequest {
    RpcOpen {
        program: String,
        cwd: String,
        args: Vec<String>,
        #[serde(default)]
        json_rpc_2: bool,
    },
    RpcStart {
        id: String,
        method: String,
        params: Value,
        timeout_ms: u64,
    },
    RpcNotify {
        id: String,
        method: String,
        params: Value,
    },
    RpcRespond {
        id: String,
        request_id: Value,
        #[serde(default)]
        result: Value,
        error: Option<Value>,
    },
    RpcPoll {
        id: String,
        after: u64,
    },
    RpcClose {
        id: String,
    },
    ReadFile {
        root: String,
        path: String,
    },
    WriteFile {
        root: String,
        path: String,
        content: String,
    },
    Fetch {
        url: String,
    },
    Run {
        program: String,
        cwd: String,
        args: Vec<String>,
    },
}
fn relative(path: &str) -> Result<(), String> {
    if path.is_empty()
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        Err("only scoped relative paths are accepted".into())
    } else {
        Ok(())
    }
}
pub(crate) async fn execute(
    context: Context,
    request: ResourceRequest,
    transports: std::sync::Arc<crate::transport::Transports>,
) -> Result<Value, String> {
    match request {
        request @ (ResourceRequest::RpcOpen { .. }
        | ResourceRequest::RpcStart { .. }
        | ResourceRequest::RpcNotify { .. }
        | ResourceRequest::RpcRespond { .. }
        | ResourceRequest::RpcPoll { .. }
        | ResourceRequest::RpcClose { .. }) => transports.execute(context, request).await,
        ResourceRequest::ReadFile { root, path } => {
            if !Path::new(&root).is_absolute() {
                return Err("filesystem permission root must be absolute".into());
            }
            let permission = Capability::FilesystemRead(root.clone());
            context
                .granted(permission.clone())
                .map_err(|error| error.to_string())?;
            relative(&path)?;
            let value = tokio::task::spawn_blocking(move || -> Result<String, String> {
                let dir = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())
                    .map_err(|error| error.to_string())?;
                let file = dir.open(path).map_err(|error| error.to_string())?;
                let mut value = String::new();
                file.take(1024 * 1024 + 1)
                    .read_to_string(&mut value)
                    .map_err(|error| error.to_string())?;
                if value.len() > 1024 * 1024 {
                    return Err("file exceeds 1 MiB".into());
                }
                Ok(value)
            })
            .await
            .map_err(|error| error.to_string())??;
            context
                .granted(permission)
                .map_err(|error| error.to_string())?;
            Ok(json!(value))
        }
        ResourceRequest::WriteFile {
            root,
            path,
            content,
        } => {
            if !Path::new(&root).is_absolute() {
                return Err("filesystem permission root must be absolute".into());
            }
            context
                .granted(Capability::FilesystemWrite(root.clone()))
                .map_err(|error| error.to_string())?;
            relative(&path)?;
            if content.len() > 1024 * 1024 {
                return Err("file exceeds 1 MiB".into());
            }
            tokio::task::spawn_blocking(move || -> Result<(), String> {
                let dir = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())
                    .map_err(|error| error.to_string())?;
                let temporary = format!("{}.virt-{}.tmp", path, uuid::Uuid::new_v4());
                let result = (|| {
                    let mut file = dir
                        .open_with(
                            &temporary,
                            cap_std::fs::OpenOptions::new().write(true).create_new(true),
                        )
                        .map_err(|error| error.to_string())?;
                    file.write_all(content.as_bytes())
                        .map_err(|error| error.to_string())?;
                    file.sync_all().map_err(|error| error.to_string())?;
                    dir.rename(&temporary, &dir, path)
                        .map_err(|error| error.to_string())
                })();
                if result.is_err() {
                    let _ = dir.remove_file(temporary);
                }
                result
            })
            .await
            .map_err(|error| error.to_string())??;
            Ok(Value::Null)
        }
        ResourceRequest::Fetch { url } => {
            let url = reqwest::Url::parse(&url).map_err(|error| error.to_string())?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err("HTTP(S) origin without embedded credentials required".into());
            }
            let origin = url.origin().ascii_serialization();
            context
                .granted(Capability::NetworkOrigin(origin))
                .map_err(|error| error.to_string())?;
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(1))
                .build()
                .map_err(|error| error.to_string())?;
            let mut response = client
                .get(url)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
                if bytes.len() + chunk.len() > 1024 * 1024 {
                    return Err("response exceeds 1 MiB".into());
                }
                bytes.extend(chunk);
            }
            Ok(
                json!({"status":status,"body":String::from_utf8(bytes).map_err(|error|error.to_string())?}),
            )
        }
        ResourceRequest::Run { program, cwd, args } => {
            context
                .granted(Capability::ProcessCommand(program.clone()))
                .map_err(|error| error.to_string())?;
            context
                .granted(Capability::FilesystemRead(cwd.clone()))
                .map_err(|error| error.to_string())?;
            if !Path::new(&program).is_absolute()
                || !Path::new(&cwd).is_absolute()
                || args.len() > 128
                || args.iter().map(String::len).sum::<usize>() > 65536
            {
                return Err("absolute executable and bounded argument vector required".into());
            }
            let mut command = CommandWrap::with_new(program, |command| {
                command
                    .args(args)
                    .current_dir(cwd)
                    .env_clear()
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
            });
            command.wrap(KillOnDrop);
            #[cfg(unix)]
            command.wrap(process_wrap::tokio::ProcessGroup::leader());
            #[cfg(windows)]
            command.wrap(process_wrap::tokio::JobObject);
            let mut child = command.spawn().map_err(|error| error.to_string())?;
            let stdout = child.stdout().take().ok_or("missing stdout")?;
            let stderr = child.stderr().take().ok_or("missing stderr")?;
            let output = async {
                let read = async move {
                    let mut out = Vec::new();
                    let mut err = Vec::new();
                    let mut stdout = stdout.take(1024 * 1024 + 1);
                    let mut stderr = stderr.take(65537);
                    tokio::try_join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err))
                        .map_err(|error| error.to_string())?;
                    if out.len() > 1024 * 1024 || err.len() > 65536 {
                        return Err("process output exceeds limits".into());
                    }
                    Ok::<_, String>((out, err))
                };
                let ((out, err), status) = tokio::try_join!(read, async {
                    child.wait().await.map_err(|error| error.to_string())
                })?;
                Ok::<_, String>(
                    json!({"code":status.code(),"stdout":String::from_utf8_lossy(&out),"stderr":String::from_utf8_lossy(&err)}),
                )
            };
            let result = tokio::time::timeout(Duration::from_secs(1), output)
                .await
                .unwrap_or_else(|_| Err("process deadline exceeded".to_owned()));
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
            result
        }
    }
}
