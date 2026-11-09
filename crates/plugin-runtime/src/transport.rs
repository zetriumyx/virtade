//! Persistent JSON-RPC resources. No shell or arbitrary ambient IPC is exposed to guests.
use crate::ResourceRequest;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Semaphore, oneshot};
use virt::Context;
use virt_protocol::Capability;
use virtade_agent_core::rpc::{ProcessConfig, RpcClient};
#[derive(Default)]
pub(crate) struct Transports {
    entries: Mutex<BTreeMap<String, Arc<Connection>>>,
}
struct Connection {
    owner: String,
    program: String,
    cwd: String,
    client: RpcClient,
    queue: Mutex<Queue>,
    requests: Arc<Semaphore>,
}
#[derive(Default)]
struct Queue {
    next: u64,
    bytes: usize,
    events: VecDeque<Value>,
    closed: Option<String>,
}
impl Connection {
    fn push(&self, frame: Value) {
        let mut q = self.queue.lock().expect("RPC queue");
        if q.closed.is_some() {
            return;
        }
        let size = frame.to_string().len() + 64;
        if q.events.len() >= 128 || q.bytes + size > 2 * 1024 * 1024 {
            q.closed = Some(
                "RPC consumer fell behind; connection closed, task outcome may be unknown".into(),
            );
            self.client.close();
            return;
        }
        q.next += 1;
        let seq = q.next;
        let event = json!({"sequence":seq,"frame":frame});
        q.bytes += event.to_string().len();
        q.events.push_back(event);
    }
    fn authorize(&self, c: &Context) -> Result<(), String> {
        if self.owner != c.owner() {
            return Err("RPC connection belongs to another plugin".into());
        }
        c.granted(Capability::ProcessCommand(self.program.clone()))
            .map_err(|e| e.to_string())?;
        c.granted(Capability::FilesystemRead(self.cwd.clone()))
            .map_err(|e| e.to_string())
    }
}
impl Transports {
    pub(crate) async fn execute(
        self: &Arc<Self>,
        context: Context,
        request: ResourceRequest,
    ) -> Result<Value, String> {
        if let ResourceRequest::RpcOpen {
            program,
            cwd,
            args,
            json_rpc_2,
        } = request
        {
            context
                .granted(Capability::ProcessCommand(program.clone()))
                .map_err(|e| e.to_string())?;
            context
                .granted(Capability::FilesystemRead(cwd.clone()))
                .map_err(|e| e.to_string())?;
            if !Path::new(&program).is_absolute()
                || !Path::new(&cwd).is_absolute()
                || args.len() > 128
                || args.iter().map(String::len).sum::<usize>() > 65536
            {
                return Err("RPC program/cwd must be absolute; argument limits apply".into());
            }
            if Path::new(&cwd).canonicalize().map_err(|e| e.to_string())? != Path::new(&cwd) {
                return Err("RPC cwd must be canonical".into());
            }
            let id = uuid::Uuid::new_v4().to_string();
            let connection = {
                let mut entries = self.entries.lock().expect("RPC resources");
                if entries.len() >= 16
                    || entries
                        .values()
                        .filter(|v| v.owner == context.owner())
                        .count()
                        >= 4
                {
                    return Err("RPC connection quota exceeded".into());
                }
                let mut config = ProcessConfig::new(&program, &cwd);
                config.args = args.into_iter().map(Into::into).collect();
                config.json_rpc_2 = json_rpc_2;
                config.queue_capacity = 16;
                config.max_frame_bytes = 256 * 1024;
                // Preserve only process launch/profile essentials. Do not inherit API keys or proxy credentials.
                config.inherit_env.retain(|key| {
                    [
                        "PATH",
                        "HOME",
                        "USERPROFILE",
                        "APPDATA",
                        "LOCALAPPDATA",
                        "SystemRoot",
                        "TEMP",
                        "TMP",
                        "LANG",
                    ]
                    .contains(&key.as_str())
                });
                let (client, mut events) = RpcClient::spawn(config).map_err(|e| e.to_string())?;
                let connection = Arc::new(Connection {
                    owner: context.owner().into(),
                    program,
                    cwd,
                    client,
                    queue: Mutex::default(),
                    requests: Arc::new(Semaphore::new(16)),
                });
                entries.insert(id.clone(), connection.clone());
                let registry = Arc::downgrade(self);
                let cleanup_id = id.clone();
                if let Err(error) = context.defer(move || {
                    if let Some(registry) = registry.upgrade()
                        && let Some(c) = registry
                            .entries
                            .lock()
                            .expect("RPC resources")
                            .remove(&cleanup_id)
                    {
                        c.client.close();
                    }
                }) {
                    entries.remove(&id);
                    connection.client.close();
                    return Err(error.to_string());
                }
                let pump = connection.clone();
                if let Err(error) = context.spawn(async move {
                    loop {
                        match events.recv().await {
                            Ok(frame) => {
                                pump.push(frame);
                                if pump.queue.lock().expect("RPC queue").closed.is_some() {
                                    break;
                                }
                            }
                            Err(error) => {
                                let mut q = pump.queue.lock().expect("RPC queue");
                                if q.closed.is_none() {
                                    q.closed = Some(error.to_string());
                                }
                                break;
                            }
                        }
                    }
                }) {
                    entries.remove(&id);
                    connection.client.close();
                    return Err(error.to_string());
                }
                connection
            };
            connection.authorize(&context)?;
            return Ok(json!({"id":id}));
        }
        let id = match &request {
            ResourceRequest::RpcStart { id, .. }
            | ResourceRequest::RpcNotify { id, .. }
            | ResourceRequest::RpcRespond { id, .. }
            | ResourceRequest::RpcPoll { id, .. }
            | ResourceRequest::RpcClose { id } => id,
            _ => return Err("not an RPC operation".into()),
        };
        let connection = self
            .entries
            .lock()
            .expect("RPC resources")
            .get(id)
            .cloned()
            .ok_or("RPC connection is closed or unknown")?;
        connection.authorize(&context)?;
        match request {
            ResourceRequest::RpcStart {
                method,
                params,
                timeout_ms,
                ..
            } => {
                if method.is_empty()
                    || method.len() > 200
                    || !(50..=900_000).contains(&timeout_ms)
                    || params.to_string().len() > 256 * 1024 - 1024
                {
                    return Err("invalid RPC method, payload or deadline".into());
                }
                let permit = connection
                    .requests
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| "RPC pending request quota exceeded")?;
                let operation = uuid::Uuid::new_v4().to_string();
                let op = operation.clone();
                let c = connection.clone();
                let (started, ready) = oneshot::channel();
                context
                    .spawn(async move {
                        let _permit = permit;
                        let result = c
                            .client
                            .request_started(
                                &method,
                                params,
                                Duration::from_millis(timeout_ms),
                                started,
                            )
                            .await;
                        let frame = match result {
                            Ok(result) => json!({"operation_id":op,"result":result}),
                            Err(error) => json!({"operation_id":op,"error":error.to_string()}),
                        };
                        c.push(frame);
                    })
                    .map_err(|e| e.to_string())?;
                if !matches!(
                    tokio::time::timeout(Duration::from_millis(500), ready).await,
                    Ok(Ok(()))
                ) {
                    connection.client.close();
                    return Err("RPC request failed to enter writer queue; connection closed, outcome may be unknown".into());
                }
                Ok(json!({"operation_id":operation}))
            }
            ResourceRequest::RpcNotify { method, params, .. } => {
                if method.is_empty() || method.len() > 200 {
                    return Err("invalid RPC method".into());
                }
                tokio::time::timeout(
                    Duration::from_millis(500),
                    connection.client.notify(&method, params),
                )
                .await
                .map_err(|_| "RPC writer timed out")?
                .map_err(|e| e.to_string())?;
                Ok(Value::Null)
            }
            ResourceRequest::RpcRespond {
                request_id,
                result,
                error,
                ..
            } => {
                if !(request_id.is_string() || request_id.is_number())
                    || (error.is_some() && !result.is_null())
                {
                    return Err(
                        "invalid RPC response identity or mutually exclusive result/error".into(),
                    );
                }
                let response = if let Some(error) = &error {
                    Err((
                        error["code"].as_i64().unwrap_or(-32603),
                        error["message"].as_str().unwrap_or("plugin error"),
                    ))
                } else {
                    Ok(result)
                };
                tokio::time::timeout(
                    Duration::from_millis(500),
                    connection.client.respond(request_id, response),
                )
                .await
                .map_err(|_| "RPC response writer timed out")?
                .map_err(|e| e.to_string())?;
                Ok(Value::Null)
            }
            ResourceRequest::RpcPoll { after, .. } => {
                let mut q = connection.queue.lock().expect("RPC queue");
                if after > q.next {
                    return Err("RPC cursor is ahead of the connection".into());
                }
                while q
                    .events
                    .front()
                    .is_some_and(|e| e["sequence"].as_u64().unwrap_or(0) <= after)
                {
                    if let Some(event) = q.events.pop_front() {
                        q.bytes = q.bytes.saturating_sub(event.to_string().len());
                    }
                }
                Ok(json!({"events":q.events.iter().take(64).collect::<Vec<_>>(),"closed":q.closed}))
            }
            ResourceRequest::RpcClose { id } => {
                self.entries.lock().expect("RPC resources").remove(&id);
                connection
                    .client
                    .shutdown()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(Value::Null)
            }
            _ => unreachable!(),
        }
    }
}
