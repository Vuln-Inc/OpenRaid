//! Native repository tools. Work queues and returned previews are bounded, but
//! operation duration is never bounded. Command output is drained to disk in full.
use anyhow::{anyhow, bail, Context, Result};
use globset::Glob;
use ignore::WalkBuilder;
use regex::Regex;
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Semaphore,
};

const PREVIEW_BYTES: usize = 64 * 1024;
const TEXT_BYTES: usize = 4 * 1024 * 1024;
const PATCH_BYTES: usize = 4 * 1024 * 1024;
static OUTPUT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct WorkspaceTools {
    root: Arc<PathBuf>,
    io: Arc<Semaphore>,
    pty: crate::pty::Hub,
    cancelled: Option<Arc<AtomicBool>>,
}

struct OperationCancellation {
    cancelled: Arc<AtomicBool>,
    completed: bool,
}

impl Drop for OperationCancellation {
    fn drop(&mut self) {
        if !self.completed {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}

impl WorkspaceTools {
    pub fn new(root: impl AsRef<Path>, max_processes: usize) -> Result<Self> {
        if max_processes == 0 {
            bail!("max_processes must be positive");
        }
        let root = fs::canonicalize(root).context("resolve workspace")?;
        if !root.is_dir() {
            bail!("workspace must be a directory");
        }
        let processes = Arc::new(Semaphore::new(max_processes));
        Ok(Self {
            root: Arc::new(root),
            pty: crate::pty::Hub::new(processes),
            io: Arc::new(Semaphore::new(max_processes.clamp(1, 16))),
            cancelled: None,
        })
    }

    pub async fn execute(&self, name: &str, args: &Value) -> Result<Value> {
        self.execute_observed(name, args, None).await
    }

    pub async fn execute_with_activity(
        &self,
        agent_id: &str,
        name: &str,
        args: &Value,
        metrics: Arc<crate::metrics::Metrics>,
    ) -> Result<Value> {
        self.execute_observed(
            name,
            args,
            Some(crate::pty::ProcessActivity {
                metrics,
                agent_id: agent_id.to_owned(),
            }),
        )
        .await
    }

    async fn execute_observed(
        &self,
        name: &str,
        args: &Value,
        activity: Option<crate::pty::ProcessActivity>,
    ) -> Result<Value> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut operation = OperationCancellation {
            cancelled: cancelled.clone(),
            completed: false,
        };
        let mut this = self.clone();
        this.cancelled = Some(cancelled);
        let result = this.execute_operation(name, args, activity).await;
        operation.completed = true;
        result
    }

    async fn execute_operation(
        &self,
        name: &str,
        args: &Value,
        activity: Option<crate::pty::ProcessActivity>,
    ) -> Result<Value> {
        let generation = self.pty.generation();
        if name.starts_with("pty_") {
            let this = self.clone();
            let name = name.to_owned();
            let args = args.clone();
            return tokio::task::spawn_blocking(move || {
                let workdir =
                    this.resolve(args.get("workdir").and_then(Value::as_str).unwrap_or("."))?;
                let output_dir = this.resolve(".openraid/tool-output")?;
                this.pty.execute_with_activity(
                    &name,
                    &args,
                    &workdir,
                    &output_dir,
                    crate::pty::ExecutionContext {
                        generation,
                        activity,
                        cancelled: this.cancelled.clone(),
                    },
                )
            })
            .await?;
        }
        if matches!(name, "run_command" | "exec") {
            return self.run_command(args, activity, generation).await;
        }
        let permit = self.io.clone().acquire_owned().await?;
        let this = self.clone();
        let name = name.to_owned();
        let args = args.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            this.check_generation(generation)?;
            match name.as_str() {
                "read_file" => this.read_file(&args, generation),
                "list_files" => this.list_files(&args, generation),
                "search_files" => this.search_files(&args, generation),
                "apply_patch" => this.apply_patch(&args, generation),
                _ => bail!("unknown workspace tool: {name}"),
            }
        })
        .await?
    }

    /// Owner stop terminates persistent terminals and invalidates admitted
    /// blocking I/O, patch operations, and pending process spawns.
    pub fn stop_processes(&self) {
        self.pty.stop_all();
    }

    fn check_generation(&self, generation: u64) -> Result<()> {
        if generation != self.pty.generation()
            || self
                .cancelled
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            bail!("workspace operation cancelled by owner stop");
        }
        Ok(())
    }

    fn read_bytes(&self, reader: impl Read, limit: usize, generation: u64) -> Result<Vec<u8>> {
        let mut reader = reader.take(limit as u64);
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            self.check_generation(generation)?;
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        self.check_generation(generation)?;
        Ok(bytes)
    }

    pub fn schemas() -> Vec<Value> {
        let mut schemas = vec![
            tool("read_file", "Read UTF-8 text by one-based line offset or byte offset. Results are bounded; continue at next_offset or next_byte_offset. Also reads persisted command output.", json!({
                "path":{"type":"string"}, "offset":{"type":"integer","minimum":1},
                "limit":{"type":"integer","minimum":1,"maximum":2000},
                "byte_offset":{"type":"integer","minimum":0}
            }), &["path"]),
            tool("list_files", "List repository files in deterministic traversal order, honoring ignore files. Pagination is by numeric offset, never topics. include_ignored includes hidden/ignored files except .git, target, and internal command logs.", json!({
                "path":{"type":"string"},"pattern":{"type":"string"},
                "offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":1000},
                "include_ignored":{"type":"boolean"}
            }), &[]),
            tool("search_files", "Search repository text with a regex, optionally restricted by a file glob. Returns path and line with paginated matches. Files above 4 MiB and binary files are reported as skipped; use read_file to inspect them.", json!({
                "query":{"type":"string"},"path":{"type":"string"},"pattern":{"type":"string"},
                "offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":1000},
                "include_ignored":{"type":"boolean"}
            }), &["query"]),
            tool("apply_patch", "Apply an exact-context patch: *** Begin Patch, *** Add File/Update File/Delete File, optional *** Move to, @@ hunks with space context/+ additions/- deletions, *** End Patch. All changes preflight before writes; re-read and coordinate on the global board before edits.", json!({"patch":{"type":"string"}}), &["patch"]),
            tool("run_command", "Execute an executable with args, or a shell command, in the workspace. No duration timeout. stdout/stderr are fully persisted with bounded previews; read the returned log paths for the remainder. Shell defaults to PowerShell on Windows and sh elsewhere.", json!({
                "program":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},
                "command":{"type":"string"},"shell":{"type":"string","enum":["default","bash","powershell","cmd","sh"]},
                "workdir":{"type":"string"}
            }), &[]),
        ];
        schemas.extend(crate::pty::Hub::schemas());
        schemas
    }

    fn resolve(&self, input: &str) -> Result<PathBuf> {
        let path = Path::new(input);
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        let mut normalized = PathBuf::new();
        for part in joined.components() {
            match part {
                Component::CurDir => (),
                Component::ParentDir => {
                    if !normalized.pop() {
                        bail!("invalid path");
                    }
                }
                other => normalized.push(other.as_os_str()),
            }
        }
        // Check the nearest existing ancestor so symlinks cannot bypass the root.
        let mut ancestor = normalized.as_path();
        while fs::symlink_metadata(ancestor).is_err() {
            ancestor = ancestor.parent().ok_or_else(|| anyhow!("invalid path"))?;
        }
        let canonical = fs::canonicalize(ancestor)?;
        if !canonical.starts_with(self.root.as_ref()) {
            bail!("symlink escapes workspace");
        }
        // Canonicalize the ancestor before adding the new suffix. On Windows,
        // canonical paths have a verbatim prefix unlike user-supplied C:\ paths.
        let suffix = normalized.strip_prefix(ancestor)?;
        Ok(if suffix.as_os_str().is_empty() {
            canonical
        } else {
            canonical.join(suffix)
        })
    }

    fn read_file(&self, args: &Value, generation: u64) -> Result<Value> {
        self.check_generation(generation)?;
        let path = self.resolve(required_str(args, "path")?)?;
        if path.is_dir() {
            bail!("path is a directory; use list_files");
        }
        if let Some(start) = args.get("byte_offset").and_then(Value::as_u64) {
            use std::io::{Seek, SeekFrom};
            let mut file = fs::File::open(&path)?;
            let size = file.metadata()?.len();
            file.seek(SeekFrom::Start(start))?;
            let bytes = self.read_bytes(file, PREVIEW_BYTES, generation)?;
            let next = start.saturating_add(bytes.len() as u64);
            return Ok(
                json!({"path":relative(&self.root,&path), "content":String::from_utf8_lossy(&bytes),
                "byte_offset":start,"next_byte_offset":next,"has_more":next<size,"size_bytes":size}),
            );
        }
        let offset = number(args, "offset", 1).max(1);
        let limit = number(args, "limit", 200).clamp(1, 2000);
        let mut reader = BufReader::new(fs::File::open(&path)?);
        let mut index = 1usize;
        let mut byte_offset = 0u64;
        let mut content = String::new();
        let mut count = 0usize;
        let mut has_more = false;
        // fill_buf/consume bounds memory even for a multi-gigabyte single line.
        loop {
            let (line, bytes, long, eof) = bounded_line(&mut reader, PREVIEW_BYTES, || {
                self.check_generation(generation)
            })?;
            if eof && bytes == 0 {
                break;
            }
            if index >= offset {
                if count == limit || (count > 0 && content.len() + line.len() > PREVIEW_BYTES) {
                    has_more = true;
                    break;
                }
                content.push_str(&format!("{index}: {line}"));
                if !line.ends_with('\n') {
                    content.push('\n');
                }
                count += 1;
                if long {
                    return Ok(json!({"path":relative(&self.root,&path),"content":content,
                        "offset":offset,"next_offset":index,"has_more":true,"long_line":true,
                        "next_byte_offset":byte_offset + bytes.min(PREVIEW_BYTES as u64),
                        "notice":"line preview bounded; continue with byte_offset"}));
                }
            }
            byte_offset += bytes;
            index += 1;
            if eof {
                break;
            }
        }
        self.check_generation(generation)?;
        Ok(
            json!({"path":relative(&self.root,&path),"content":content,"offset":offset,
            "next_offset":offset+count,"has_more":has_more,"next_byte_offset":byte_offset}),
        )
    }

    fn walker(&self, args: &Value, generation: u64) -> Result<ignore::Walk> {
        let path = self.resolve(args.get("path").and_then(Value::as_str).unwrap_or("."))?;
        let include = args
            .get("include_ignored")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut builder = WalkBuilder::new(path);
        builder
            .standard_filters(!include)
            .follow_links(false)
            .sort_by_file_path(|a, b| a.cmp(b));
        let pty = self.pty.clone();
        let cancelled = self.cancelled.clone();
        builder.filter_entry(move |entry| {
            generation == pty.generation()
                && !cancelled
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::Acquire))
                && !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | "target" | "tool-output")
                )
        });
        Ok(builder.build())
    }

    fn list_files(&self, args: &Value, generation: u64) -> Result<Value> {
        self.check_generation(generation)?;
        let matcher = Glob::new(
            args.get("pattern")
                .and_then(Value::as_str)
                .unwrap_or("**/*"),
        )?
        .compile_matcher();
        let offset = number(args, "offset", 0);
        let limit = number(args, "limit", 200).clamp(1, 1000);
        let mut seen = 0usize;
        let mut paths = Vec::new();
        let mut has_more = false;
        for entry in self.walker(args, generation)? {
            self.check_generation(generation)?;
            let entry = entry?;
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = relative(&self.root, entry.path());
            if !matcher.is_match(&path) {
                continue;
            }
            seen += 1;
            if seen <= offset {
                continue;
            }
            if paths.len() == limit {
                has_more = true;
                break;
            }
            paths.push(path);
        }
        self.check_generation(generation)?;
        Ok(
            json!({"files":paths,"offset":offset,"next_offset":offset+paths.len(),"has_more":has_more}),
        )
    }

    fn search_files(&self, args: &Value, generation: u64) -> Result<Value> {
        self.check_generation(generation)?;
        let regex = Regex::new(required_str(args, "query")?)?;
        let matcher = Glob::new(
            args.get("pattern")
                .and_then(Value::as_str)
                .unwrap_or("**/*"),
        )?
        .compile_matcher();
        let offset = number(args, "offset", 0);
        let limit = number(args, "limit", 100).clamp(1, 1000);
        let mut seen = 0usize;
        let mut matches = Vec::new();
        let mut skipped = 0usize;
        let mut output_bytes = 0usize;
        let mut has_more = false;
        'files: for entry in self.walker(args, generation)? {
            self.check_generation(generation)?;
            let entry = entry?;
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = relative(&self.root, entry.path());
            if !matcher.is_match(&path) {
                continue;
            }
            let file = fs::File::open(entry.path())?;
            if file.metadata()?.len() > TEXT_BYTES as u64 {
                skipped += 1;
                continue;
            }
            let bytes = self.read_bytes(file, TEXT_BYTES + 1, generation)?;
            if bytes.len() > TEXT_BYTES || bytes.contains(&0) {
                skipped += 1;
                continue;
            }
            let Ok(text) = std::str::from_utf8(&bytes) else {
                skipped += 1;
                continue;
            };
            for (line, body) in text.lines().enumerate() {
                self.check_generation(generation)?;
                if !regex.is_match(body) {
                    continue;
                }
                seen += 1;
                if seen <= offset {
                    continue;
                }
                if matches.len() == limit || output_bytes >= PREVIEW_BYTES {
                    has_more = true;
                    break 'files;
                }
                let mut end = body.len().min(2000);
                while !body.is_char_boundary(end) {
                    end -= 1;
                }
                output_bytes += end + path.len();
                matches.push(json!({"path":path,"line":line+1,"text":&body[..end],"truncated":end<body.len()}));
            }
        }
        self.check_generation(generation)?;
        Ok(
            json!({"matches":matches,"offset":offset,"next_offset":offset+matches.len(),
            "has_more":has_more,"skipped_binary_or_large_files":skipped}),
        )
    }

    fn apply_patch(&self, args: &Value, generation: u64) -> Result<Value> {
        self.check_generation(generation)?;
        let patch = required_str(args, "patch")?;
        if patch.len() > PATCH_BYTES {
            bail!("patch exceeds 4 MiB");
        }
        let operations = parse_patch(patch)?;
        let mut prepared = Vec::new();
        for operation in operations {
            self.check_generation(generation)?;
            let path = self.resolve(&operation.path)?;
            if prepared
                .iter()
                .any(|p: &Prepared| p.path == path || p.destination.as_ref() == Some(&path))
            {
                bail!("duplicate patch path: {}", operation.path);
            }
            let destination = operation
                .destination
                .as_deref()
                .map(|p| self.resolve(p))
                .transpose()?;
            if destination.as_ref().is_some_and(|destination| {
                prepared.iter().any(|edit| {
                    &edit.path == destination || edit.destination.as_ref() == Some(destination)
                })
            }) {
                bail!("duplicate patch destination");
            }
            let original = match operation.kind {
                Kind::Add => {
                    if path.exists() {
                        bail!("add target already exists: {}", operation.path);
                    }
                    None
                }
                _ => {
                    let metadata = fs::metadata(&path)?;
                    if metadata.len() > PATCH_BYTES as u64 {
                        bail!("edit target exceeds 4 MiB");
                    }
                    let bytes =
                        self.read_bytes(fs::File::open(&path)?, PATCH_BYTES + 1, generation)?;
                    if bytes.len() > PATCH_BYTES {
                        bail!("edit target exceeds 4 MiB");
                    }
                    Some(bytes)
                }
            };
            if destination
                .as_ref()
                .is_some_and(|p| p != &path && p.exists())
            {
                bail!("move destination already exists");
            }
            let replacement = match operation.kind {
                Kind::Add => Some(
                    operation.added.join("\n") + if operation.added.is_empty() { "" } else { "\n" },
                ),
                Kind::Delete => None,
                Kind::Update => Some(apply_hunks(
                    std::str::from_utf8(original.as_ref().unwrap())?,
                    &operation.hunks,
                )?),
            };
            prepared.push(Prepared {
                path,
                destination,
                original,
                replacement,
            });
        }
        // Optimistic conflict detection, no locks or file-ownership system.
        for edit in &prepared {
            self.check_generation(generation)?;
            match &edit.original {
                Some(bytes)
                    if self.read_bytes(
                        fs::File::open(&edit.path)?,
                        PATCH_BYTES + 1,
                        generation,
                    )? != *bytes =>
                {
                    bail!("file changed during patch preflight; re-read and coordinate")
                }
                None if edit.path.exists() => bail!("add target appeared during patch preflight"),
                _ => (),
            }
        }
        let mut changed = Vec::new();
        for edit in prepared {
            self.check_generation(generation)?;
            let destination = edit.destination.as_ref().unwrap_or(&edit.path);
            if let Some(text) = edit.replacement {
                if let Some(parent) = destination.parent() {
                    self.check_generation(generation)?;
                    fs::create_dir_all(parent)?;
                }
                self.check_generation(generation)?;
                fs::write(destination, text)?;
                if destination != &edit.path {
                    self.check_generation(generation)?;
                    fs::remove_file(&edit.path)?;
                }
            } else {
                self.check_generation(generation)?;
                fs::remove_file(&edit.path)?;
            }
            changed.push(relative(&self.root, destination));
        }
        Ok(json!({"changed":changed,"count":changed.len()}))
    }

    async fn run_command(
        &self,
        args: &Value,
        activity: Option<crate::pty::ProcessActivity>,
        generation: u64,
    ) -> Result<Value> {
        let _permit = self.pty.command_permit().await?;
        self.check_generation(generation)?;
        let workdir = self.resolve(args.get("workdir").and_then(Value::as_str).unwrap_or("."))?;
        if !workdir.is_dir() {
            bail!("workdir must be a directory");
        }
        let mut command = if let Some(program) = args.get("program").and_then(Value::as_str) {
            let mut command = Command::new(program);
            if let Some(arguments) = args.get("args") {
                let arguments = arguments
                    .as_array()
                    .ok_or_else(|| anyhow!("args must be an array"))?;
                for arg in arguments {
                    command.arg(
                        arg.as_str()
                            .ok_or_else(|| anyhow!("each argument must be a string"))?,
                    );
                }
            }
            command
        } else {
            shell_command(
                required_str(args, "command")?,
                args.get("shell")
                    .and_then(Value::as_str)
                    .unwrap_or("default"),
            )?
        };
        let output_directory = self.resolve(".openraid/tool-output")?;
        tokio::fs::create_dir_all(&output_directory).await?;
        let id = format!(
            "{}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
            OUTPUT_ID.fetch_add(1, Ordering::Relaxed)
        );
        let stdout_path = output_directory.join(format!("{id}.stdout.log"));
        let stderr_path = output_directory.join(format!("{id}.stderr.log"));
        let stdout_file = tokio::fs::File::create(&stdout_path).await?;
        let stderr_file = tokio::fs::File::create(&stderr_path).await?;
        command
            .current_dir(workdir)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);
        self.check_generation(generation)?;
        let mut child = command.spawn().context("spawn command")?;
        let mut process = CommandProcess::new(child.id().context("command PID missing")?);
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("stdout missing"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("stderr missing"))?;
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            capture(stdout, stdout_file, activity.clone()),
            capture(stderr, stderr_file, activity)
        );
        let status = status?;
        let (stdout, stdout_bytes) = stdout?;
        let (stderr, stderr_bytes) = stderr?;
        process.finished = true;
        Ok(json!({"exit_code":status.code(),"success":status.success(),
            "stdout":stdout,"stderr":stderr,"stdout_bytes":stdout_bytes,"stderr_bytes":stderr_bytes,
            "stdout_path":relative(&self.root,&stdout_path),"stderr_path":relative(&self.root,&stderr_path),
            "truncated":stdout_bytes>PREVIEW_BYTES as u64 || stderr_bytes>PREVIEW_BYTES as u64}))
    }
}

/// Cancelling a tool future must terminate the shell's descendants as well as
/// the direct Tokio child. Keep this armed until both output pipes are drained.
struct CommandProcess {
    pid: u32,
    finished: bool,
}

impl CommandProcess {
    fn new(pid: u32) -> Self {
        Self {
            pid,
            finished: false,
        }
    }
}

impl Drop for CommandProcess {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        #[cfg(unix)]
        {
            use nix::{
                sys::signal::{killpg, Signal},
                unistd::Pid,
            };
            let _ = killpg(Pid::from_raw(self.pid as i32), Signal::SIGKILL);
        }
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill.exe")
                .args(["/F", "/T", "/PID", &self.pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"type":"function","function":{"name":name,"description":description,
        "parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}})
}
fn required_str<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{name} must be a string"))
}
fn number(args: &Value, name: &str, default: usize) -> usize {
    args.get(name)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(default)
}
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn bounded_line(
    reader: &mut impl BufRead,
    cap: usize,
    check: impl Fn() -> Result<()>,
) -> Result<(String, u64, bool, bool)> {
    let mut preview = Vec::new();
    let mut bytes = 0u64;
    let mut eof = false;
    loop {
        check()?;
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            eof = true;
            break;
        }
        let newline = buffer.iter().position(|b| *b == b'\n');
        let consumed = newline.map_or(buffer.len(), |n| n + 1);
        let copied = consumed.min(cap.saturating_sub(preview.len()));
        preview.extend_from_slice(&buffer[..copied]);
        bytes += consumed as u64;
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    let long = bytes > cap as u64;
    Ok((
        String::from_utf8_lossy(&preview).into_owned(),
        bytes,
        long,
        eof,
    ))
}

async fn capture(
    mut reader: impl AsyncRead + Unpin,
    mut file: tokio::fs::File,
    activity: Option<crate::pty::ProcessActivity>,
) -> Result<(String, u64)> {
    let mut preview = Vec::with_capacity(PREVIEW_BYTES);
    let mut buffer = [0u8; 8192];
    let mut total = 0u64;
    let mut disk_error = None;
    let mut activity = activity.map(crate::pty::ProcessActivity::writer);
    loop {
        let length = reader.read(&mut buffer).await?;
        if length == 0 {
            break;
        }
        if let Some(activity) = &mut activity {
            activity.append(&buffer[..length]);
        }
        total += length as u64;
        let count = length.min(PREVIEW_BYTES.saturating_sub(preview.len()));
        preview.extend_from_slice(&buffer[..count]);
        if disk_error.is_none() {
            if let Err(error) = file.write_all(&buffer[..length]).await {
                disk_error = Some(error);
            }
        }
    }
    if let Some(activity) = &mut activity {
        activity.finish();
    }
    // Even a disk error must not stop draining the child and deadlock its pipes.
    if let Some(error) = disk_error {
        return Err(error.into());
    }
    file.flush().await?;
    Ok((String::from_utf8_lossy(&preview).into_owned(), total))
}

fn shell_command(text: &str, shell: &str) -> Result<Command> {
    let shell = if shell == "default" {
        if cfg!(windows) {
            "powershell"
        } else {
            "sh"
        }
    } else {
        shell
    };
    let mut command = match shell {
        "powershell" => {
            let mut c = Command::new("powershell.exe");
            c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]);
            c
        }
        "cmd" => {
            let mut c = Command::new("cmd.exe");
            c.args(["/D", "/S", "/C"]);
            c
        }
        "bash" => {
            let program =
                if cfg!(windows) && Path::new("C:\\Program Files\\Git\\bin\\bash.exe").exists() {
                    "C:\\Program Files\\Git\\bin\\bash.exe"
                } else {
                    "bash"
                };
            let mut c = Command::new(program);
            c.arg("-c");
            c
        }
        "sh" => {
            let mut c = Command::new("sh");
            c.arg("-c");
            c
        }
        _ => bail!("unsupported shell"),
    };
    command.arg(text);
    Ok(command)
}

#[derive(Clone, Copy)]
enum Kind {
    Add,
    Update,
    Delete,
}
struct Operation {
    path: String,
    destination: Option<String>,
    kind: Kind,
    added: Vec<String>,
    hunks: Vec<Hunk>,
}
struct Hunk {
    anchor: Option<String>,
    lines: Vec<String>,
    end: bool,
}
struct Prepared {
    path: PathBuf,
    destination: Option<PathBuf>,
    original: Option<Vec<u8>>,
    replacement: Option<String>,
}

fn parse_patch(text: &str) -> Result<Vec<Operation>> {
    let lines: Vec<_> = text.lines().collect();
    if lines.first() != Some(&"*** Begin Patch") || lines.last() != Some(&"*** End Patch") {
        bail!("patch requires begin/end markers");
    }
    let mut operations = Vec::new();
    let mut i = 1;
    while i + 1 < lines.len() {
        let (kind, path) = if let Some(path) = lines[i].strip_prefix("*** Add File: ") {
            (Kind::Add, path)
        } else if let Some(path) = lines[i].strip_prefix("*** Update File: ") {
            (Kind::Update, path)
        } else if let Some(path) = lines[i].strip_prefix("*** Delete File: ") {
            (Kind::Delete, path)
        } else {
            bail!("expected file operation at line {}", i + 1);
        };
        if path.trim().is_empty() {
            bail!("empty patch path");
        }
        let mut operation = Operation {
            path: path.to_owned(),
            destination: None,
            kind,
            added: vec![],
            hunks: vec![],
        };
        i += 1;
        if matches!(kind, Kind::Update) && i + 1 < lines.len() {
            if let Some(path) = lines[i].strip_prefix("*** Move to: ") {
                operation.destination = Some(path.to_owned());
                i += 1;
            }
        }
        while i + 1 < lines.len()
            && !lines[i].starts_with("*** Add File: ")
            && !lines[i].starts_with("*** Update File: ")
            && !lines[i].starts_with("*** Delete File: ")
        {
            match kind {
                Kind::Add => operation.added.push(
                    lines[i]
                        .strip_prefix('+')
                        .ok_or_else(|| anyhow!("add file lines require +"))?
                        .to_owned(),
                ),
                Kind::Delete => bail!("delete operation cannot have body"),
                Kind::Update => {
                    if lines[i] == "@@" || lines[i].starts_with("@@ ") {
                        let anchor = lines[i]
                            .strip_prefix("@@ ")
                            .filter(|a| !a.starts_with('-'))
                            .map(str::to_owned);
                        operation.hunks.push(Hunk {
                            anchor,
                            lines: vec![],
                            end: false,
                        });
                    } else if lines[i] == "*** End of File" {
                        operation
                            .hunks
                            .last_mut()
                            .ok_or_else(|| anyhow!("end marker without hunk"))?
                            .end = true;
                    } else {
                        if !lines[i].starts_with([' ', '+', '-']) {
                            bail!("invalid hunk line");
                        }
                        operation
                            .hunks
                            .last_mut()
                            .ok_or_else(|| anyhow!("update requires @@ hunk"))?
                            .lines
                            .push(lines[i].to_owned());
                    }
                }
            }
            i += 1;
        }
        operations.push(operation);
    }
    if operations.is_empty() {
        bail!("empty patch");
    }
    Ok(operations)
}

fn apply_hunks(original: &str, hunks: &[Hunk]) -> Result<String> {
    let crlf = original.contains("\r\n");
    let had_newline = original.ends_with('\n');
    let mut lines: Vec<String> = original.lines().map(str::to_owned).collect();
    let mut cursor = 0;
    for hunk in hunks {
        if let Some(anchor) = &hunk.anchor {
            cursor = lines[cursor..]
                .iter()
                .position(|line| line == anchor)
                .map(|n| cursor + n + 1)
                .ok_or_else(|| anyhow!("patch anchor not found: {anchor}"))?;
        }
        let before: Vec<_> = hunk
            .lines
            .iter()
            .filter(|line| !line.starts_with('+'))
            .map(|line| line[1..].to_owned())
            .collect();
        let after: Vec<_> = hunk
            .lines
            .iter()
            .filter(|line| !line.starts_with('-'))
            .map(|line| line[1..].to_owned())
            .collect();
        let start = if before.is_empty() {
            if hunk.end {
                lines.len()
            } else {
                cursor
            }
        } else {
            (cursor..=lines.len().saturating_sub(before.len()))
                .find(|start| {
                    *start + before.len() <= lines.len()
                        && lines[*start..*start + before.len()] == before
                        && (!hunk.end || *start + before.len() == lines.len())
                })
                .ok_or_else(|| anyhow!("patch context does not match; re-read file"))?
        };
        let count = after.len();
        lines.splice(start..start + before.len(), after);
        cursor = start + count;
    }
    let separator = if crlf { "\r\n" } else { "\n" };
    let mut text = lines.join(separator);
    if had_newline && !lines.is_empty() {
        text.push_str(separator);
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stop_rejects_queued_blocking_patch_without_workspace_changes() {
        let root = tempfile::tempdir().unwrap();
        let tools = WorkspaceTools::new(root.path(), 1).unwrap();
        let permit = tools.io.clone().acquire_owned().await.unwrap();
        let args = json!({"patch":"*** Begin Patch\n*** Add File: stopped.txt\n+must not be written\n*** End Patch"});
        let mut operation = Box::pin(tools.execute("apply_patch", &args));
        // Poll the real API up to I/O admission, deterministically capturing its
        // generation while the blocking-pool permit is still unavailable.
        assert!(futures_util::poll!(operation.as_mut()).is_pending());
        tools.stop_processes();
        drop(permit);
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), operation)
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("cancelled by owner stop"));
        assert!(!root.path().join("stopped.txt").exists());
        tools.execute("apply_patch", &args).await.unwrap();
        assert!(
            root.path().join("stopped.txt").is_file(),
            "new work uses a fresh generation"
        );
    }

    #[test]
    fn stop_interrupts_already_running_blocking_read_before_next_chunk() {
        struct StopDuringRead<'a>(&'a WorkspaceTools, usize);
        impl Read for StopDuringRead<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.1 += 1;
                assert_eq!(self.1, 1, "cancelled read must not request another chunk");
                buffer.fill(b'x');
                self.0.stop_processes();
                Ok(buffer.len())
            }
        }
        let root = tempfile::tempdir().unwrap();
        let tools = WorkspaceTools::new(root.path(), 1).unwrap();
        let generation = tools.pty.generation();
        let mut reader = StopDuringRead(&tools, 0);
        let error = tools
            .read_bytes(&mut reader, TEXT_BYTES, generation)
            .unwrap_err();
        assert!(error.to_string().contains("cancelled by owner stop"));
        assert_eq!(reader.1, 1);
    }

    #[test]
    fn caller_drop_interrupts_running_read_even_with_current_stop_generation() {
        struct DropDuringRead(Option<OperationCancellation>);
        impl Read for DropDuringRead {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                assert!(
                    self.0.is_some(),
                    "dropped caller must prevent the next read"
                );
                buffer.fill(b'x');
                drop(self.0.take());
                Ok(buffer.len())
            }
        }
        let root = tempfile::tempdir().unwrap();
        let mut tools = WorkspaceTools::new(root.path(), 1).unwrap();
        // Simulate the worker entering execute just after global stop advanced
        // the generation, then losing its caller while blocking I/O runs.
        tools.stop_processes();
        let cancelled = Arc::new(AtomicBool::new(false));
        tools.cancelled = Some(cancelled.clone());
        let mut reader = DropDuringRead(Some(OperationCancellation {
            cancelled,
            completed: false,
        }));
        let error = tools
            .read_bytes(&mut reader, TEXT_BYTES, tools.pty.generation())
            .unwrap_err();
        assert!(error.to_string().contains("cancelled by owner stop"));
    }

    #[test]
    fn exact_patch_preserves_line_endings_and_rejects_bad_context() {
        let ops=parse_patch("*** Begin Patch\n*** Update File: src/a.rs\n@@\n one\n-two\n+three\n four\n*** End Patch").unwrap();
        assert_eq!(
            apply_hunks("one\r\ntwo\r\nfour\r\n", &ops[0].hunks).unwrap(),
            "one\r\nthree\r\nfour\r\n"
        );
        assert!(apply_hunks("different\n", &ops[0].hunks).is_err());
    }
    #[test]
    fn line_reader_bounds_huge_single_line() {
        let bytes = vec![b'x'; PREVIEW_BYTES * 3];
        let (preview, consumed, long, eof) =
            bounded_line(&mut std::io::Cursor::new(bytes), PREVIEW_BYTES, || Ok(())).unwrap();
        assert_eq!(preview.len(), PREVIEW_BYTES);
        assert_eq!(consumed, (PREVIEW_BYTES * 3) as u64);
        assert!(long && eof);
    }
    #[tokio::test]
    async fn patch_preflight_pagination_and_command_spooling() {
        let temp = tempfile::tempdir().unwrap();
        let tools = WorkspaceTools::new(temp.path(), 2).unwrap();
        let patch = "*** Begin Patch\n*** Add File: a.txt\n+first\n+second\n+third\n*** End Patch";
        tools
            .execute("apply_patch", &json!({"patch":patch}))
            .await
            .unwrap();
        let page = tools
            .execute("read_file", &json!({"path":"a.txt","limit":2}))
            .await
            .unwrap();
        assert_eq!(page["content"], "1: first\n2: second\n");
        assert_eq!(page["next_offset"], 3);
        assert_eq!(page["has_more"], true);
        let page = tools
            .execute("read_file", &json!({"path":"a.txt","offset":3}))
            .await
            .unwrap();
        assert_eq!(page["content"], "3: third\n");
        assert!(tools
            .execute("read_file", &json!({"path":"../escape"}))
            .await
            .is_err());
        let absolute = tools
            .execute(
                "read_file",
                &json!({"path":temp.path().join("a.txt").to_string_lossy(),"limit":1}),
            )
            .await
            .unwrap();
        assert_eq!(absolute["content"], "1: first\n");
        fs::write(temp.path().join("long.txt"), vec![b'x'; PREVIEW_BYTES * 2]).unwrap();
        let long = tools
            .execute("read_file", &json!({"path":"long.txt"}))
            .await
            .unwrap();
        assert_eq!(long["long_line"], true);
        assert_eq!(long["next_byte_offset"], PREVIEW_BYTES);
        let bad="*** Begin Patch\n*** Add File: should-not-exist\n+test\n*** Update File: a.txt\n@@\n-missing\n+replacement\n*** End Patch";
        assert!(tools
            .execute("apply_patch", &json!({"patch":bad}))
            .await
            .is_err());
        assert!(!temp.path().join("should-not-exist").exists());
        let search = tools
            .execute(
                "search_files",
                &json!({"query":"first|second|third", "pattern":"*.txt", "limit":1}),
            )
            .await
            .unwrap();
        assert_eq!(search["matches"][0]["line"], 1);
        assert_eq!(search["has_more"], true);
        let search = tools
            .execute(
                "search_files",
                &json!({"query":"first|second|third", "pattern":"*.txt", "offset":1}),
            )
            .await
            .unwrap();
        assert_eq!(search["matches"].as_array().unwrap().len(), 2);
        assert_eq!(search["matches"][0]["line"], 2);
        let files = tools
            .execute("list_files", &json!({"pattern":"*.txt", "limit":1}))
            .await
            .unwrap();
        assert_eq!(files["files"][0], "a.txt");
        assert_eq!(files["has_more"], true);
        let files = tools
            .execute("list_files", &json!({"pattern":"*.txt", "offset":1}))
            .await
            .unwrap();
        assert_eq!(files["files"][0], "long.txt");
        let moved = "*** Begin Patch\n*** Update File: a.txt\n*** Move to: sub/moved.txt\n@@\n-first\n+changed\n second\n*** End Patch";
        tools
            .execute("apply_patch", &json!({"patch":moved}))
            .await
            .unwrap();
        assert!(!temp.path().join("a.txt").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("sub/moved.txt")).unwrap(),
            "changed\nsecond\nthird\n"
        );
        let text = if cfg!(windows) {
            "[Console]::Out.Write(('x' * 70000)); [Console]::Error.Write(('y' * 70000))"
        } else {
            "head -c 70000 /dev/zero | tr '\\0' x; head -c 70000 /dev/zero | tr '\\0' y >&2"
        };
        let result = tools
            .execute("run_command", &json!({"command":text}))
            .await
            .unwrap();
        assert_eq!(result["success"], true);
        assert_eq!(result["stdout_bytes"], 70000);
        assert_eq!(result["stderr_bytes"], 70000);
        assert_eq!(result["stderr"].as_str().unwrap().len(), PREVIEW_BYTES);
        assert_eq!(result["truncated"], true);
        assert_eq!(
            fs::metadata(temp.path().join(result["stdout_path"].as_str().unwrap()))
                .unwrap()
                .len(),
            70000
        );
        assert_eq!(
            fs::metadata(temp.path().join(result["stderr_path"].as_str().unwrap()))
                .unwrap()
                .len(),
            70000
        );
    }
}
