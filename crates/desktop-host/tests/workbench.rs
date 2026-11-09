use serde_json::Value;
use std::{path::Path, time::Duration};
use tempfile::TempDir;
use virtade_desktop_host::{AppHost, DesktopRequest, WorkbenchRequest};

async fn setup() -> (TempDir, std::sync::Arc<AppHost>, String) {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let host = AppHost::open(temp.path().join("state.db")).await.unwrap();
    let workspace = host
        .request(1, DesktopRequest::OpenWorkspace { root })
        .await
        .unwrap();
    (temp, host, workspace["id"].as_str().unwrap().into())
}
async fn call(
    host: &AppHost,
    id: &str,
    request: WorkbenchRequest,
) -> Result<Value, virtade_desktop_host::HostError> {
    host.request(
        1,
        DesktopRequest::Workbench {
            workspace_id: id.into(),
            request,
        },
    )
    .await
}
fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn editor_rejects_escape_binary_and_stale_save_without_losing_changes() {
    let (temp, host, id) = setup().await;
    let root = temp.path().join("project");
    std::fs::write(root.join("code.rs"), "old").unwrap();
    std::fs::write(temp.path().join("secret"), "private").unwrap();
    let first = call(
        &host,
        &id,
        WorkbenchRequest::ReadFile {
            path: "code.rs".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(first["content"], "old");
    for path in ["../secret", "/etc/passwd"] {
        assert!(
            call(&host, &id, WorkbenchRequest::ReadFile { path: path.into() })
                .await
                .is_err()
        );
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(temp.path().join("secret"), root.join("escape")).unwrap();
        assert!(
            call(
                &host,
                &id,
                WorkbenchRequest::ReadFile {
                    path: "escape".into()
                }
            )
            .await
            .is_err()
        );
    }
    std::fs::write(root.join("code.rs"), "external edit").unwrap();
    assert!(
        call(
            &host,
            &id,
            WorkbenchRequest::WriteFile {
                path: "code.rs".into(),
                content: "overwrite".into(),
                expected: first["version"].as_str().unwrap().into()
            }
        )
        .await
        .is_err()
    );
    let current = call(
        &host,
        &id,
        WorkbenchRequest::ReadFile {
            path: "code.rs".into(),
        },
    )
    .await
    .unwrap();
    call(
        &host,
        &id,
        WorkbenchRequest::WriteFile {
            path: "code.rs".into(),
            content: "saved".into(),
            expected: current["version"].as_str().unwrap().into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("code.rs")).unwrap(),
        "saved"
    );
    std::fs::write(root.join("binary"), [0xff, 0xfe]).unwrap();
    assert!(
        call(
            &host,
            &id,
            WorkbenchRequest::ReadFile {
                path: "binary".into()
            }
        )
        .await
        .is_err()
    );
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_git_stage_diff_and_dirty_worktree_removal_preserve_files() {
    let (temp, host, id) = setup().await;
    let root = temp.path().join("project");
    call(&host, &id, WorkbenchRequest::GitInit).await.unwrap();
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(&root, &["config", "user.name", "Fixture"]);
    std::fs::write(root.join("code.txt"), "initial\n").unwrap();
    call(
        &host,
        &id,
        WorkbenchRequest::GitStage {
            path: "code.txt".into(),
        },
    )
    .await
    .unwrap();
    call(
        &host,
        &id,
        WorkbenchRequest::GitCommit {
            message: "initial".into(),
        },
    )
    .await
    .unwrap();
    std::fs::write(root.join("code.txt"), "changed\n").unwrap();
    let diff = call(
        &host,
        &id,
        WorkbenchRequest::GitDiff {
            path: Some("code.txt".into()),
            staged: false,
        },
    )
    .await
    .unwrap();
    assert!(diff["output"].as_str().unwrap().contains("+changed"));
    call(
        &host,
        &id,
        WorkbenchRequest::GitStage {
            path: "code.txt".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        call(
            &host,
            &id,
            WorkbenchRequest::GitFile {
                path: "code.txt".into(),
                index: true
            }
        )
        .await
        .unwrap()["output"],
        "changed\n"
    );
    let path = temp.path().join("branch");
    call(
        &host,
        &id,
        WorkbenchRequest::WorktreeAdd {
            path: path.clone(),
            branch: "test-branch".into(),
        },
    )
    .await
    .unwrap();
    std::fs::write(path.join("untracked"), "keep").unwrap();
    assert!(
        call(
            &host,
            &id,
            WorkbenchRequest::WorktreeRemove { path: path.clone() }
        )
        .await
        .is_err()
    );
    assert!(path.join("untracked").exists());
    std::fs::remove_file(path.join("untracked")).unwrap();
    call(
        &host,
        &id,
        WorkbenchRequest::WorktreeRemove { path: path.clone() },
    )
    .await
    .unwrap();
    assert!(!path.exists());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_pty_input_resize_owner_checks_and_close() {
    let (temp, host, id) = setup().await;
    let terminal = call(
        &host,
        &id,
        WorkbenchRequest::TerminalOpen { cols: 80, rows: 24 },
    )
    .await
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let other = temp.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let other = host
        .request(1, DesktopRequest::OpenWorkspace { root: other })
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        call(
            &host,
            &other,
            WorkbenchRequest::TerminalWrite {
                id: terminal.clone(),
                data: "wrong".into()
            }
        )
        .await
        .is_err()
    );
    call(
        &host,
        &id,
        WorkbenchRequest::TerminalResize {
            id: terminal.clone(),
            cols: 100,
            rows: 30,
        },
    )
    .await
    .unwrap();
    // The output literal differs from the input command, so PTY input echo cannot satisfy the assertion.
    let command = if cfg!(windows) {
        "Write-Output ('virt-' + 'pty-ok')\r\n"
    } else {
        "printf 'virt-%s\\n' 'pty-ok'\n"
    };
    call(
        &host,
        &id,
        WorkbenchRequest::TerminalWrite {
            id: terminal.clone(),
            data: command.into(),
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut bytes = Vec::new();
        let mut after = 0;
        loop {
            let output = call(
                &host,
                &id,
                WorkbenchRequest::TerminalPoll {
                    id: terminal.clone(),
                    after,
                },
            )
            .await
            .unwrap();
            after = output["cursor"].as_u64().unwrap();
            bytes.extend(
                output["bytes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_u64().unwrap() as u8),
            );
            if String::from_utf8_lossy(&bytes).contains("virt-pty-ok") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    call(
        &host,
        &id,
        WorkbenchRequest::TerminalClose {
            id: terminal.clone(),
        },
    )
    .await
    .unwrap();
    assert!(
        call(
            &host,
            &id,
            WorkbenchRequest::TerminalPoll {
                id: terminal,
                after: 0
            }
        )
        .await
        .is_err()
    );
    host.shutdown().await.unwrap();
}
