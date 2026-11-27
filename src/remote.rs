use crate::config::{AuthKind, ConnectionProfile, Protocol, expand_tilde};
use crate::fs_model::{FileEntry, format_unix_mode, remote_join, remote_parent, sort_entries};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use ssh2::{Session, Sftp};
use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;
use suppaftp::list::File as FtpListFile;
use suppaftp::native_tls::TlsConnector;
use suppaftp::types::FileType;
use suppaftp::{NativeTlsConnector, NativeTlsFtpStream};

#[derive(Debug, Clone)]
pub enum WorkerCommand {
    Connect {
        profile: ConnectionProfile,
        runtime_secret: Option<String>,
    },
    Disconnect,
    ListDir {
        path: String,
        show_hidden: bool,
    },
    Upload {
        local_path: std::path::PathBuf,
        remote_path: String,
        transfer_id: u64,
    },
    Download {
        remote_path: String,
        local_path: std::path::PathBuf,
        transfer_id: u64,
        total_size: u64,
    },
    Mkdir {
        remote_path: String,
    },
    Delete {
        remote_path: String,
        is_dir: bool,
    },
    Rename {
        from: String,
        to: String,
    },
    Preview {
        remote_path: String,
        max_bytes: usize,
    },
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum WorkerEvent {
    Log {
        level: LogLevel,
        message: String,
    },
    Connected {
        profile_name: String,
        remote_pwd: String,
        entries: Vec<FileEntry>,
    },
    Disconnected,
    DirListed {
        path: String,
        entries: Vec<FileEntry>,
    },
    TransferProgress {
        transfer_id: u64,
        done: u64,
        total: u64,
    },
    TransferFinished {
        transfer_id: u64,
        ok: bool,
        message: String,
    },
    OperationDone {
        message: String,
        refresh_remote: bool,
    },
    PreviewLoaded {
        title: String,
        bytes: Vec<u8>,
        total_size: Option<u64>,
        truncated: bool,
    },
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Success,
    Warn,
    Error,
}

enum ActiveClient {
    Sftp {
        _session: Session,
        sftp: Sftp,
    },
    Ftp(NativeTlsFtpStream),
}

impl ActiveClient {
    fn pwd(&mut self) -> Result<String> {
        match self {
            ActiveClient::Sftp { sftp, .. } => {
                let p = sftp.realpath(Path::new("."))?;
                Ok(p.to_string_lossy().to_string())
            }
            ActiveClient::Ftp(ftp) => Ok(ftp.pwd()?),
        }
    }

    fn list_dir(&mut self, path: &str, show_hidden: bool) -> Result<Vec<FileEntry>> {
        match self {
            ActiveClient::Sftp { sftp, .. } => {
                let mut entries = Vec::new();
                if path != "/" {
                    entries.push(FileEntry::parent(remote_parent(path)));
                }
                let raw = sftp
                    .readdir(Path::new(path))
                    .with_context(|| format!("Unable to read {path}"))?;

                for (entry_path, stat) in raw {
                    let Some(name) = entry_path.file_name().map(|s| s.to_string_lossy().to_string())
                    else {
                        continue;
                    };
                    if name == "." || name == ".." {
                        continue;
                    }
                    if !show_hidden && name.starts_with('.') {
                        continue;
                    }
                    let is_dir = stat.is_dir();
                    let is_symlink = stat.file_type().is_symlink();
                    let size = stat.size.unwrap_or(0);
                    let modified = stat
                        .mtime
                        .and_then(|ts| DateTime::<Utc>::from_timestamp(ts as i64, 0));
                    let permissions = stat
                        .perm
                        .map(|m| format_unix_mode(m, is_dir))
                        .unwrap_or_default();

                    entries.push(FileEntry {
                        name: name.clone(),
                        path: remote_join(path, &name),
                        is_dir,
                        is_symlink,
                        is_parent: false,
                        size,
                        modified,
                        permissions,
                    });
                }
                sort_entries(&mut entries);
                Ok(entries)
            }
            ActiveClient::Ftp(ftp) => {
                ftp.cwd(path)
                    .with_context(|| format!("Unable to change directory to {path}"))?;
                let lines = ftp
                    .list(None)
                    .with_context(|| format!("LIST failed for {path}"))?;
                let mut entries = Vec::new();
                if path != "/" {
                    entries.push(FileEntry::parent(remote_parent(path)));
                }
                for line in lines {
                    if let Ok(parsed) = FtpListFile::try_from(line.as_str()) {
                        let name = parsed.name().to_string();
                        if name == "." || name == ".." {
                            continue;
                        }
                        if !show_hidden && name.starts_with('.') {
                            continue;
                        }
                        let is_dir = parsed.is_directory();
                        let is_symlink = parsed.is_symlink();
                        let size = parsed.size() as u64;
                        let modified = Some(DateTime::<Utc>::from(parsed.modified()));
                        let permissions = format_ftp_perms(&parsed);
                        entries.push(FileEntry {
                            name: name.clone(),
                            path: remote_join(path, &name),
                            is_dir,
                            is_symlink,
                            is_parent: false,
                            size,
                            modified,
                            permissions,
                        });
                    }
                }
                sort_entries(&mut entries);
                Ok(entries)
            }
        }
    }

    fn upload(
        &mut self,
        local_path: &Path,
        remote_path: &str,
        transfer_id: u64,
        tx: &Sender<WorkerEvent>,
    ) -> Result<()> {
        let total = std::fs::metadata(local_path)?.len();
        let mut local_file = File::open(local_path)
            .with_context(|| format!("Unable to open {}", local_path.display()))?;

        match self {
            ActiveClient::Sftp { sftp, .. } => {
                let mut remote_file = sftp
                    .create(Path::new(remote_path))
                    .with_context(|| format!("Unable to create {remote_path}"))?;
                copy_with_progress(&mut local_file, &mut remote_file, total, transfer_id, tx)?;
                remote_file.flush()?;
                Ok(())
            }
            ActiveClient::Ftp(ftp) => {
                ftp.transfer_type(FileType::Binary)?;
                let mut stream = ftp
                    .put_with_stream(remote_path)
                    .with_context(|| format!("STOR failed for {remote_path}"))?;
                copy_with_progress(&mut local_file, &mut stream, total, transfer_id, tx)?;
                stream.flush()?;
                stream.finish()?;
                Ok(())
            }
        }
    }

    fn download(
        &mut self,
        remote_path: &str,
        local_path: &Path,
        total_hint: u64,
        transfer_id: u64,
        tx: &Sender<WorkerEvent>,
    ) -> Result<()> {
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut local_file = File::create(local_path)
            .with_context(|| format!("Unable to create {}", local_path.display()))?;

        match self {
            ActiveClient::Sftp { sftp, .. } => {
                let total = sftp
                    .stat(Path::new(remote_path))
                    .ok()
                    .and_then(|s| s.size)
                    .unwrap_or(total_hint);
                let mut remote_file = sftp
                    .open(Path::new(remote_path))
                    .with_context(|| format!("Unable to open {remote_path}"))?;
                copy_with_progress(&mut remote_file, &mut local_file, total, transfer_id, tx)?;
                local_file.flush()?;
                Ok(())
            }
            ActiveClient::Ftp(ftp) => {
                ftp.transfer_type(FileType::Binary)?;
                let total = ftp
                    .size(remote_path)
                    .ok()
                    .map(|s| s as u64)
                    .unwrap_or(total_hint);
                let mut stream = ftp
                    .retr_as_stream(remote_path)
                    .with_context(|| format!("RETR failed for {remote_path}"))?;
                copy_with_progress(&mut stream, &mut local_file, total, transfer_id, tx)?;
                local_file.flush()?;
                stream.finish()?;
                Ok(())
            }
        }
    }

    fn mkdir(&mut self, remote_path: &str) -> Result<()> {
        match self {
            ActiveClient::Sftp { sftp, .. } => {
                sftp.mkdir(Path::new(remote_path), 0o755)?;
                Ok(())
            }
            ActiveClient::Ftp(ftp) => {
                ftp.mkdir(remote_path)?;
                Ok(())
            }
        }
    }

    fn delete(&mut self, remote_path: &str, is_dir: bool) -> Result<()> {
        match self {
            ActiveClient::Sftp { sftp, .. } => {
                if is_dir {
                    sftp.rmdir(Path::new(remote_path))?;
                } else {
                    sftp.unlink(Path::new(remote_path))?;
                }
                Ok(())
            }
            ActiveClient::Ftp(ftp) => {
                if is_dir {
                    ftp.rmdir(remote_path)?;
                } else {
                    ftp.rm(remote_path)?;
                }
                Ok(())
            }
        }
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        match self {
            ActiveClient::Sftp { sftp, .. } => {
                sftp.rename(Path::new(from), Path::new(to), None)?;
                Ok(())
            }
            ActiveClient::Ftp(ftp) => {
                ftp.rename(from, to)?;
                Ok(())
            }
        }
    }

    fn read_preview(&mut self, remote_path: &str, max_bytes: usize) -> Result<(Vec<u8>, Option<u64>, bool)> {
        match self {
            ActiveClient::Sftp { sftp, .. } => {
                let total_size = sftp.stat(Path::new(remote_path)).ok().and_then(|s| s.size);
                let mut remote_file = sftp
                    .open(Path::new(remote_path))
                    .with_context(|| format!("Unable to open {remote_path}"))?;
                let (bytes, truncated) = read_up_to(&mut remote_file, max_bytes)?;
                Ok((bytes, total_size, truncated))
            }
            ActiveClient::Ftp(ftp) => {
                ftp.transfer_type(FileType::Binary)?;
                let total_size = ftp.size(remote_path).ok().map(|s| s as u64);
                let mut stream = ftp
                    .retr_as_stream(remote_path)
                    .with_context(|| format!("RETR failed for {remote_path}"))?;
                let (bytes, truncated) = read_up_to(&mut stream, max_bytes)?;
                if truncated {
                    let _ = ftp.abort(stream);
                } else {
                    let _ = stream.finish();
                }
                Ok((bytes, total_size, truncated))
            }
        }
    }

    fn disconnect(self) {
        if let ActiveClient::Ftp(mut ftp) = self {
            let _ = ftp.quit();
        }
    }
}

fn read_up_to<R: Read>(reader: &mut R, max_bytes: usize) -> Result<(Vec<u8>, bool)> {
    let mut out = Vec::with_capacity(max_bytes.min(64 * 1024));
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    while out.len() <= max_bytes {
        let want = (max_bytes + 1 - out.len()).min(buf.len());
        let n = reader.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    if out.len() > max_bytes {
        out.truncate(max_bytes);
        truncated = true;
    }
    Ok((out, truncated))
}

fn format_ftp_perms(f: &FtpListFile) -> String {
    let prefix = if f.is_directory() {
        'd'
    } else if f.is_symlink() {
        'l'
    } else {
        '-'
    };
    let mut s = String::with_capacity(10);
    s.push(prefix);
    for who in [
        suppaftp::list::PosixPexQuery::Owner,
        suppaftp::list::PosixPexQuery::Group,
        suppaftp::list::PosixPexQuery::Others,
    ] {
        s.push(if f.can_read(who) { 'r' } else { '-' });
        s.push(if f.can_write(who) { 'w' } else { '-' });
        s.push(if f.can_execute(who) { 'x' } else { '-' });
    }
    s
}

fn copy_with_progress<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    total: u64,
    transfer_id: u64,
    tx: &Sender<WorkerEvent>,
) -> Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut done: u64 = 0;
    let mut last_report: u64 = 0;
    let _ = tx.send(WorkerEvent::TransferProgress {
        transfer_id,
        done: 0,
        total,
    });
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        done += n as u64;
        if done.saturating_sub(last_report) >= 64 * 1024 || (total > 0 && done >= total) {
            last_report = done;
            let _ = tx.send(WorkerEvent::TransferProgress {
                transfer_id,
                done,
                total,
            });
        }
    }
    let _ = tx.send(WorkerEvent::TransferProgress {
        transfer_id,
        done,
        total: total.max(done),
    });
    Ok(())
}

fn connect_client(
    profile: &ConnectionProfile,
    runtime_secret: Option<&str>,
    tx: &Sender<WorkerEvent>,
) -> Result<ActiveClient> {
    let secret = runtime_secret
        .filter(|s| !s.is_empty())
        .unwrap_or(&profile.secret);

    match profile.protocol {
        Protocol::Sftp => {
            let addr_str = format!("{}:{}", profile.host, profile.port);
            let _ = tx.send(WorkerEvent::Log {
                level: LogLevel::Info,
                message: format!("Connecting to SFTP at {addr_str}..."),
            });
            let sock_addr = addr_str
                .to_socket_addrs()
                .with_context(|| format!("DNS resolution failed for {addr_str}"))?
                .next()
                .ok_or_else(|| anyhow!("No address found for {addr_str}"))?;
            let tcp = TcpStream::connect_timeout(&sock_addr, Duration::from_secs(15))
                .with_context(|| format!("TCP connection to {addr_str} failed"))?;
            tcp.set_read_timeout(Some(Duration::from_secs(30))).ok();
            tcp.set_write_timeout(Some(Duration::from_secs(30))).ok();

            let mut sess = Session::new().context("Failed to create SSH session")?;
            sess.set_tcp_stream(tcp);
            sess.handshake().context("SSH handshake failed")?;

            match profile.auth_kind {
                AuthKind::Password => {
                    sess.userauth_password(&profile.username, secret)
                        .context("SFTP password authentication failed")?;
                }
                AuthKind::KeyFile => {
                    let key_path = expand_tilde(&profile.key_path);
                    if !key_path.exists() {
                        return Err(anyhow!(
                            "Private key file not found: {}",
                            key_path.display()
                        ));
                    }
                    let pub_path_candidate = {
                        let mut os = key_path.as_os_str().to_os_string();
                        os.push(".pub");
                        let p = std::path::PathBuf::from(os);
                        if p.exists() { Some(p) } else { None }
                    };
                    let passphrase = if secret.is_empty() {
                        None
                    } else {
                        Some(secret)
                    };
                    sess.userauth_pubkey_file(
                        &profile.username,
                        pub_path_candidate.as_deref(),
                        &key_path,
                        passphrase,
                    )
                    .with_context(|| {
                        format!(
                            "SSH key authentication failed ({})",
                            key_path.display()
                        )
                    })?;
                }
                AuthKind::SshAgent => {
                    sess.userauth_agent(&profile.username)
                        .context("SSH Agent authentication failed")?;
                }
            }

            if !sess.authenticated() {
                return Err(anyhow!("SSH session is not authenticated"));
            }

            let sftp = sess.sftp().context("Failed to open SFTP subsystem")?;
            Ok(ActiveClient::Sftp {
                _session: sess,
                sftp,
            })
        }
        Protocol::Ftp => {
            let addr_str = format!("{}:{}", profile.host, profile.port);
            let _ = tx.send(WorkerEvent::Log {
                level: LogLevel::Info,
                message: format!("Connecting to FTP at {addr_str}..."),
            });
            let mut ftp = NativeTlsFtpStream::connect(&addr_str)
                .with_context(|| format!("FTP connection to {addr_str} failed"))?;
            let user = if profile.username.is_empty() {
                "anonymous"
            } else {
                &profile.username
            };
            let pass = if profile.username.is_empty() && secret.is_empty() {
                "anonymous@"
            } else {
                secret
            };
            ftp.login(user, pass).context("FTP login failed")?;
            Ok(ActiveClient::Ftp(ftp))
        }
        Protocol::Ftps => {
            let addr_str = format!("{}:{}", profile.host, profile.port);
            let _ = tx.send(WorkerEvent::Log {
                level: LogLevel::Info,
                message: format!("Connecting to FTPS (Explicit TLS) at {addr_str}..."),
            });
            let ftp = NativeTlsFtpStream::connect(&addr_str)
                .with_context(|| format!("FTPS connection to {addr_str} failed"))?;
            let tls_connector = TlsConnector::new().context("TLS initialization failed")?;
            let ctx = NativeTlsConnector::from(tls_connector);
            let mut ftp = ftp
                .into_secure(ctx, &profile.host)
                .context("FTPS TLS handshake failed")?;
            let user = if profile.username.is_empty() {
                "anonymous"
            } else {
                &profile.username
            };
            ftp.login(user, secret).context("FTPS login failed")?;
            Ok(ActiveClient::Ftp(ftp))
        }
    }
}

pub struct RemoteWorkerHandle {
    pub tx: Sender<WorkerCommand>,
    pub rx: Receiver<WorkerEvent>,
}

pub fn spawn_worker() -> RemoteWorkerHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCommand>();
    let (ev_tx, ev_rx) = mpsc::channel::<WorkerEvent>();

    std::thread::spawn(move || {
        let mut client: Option<ActiveClient> = None;

        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                WorkerCommand::Shutdown => {
                    if let Some(c) = client.take() {
                        c.disconnect();
                    }
                    break;
                }
                WorkerCommand::Disconnect => {
                    if let Some(c) = client.take() {
                        c.disconnect();
                    }
                    let _ = ev_tx.send(WorkerEvent::Disconnected);
                }
                WorkerCommand::Connect {
                    profile,
                    runtime_secret,
                } => {
                    if let Some(c) = client.take() {
                        c.disconnect();
                    }
                    match connect_client(&profile, runtime_secret.as_deref(), &ev_tx) {
                        Ok(mut new_client) => {
                            let start_dir = if !profile.remote_dir.trim().is_empty() {
                                profile.remote_dir.trim().to_string()
                            } else {
                                new_client.pwd().unwrap_or_else(|_| "/".to_string())
                            };
                            match new_client.list_dir(&start_dir, false) {
                                Ok(entries) => {
                                    let display_name = if profile.name.trim().is_empty() {
                                        format!("{}@{}", profile.username, profile.host)
                                    } else {
                                        profile.name.clone()
                                    };
                                    client = Some(new_client);
                                    let _ = ev_tx.send(WorkerEvent::Connected {
                                        profile_name: display_name,
                                        remote_pwd: start_dir,
                                        entries,
                                    });
                                }
                                Err(err) => {
                                    new_client.disconnect();
                                    let _ = ev_tx.send(WorkerEvent::Error(format!(
                                        "Connected, but failed to read initial directory: {err:#}"
                                    )));
                                }
                            }
                        }
                        Err(err) => {
                            let _ = ev_tx.send(WorkerEvent::Error(format!("{err:#}")));
                        }
                    }
                }
                WorkerCommand::ListDir { path, show_hidden } => {
                    let Some(c) = client.as_mut() else {
                        let _ = ev_tx.send(WorkerEvent::Error("Not connected".into()));
                        continue;
                    };
                    match c.list_dir(&path, show_hidden) {
                        Ok(entries) => {
                            let _ = ev_tx.send(WorkerEvent::DirListed { path, entries });
                        }
                        Err(err) => {
                            let _ = ev_tx.send(WorkerEvent::Error(format!("{err:#}")));
                        }
                    }
                }
                WorkerCommand::Upload {
                    local_path,
                    remote_path,
                    transfer_id,
                } => {
                    let Some(c) = client.as_mut() else {
                        let _ = ev_tx.send(WorkerEvent::TransferFinished {
                            transfer_id,
                            ok: false,
                            message: "Not connected".into(),
                        });
                        continue;
                    };
                    match c.upload(&local_path, &remote_path, transfer_id, &ev_tx) {
                        Ok(()) => {
                            let _ = ev_tx.send(WorkerEvent::TransferFinished {
                                transfer_id,
                                ok: true,
                                message: format!("Upload complete: {remote_path}"),
                            });
                        }
                        Err(err) => {
                            let _ = ev_tx.send(WorkerEvent::TransferFinished {
                                transfer_id,
                                ok: false,
                                message: format!("Upload failed: {err:#}"),
                            });
                        }
                    }
                }
                WorkerCommand::Download {
                    remote_path,
                    local_path,
                    transfer_id,
                    total_size,
                } => {
                    let Some(c) = client.as_mut() else {
                        let _ = ev_tx.send(WorkerEvent::TransferFinished {
                            transfer_id,
                            ok: false,
                            message: "Not connected".into(),
                        });
                        continue;
                    };
                    match c.download(&remote_path, &local_path, total_size, transfer_id, &ev_tx) {
                        Ok(()) => {
                            let _ = ev_tx.send(WorkerEvent::TransferFinished {
                                transfer_id,
                                ok: true,
                                message: format!(
                                    "Download complete: {}",
                                    local_path.display()
                                ),
                            });
                        }
                        Err(err) => {
                            let _ = ev_tx.send(WorkerEvent::TransferFinished {
                                transfer_id,
                                ok: false,
                                message: format!("Download failed: {err:#}"),
                            });
                        }
                    }
                }
                WorkerCommand::Mkdir { remote_path } => {
                    let Some(c) = client.as_mut() else {
                        let _ = ev_tx.send(WorkerEvent::Error("Not connected".into()));
                        continue;
                    };
                    match c.mkdir(&remote_path) {
                        Ok(()) => {
                            let _ = ev_tx.send(WorkerEvent::OperationDone {
                                message: format!("Remote directory created: {remote_path}"),
                                refresh_remote: true,
                            });
                        }
                        Err(err) => {
                            let _ = ev_tx.send(WorkerEvent::Error(format!(
                                "Error creating directory: {err:#}"
                            )));
                        }
                    }
                }
                WorkerCommand::Delete {
                    remote_path,
                    is_dir,
                } => {
                    let Some(c) = client.as_mut() else {
                        let _ = ev_tx.send(WorkerEvent::Error("Not connected".into()));
                        continue;
                    };
                    match c.delete(&remote_path, is_dir) {
                        Ok(()) => {
                            let _ = ev_tx.send(WorkerEvent::OperationDone {
                                message: format!("Deleted from server: {remote_path}"),
                                refresh_remote: true,
                            });
                        }
                        Err(err) => {
                            let _ = ev_tx.send(WorkerEvent::Error(format!(
                                "Error deleting remote item: {err:#}"
                            )));
                        }
                    }
                }
                WorkerCommand::Rename { from, to } => {
                    let Some(c) = client.as_mut() else {
                        let _ = ev_tx.send(WorkerEvent::Error("Not connected".into()));
                        continue;
                    };
                    match c.rename(&from, &to) {
                        Ok(()) => {
                            let _ = ev_tx.send(WorkerEvent::OperationDone {
                                message: format!("Renamed: {from} -> {to}"),
                                refresh_remote: true,
                            });
                        }
                        Err(err) => {
                            let _ = ev_tx.send(WorkerEvent::Error(format!(
                                "Error renaming remote item: {err:#}"
                            )));
                        }
                    }
                }
                WorkerCommand::Preview {
                    remote_path,
                    max_bytes,
                } => {
                    let Some(c) = client.as_mut() else {
                        let _ = ev_tx.send(WorkerEvent::Error("Not connected".into()));
                        continue;
                    };
                    match c.read_preview(&remote_path, max_bytes) {
                        Ok((bytes, total_size, truncated)) => {
                            let _ = ev_tx.send(WorkerEvent::PreviewLoaded {
                                title: remote_path,
                                bytes,
                                total_size,
                                truncated,
                            });
                        }
                        Err(err) => {
                            let _ = ev_tx.send(WorkerEvent::Error(format!(
                                "Remote preview error: {err:#}"
                            )));
                        }
                    }
                }
            }
        }
    });

    RemoteWorkerHandle {
        tx: cmd_tx,
        rx: ev_rx,
    }
}
