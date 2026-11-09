use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::AsyncReadExt;
use virt::{Context, Contribution, ContributionKind, Plugin, Service, async_trait};
use virt_protocol::{Capability, Error, Manifest, Result, ServiceKey};
use virtade_storage::Store;

pub(crate) fn key() -> ServiceKey {
    ServiceKey::new("virtade.workbench.internal", 1)
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum WorkbenchRequest {
    ListFiles {
        path: String,
    },
    ReadFile {
        path: String,
    },
    WriteFile {
        path: String,
        content: String,
        expected: String,
    },
    GitStatus,
    GitDiff {
        path: Option<String>,
        staged: bool,
    },
    GitWorktrees,
    GitFile {
        path: String,
        index: bool,
    },
    GitInit,
    GitStage {
        path: String,
    },
    GitUnstage {
        path: String,
    },
    GitCommit {
        message: String,
    },
    WorktreeAdd {
        path: PathBuf,
        branch: String,
    },
    WorktreeRemove {
        path: PathBuf,
    },
    TerminalOpen {
        cols: u16,
        rows: u16,
    },
    TerminalPoll {
        id: String,
        after: u64,
    },
    TerminalWrite {
        id: String,
        data: String,
    },
    TerminalResize {
        id: String,
        cols: u16,
        rows: u16,
    },
    TerminalClose {
        id: String,
    },
}
#[derive(Deserialize)]
struct Envelope {
    workspace_id: String,
    request: WorkbenchRequest,
}
#[derive(Default)]
struct Output {
    base: u64,
    next: u64,
    bytes: VecDeque<u8>,
}
struct Terminal {
    workspace_id: String,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    output: Arc<Mutex<Output>>,
    reader: Option<std::thread::JoinHandle<()>>,
}
impl Terminal {
    fn terminate(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            #[cfg(unix)]
            if let Some(pid) = self.child.process_id() {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
            #[cfg(windows)]
            if let Some(pid) = self.child.process_id() {
                let _ = std::process::Command::new("taskkill.exe")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        self.terminate();
    }
}
pub(crate) struct WorkbenchPlugin {
    store: Store,
    terminals: Arc<Mutex<HashMap<String, Terminal>>>,
}
impl WorkbenchPlugin {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            terminals: Arc::default(),
        }
    }
}
fn failure(error: impl std::fmt::Display) -> Error {
    Error::PluginFailure {
        plugin: "virtade-workbench".into(),
        message: error.to_string(),
    }
}
#[async_trait]
impl Plugin for WorkbenchPlugin {
    fn manifest(&self) -> Manifest {
        let mut manifest = Manifest::new("virtade-workbench");
        manifest
            .permissions
            .insert(Capability::ServiceProvide(key()));
        manifest
            .permissions
            .insert(Capability::Contribution("panel".into()));
        manifest
    }
    async fn start(&self, context: Context) -> Result<()> {
        context.provide_private(
            key(),
            Arc::new(WorkbenchService {
                store: self.store.clone(),
                terminals: self.terminals.clone(),
            }),
        )?;
        for (view, title) in [
            ("chat", "Agent 对话"),
            ("files", "文件浏览"),
            ("git", "Git 变更"),
            ("terminal", "终端"),
            ("plugins", "插件管理"),
        ] {
            context.contribute(Contribution {
                id: format!("virtade-workbench.{view}"),
                kind: ContributionKind::Panel,
                title: title.into(),
                schema_version: 1,
                spec: json!({"type":"builtin","view":view}),
            })?;
        }
        let terminals = self.terminals.clone();
        context.defer(move || {
            terminals.lock().expect("terminals").clear();
        })
    }
    async fn stop(&self, _: Context) -> Result<()> {
        let terminals = self.terminals.clone();
        tokio::task::spawn_blocking(move || terminals.lock().expect("terminals").clear())
            .await
            .map_err(failure)?;
        Ok(())
    }
}
struct WorkbenchService {
    store: Store,
    terminals: Arc<Mutex<HashMap<String, Terminal>>>,
}
fn relative(path: &str, empty: bool) -> Result<()> {
    if (path.is_empty() && !empty)
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        Err(failure(
            "Workspace paths must be relative and cannot escape the project",
        ))
    } else {
        Ok(())
    }
}
fn scoped(root: &Path) -> Result<cap_std::fs::Dir> {
    cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority()).map_err(failure)
}
#[async_trait]
impl Service for WorkbenchService {
    async fn call(&self, request: Value) -> Result<Value> {
        let envelope: Envelope = serde_json::from_value(request).map_err(failure)?;
        let workspace = self
            .store
            .workspaces()
            .await
            .map_err(failure)?
            .into_iter()
            .find(|workspace| workspace.id == envelope.workspace_id)
            .ok_or_else(|| failure("Workspace not found"))?;
        match envelope.request {
            WorkbenchRequest::GitInit => git(workspace.root, vec!["init".into()]).await,
            WorkbenchRequest::GitFile { path, index } => {
                relative(&path, false)?;
                git(
                    workspace.root,
                    vec![
                        "show".into(),
                        format!("{}:{path}", if index { "" } else { "HEAD" }),
                    ],
                )
                .await
            }
            WorkbenchRequest::GitStatus => {
                git(
                    workspace.root,
                    vec![
                        "status".into(),
                        "--porcelain=v1".into(),
                        "-z".into(),
                        "--branch".into(),
                    ],
                )
                .await
            }
            WorkbenchRequest::GitWorktrees => {
                git(
                    workspace.root,
                    vec!["worktree".into(), "list".into(), "--porcelain".into()],
                )
                .await
            }
            WorkbenchRequest::GitDiff { path, staged } => {
                let mut args = vec!["diff".into(), "--no-ext-diff".into(), "--no-color".into()];
                if staged {
                    args.push("--cached".into());
                }
                if let Some(path) = path {
                    relative(&path, false)?;
                    args.push("--".into());
                    args.push(path);
                }
                git(workspace.root, args).await
            }
            WorkbenchRequest::GitStage { path } => {
                relative(&path, false)?;
                git(workspace.root, vec!["add".into(), "--".into(), path]).await
            }
            WorkbenchRequest::GitUnstage { path } => {
                relative(&path, false)?;
                git(
                    workspace.root,
                    vec!["restore".into(), "--staged".into(), "--".into(), path],
                )
                .await
            }
            WorkbenchRequest::GitCommit { message } => {
                if message.trim().is_empty() || message.len() > 8192 {
                    return Err(failure("A bounded commit message is required"));
                }
                git(workspace.root, vec!["commit".into(), "-m".into(), message]).await
            }
            WorkbenchRequest::WorktreeAdd { path, branch } => {
                if !path.is_absolute() || branch.starts_with('-') || branch.is_empty() {
                    return Err(failure(
                        "Absolute worktree path and a branch name are required",
                    ));
                }
                git(
                    workspace.root,
                    vec![
                        "worktree".into(),
                        "add".into(),
                        "-b".into(),
                        branch,
                        "--".into(),
                        path.to_string_lossy().into_owned(),
                    ],
                )
                .await
            }
            WorkbenchRequest::WorktreeRemove { path } => {
                if !path.is_absolute() {
                    return Err(failure("Absolute worktree path required"));
                }
                git(
                    workspace.root,
                    vec![
                        "worktree".into(),
                        "remove".into(),
                        "--".into(),
                        path.to_string_lossy().into_owned(),
                    ],
                )
                .await
            }
            operation => {
                let terminals = self.terminals.clone();
                let owner = workspace.id;
                tokio::task::spawn_blocking(move||match operation{
                    WorkbenchRequest::ListFiles{path}=>{relative(&path,true)?;let dir=scoped(&workspace.root)?;let dir=if path.is_empty(){dir}else{dir.open_dir(path).map_err(failure)?};let mut entries=Vec::new();for entry in dir.entries().map_err(failure)?.take(5000){let entry=entry.map_err(failure)?;let name=entry.file_name().to_string_lossy().into_owned();if name==".git"||name=="node_modules"||name=="target"{continue;}let kind=entry.file_type().map_err(failure)?;entries.push(json!({"name":name,"directory":kind.is_dir(),"symlink":kind.is_symlink()}));}entries.sort_by(|a,b|b["directory"].as_bool().cmp(&a["directory"].as_bool()).then(a["name"].as_str().cmp(&b["name"].as_str())));Ok(json!(entries))},
                    WorkbenchRequest::ReadFile{path}=>{relative(&path,false)?;let dir=scoped(&workspace.root)?;let file=dir.open(path).map_err(failure)?;let mut bytes=Vec::new();file.take(4*1024*1024+1).read_to_end(&mut bytes).map_err(failure)?;if bytes.len()>4*1024*1024{return Err(failure("Editor files are limited to 4 MiB"));}let digest=format!("{:x}",Sha256::digest(&bytes));let text=String::from_utf8(bytes).map_err(|_|failure("This file is binary or not UTF-8"))?;Ok(json!({"content":text,"version":digest}))},
                    WorkbenchRequest::WriteFile{path,content,expected}=>{relative(&path,false)?;if content.len()>4*1024*1024{return Err(failure("Editor files are limited to 4 MiB"));}let dir=scoped(&workspace.root)?;if dir.symlink_metadata(&path).map_err(failure)?.file_type().is_symlink(){return Err(failure("Saving through a symbolic link is not supported; open the target file"));}let file=dir.open(&path).map_err(failure)?;let mut bytes=Vec::new();file.take(4*1024*1024+1).read_to_end(&mut bytes).map_err(failure)?;if format!("{:x}",Sha256::digest(&bytes))!=expected{return Err(failure("File changed outside the editor; reload before saving"));}let temporary=format!("{path}.virt-{}.tmp",uuid::Uuid::new_v4());let mut file=dir.open_with(&temporary,cap_std::fs::OpenOptions::new().write(true).create_new(true)).map_err(failure)?;let result=(||{file.write_all(content.as_bytes()).map_err(failure)?;file.sync_all().map_err(failure)?;dir.rename(&temporary,&dir,&path).map_err(failure)})();if result.is_err(){let _=dir.remove_file(temporary);}result?;Ok(json!({"version":format!("{:x}",Sha256::digest(content.as_bytes()))}))},
                    WorkbenchRequest::TerminalOpen{cols,rows}=>{let mut terminals=terminals.lock().expect("terminals");if terminals.len()>=16{return Err(failure("Terminal limit reached"));}let pair=native_pty_system().openpty(size(cols,rows)?).map_err(failure)?;let shell=if cfg!(windows){"powershell.exe".into()}else{std::env::var("SHELL").unwrap_or_else(|_|"/bin/bash".into())};let mut command=CommandBuilder::new(shell);command.cwd(workspace.root);let child=pair.slave.spawn_command(command).map_err(failure)?;drop(pair.slave);let writer=pair.master.take_writer().map_err(failure)?;let mut reader=pair.master.try_clone_reader().map_err(failure)?;let output=Arc::new(Mutex::new(Output::default()));let buffer=output.clone();let read_thread=std::thread::spawn(move||{let mut bytes=[0u8;4096];loop{match reader.read(&mut bytes){Ok(0)|Err(_)=>break,Ok(size)=>{let mut output=buffer.lock().expect("PTY output");output.bytes.extend(&bytes[..size]);output.next+=size as u64;while output.bytes.len()>1024*1024{output.bytes.pop_front();output.base+=1;}}}}});let id=uuid::Uuid::new_v4().to_string();terminals.insert(id.clone(),Terminal{workspace_id:owner,master:pair.master,child,writer,output,reader:Some(read_thread)});Ok(json!({"id":id}))},
                    WorkbenchRequest::TerminalPoll{id,after}=>{let mut terminals=terminals.lock().expect("terminals");let terminal=owned(&mut terminals,&id,&owner)?;let output=terminal.output.lock().expect("PTY output");let start=after.max(output.base).min(output.next);let bytes:Vec<_>=output.bytes.iter().skip((start-output.base)as usize).take(65536).copied().collect();let cursor=start+bytes.len()as u64;Ok(json!({"cursor":cursor,"bytes":bytes,"dropped":after<output.base,"exited":terminal.child.try_wait().map_err(failure)?.map(|status|status.exit_code())}))},
                    WorkbenchRequest::TerminalWrite{id,data}=>{if data.len()>65536{return Err(failure("Terminal input exceeds limits"));}let mut terminals=terminals.lock().expect("terminals");let terminal=owned(&mut terminals,&id,&owner)?;terminal.writer.write_all(data.as_bytes()).map_err(failure)?;terminal.writer.flush().map_err(failure)?;Ok(Value::Null)},
                    WorkbenchRequest::TerminalResize{id,cols,rows}=>{let mut terminals=terminals.lock().expect("terminals");owned(&mut terminals,&id,&owner)?.master.resize(size(cols,rows)?).map_err(failure)?;Ok(Value::Null)},
                    WorkbenchRequest::TerminalClose{id}=>{let mut terminals=terminals.lock().expect("terminals");owned(&mut terminals,&id,&owner)?;terminals.remove(&id);Ok(Value::Null)},
                    _=>Err(failure("Unsupported workbench operation")),
                }).await.map_err(failure)?
            }
        }
    }
}
fn owned<'a>(
    terminals: &'a mut HashMap<String, Terminal>,
    id: &str,
    owner: &str,
) -> Result<&'a mut Terminal> {
    let terminal = terminals
        .get_mut(id)
        .ok_or_else(|| failure("Terminal is closed"))?;
    if terminal.workspace_id != owner {
        return Err(failure("Terminal belongs to another workspace"));
    }
    Ok(terminal)
}
fn size(cols: u16, rows: u16) -> Result<PtySize> {
    if !(2..=500).contains(&cols) || !(2..=200).contains(&rows) {
        return Err(failure("Invalid terminal dimensions"));
    }
    Ok(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })
}
async fn git(root: PathBuf, args: Vec<String>) -> Result<Value> {
    let mut command = CommandWrap::with_new("git", |command| {
        command
            .args(args)
            .current_dir(root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    });
    command.wrap(KillOnDrop);
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(process_wrap::tokio::JobObject);
    let mut child = command.spawn().map_err(failure)?;
    let mut stdout = child
        .stdout()
        .take()
        .ok_or_else(|| failure("Git stdout unavailable"))?
        .take(4 * 1024 * 1024 + 1);
    let mut stderr = child
        .stderr()
        .take()
        .ok_or_else(|| failure("Git stderr unavailable"))?
        .take(65537);
    let operation = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (_, _, status) = tokio::try_join!(
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err),
            child.wait()
        )
        .map_err(failure)?;
        if out.len() > 4 * 1024 * 1024 || err.len() > 65536 {
            return Err(failure("Git output exceeds limits"));
        }
        if !status.success() {
            return Err(failure(String::from_utf8_lossy(&err)));
        }
        Ok(json!({"output":String::from_utf8_lossy(&out)}))
    };
    let result = tokio::time::timeout(Duration::from_secs(10), operation)
        .await
        .map_err(|_| failure("Git command timed out"))?;
    let _ = child.start_kill();
    let _ = child.wait().await;
    result
}
