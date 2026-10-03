//! Native persistent terminals: ConPTY on Windows, Unix PTYs elsewhere.
//! Output is continuously drained to disk, independent of model tool polling.
use anyhow::{bail, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};

const PREVIEW_BYTES: usize = 64 * 1024;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct Hub(Arc<Inner>);

struct Inner {
    sessions: Mutex<BTreeMap<String, Arc<Session>>>,
    generation: AtomicU64,
    processes: Arc<Semaphore>,
    capacity: usize,
    active: Arc<AtomicUsize>,
    capacity_changed: watch::Sender<usize>,
}

struct PtyPermit {
    _permit: OwnedSemaphorePermit,
    active: Arc<AtomicUsize>,
    changed: watch::Sender<usize>,
}

impl Drop for PtyPermit {
    fn drop(&mut self) {
        let remaining = self.active.fetch_sub(1, Ordering::AcqRel) - 1;
        self.changed.send_replace(remaining);
    }
}

struct Session {
    id: String,
    program: String,
    process_id: Option<u32>,
    output_path: PathBuf,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    state: Mutex<State>,
    input_ready: Condvar,
    monitor: Mutex<Option<JoinHandle<()>>>,
    activity: Option<ProcessActivity>,
    cancelled_spawn: Option<Arc<AtomicBool>>,
}

#[derive(Clone)]
pub(crate) struct ProcessActivity {
    pub metrics: Arc<crate::metrics::Metrics>,
    pub agent_id: String,
}

pub(crate) struct ExecutionContext {
    pub generation: u64,
    pub activity: Option<ProcessActivity>,
    pub cancelled: Option<Arc<AtomicBool>>,
}

impl ProcessActivity {
    pub fn writer(self) -> ProcessOutput {
        ProcessOutput {
            activity: self,
            pending: Vec::new(),
        }
    }
}

/// Every pipe owns its decoder: reads may split a Unicode character, while
/// stdout and stderr may concurrently end in different incomplete characters.
pub(crate) struct ProcessOutput {
    activity: ProcessActivity,
    pending: Vec<u8>,
}

impl ProcessOutput {
    pub fn append(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let mut consumed = 0;
        while consumed < self.pending.len() {
            match std::str::from_utf8(&self.pending[consumed..]) {
                Ok(text) => {
                    self.activity
                        .metrics
                        .append_activity(&self.activity.agent_id, text);
                    consumed = self.pending.len();
                }
                Err(error) => {
                    let valid_end = consumed + error.valid_up_to();
                    if valid_end > consumed {
                        let text = std::str::from_utf8(&self.pending[consumed..valid_end])
                            .expect("validated UTF-8 prefix");
                        self.activity
                            .metrics
                            .append_activity(&self.activity.agent_id, text);
                    }
                    consumed = valid_end;
                    if let Some(length) = error.error_len() {
                        self.activity
                            .metrics
                            .append_activity(&self.activity.agent_id, "\u{fffd}");
                        consumed += length;
                    } else {
                        break;
                    }
                }
            }
        }
        self.pending.drain(..consumed);
    }

    pub fn finish(&mut self) {
        if !self.pending.is_empty() {
            self.activity.metrics.append_activity(
                &self.activity.agent_id,
                &String::from_utf8_lossy(&self.pending),
            );
            self.pending.clear();
        }
    }
}

#[derive(Default)]
struct State {
    exit_code: Option<u32>,
    killed: bool,
    output_complete: bool,
    output_error: Option<String>,
    input_ready: bool,
    rows: u16,
    cols: u16,
}

impl Hub {
    pub fn new(processes: Arc<Semaphore>) -> Self {
        let (capacity_changed, _) = watch::channel(0);
        Self(Arc::new(Inner {
            sessions: Mutex::new(BTreeMap::new()),
            generation: AtomicU64::new(0),
            capacity: processes.available_permits(),
            processes,
            active: Arc::new(AtomicUsize::new(0)),
            capacity_changed,
        }))
    }

    /// Ordinary commands queue behind ordinary commands. Persistent terminals
    /// occupying every slot produce actionable capacity feedback, so a worker can still
    /// reach the next pty_kill tool that releases its terminal's slot.
    pub async fn command_permit(&self) -> Result<OwnedSemaphorePermit> {
        let mut changed = self.0.capacity_changed.subscribe();
        loop {
            if self.0.active.load(Ordering::Acquire) >= self.0.capacity {
                return self.0.processes.clone().try_acquire_owned().context("native process capacity is full with a persistent PTY; use pty_list then pty_kill before running another command");
            }
            tokio::select! {
                permit = self.0.processes.clone().acquire_owned() => return Ok(permit?),
                result = changed.changed() => { result.context("PTY capacity notifier closed")?; }
            }
        }
    }

    pub fn schemas() -> Vec<Value> {
        vec![
            schema("pty_spawn", "Start an executable or shell command in a persistent native PTY (ConPTY on Windows). No timeout. Output is fully saved to disk. Use pty_write/read/resize/kill with returned id; all workers share the terminal registry. Active PTYs share the bounded native command capacity.", json!({
                "program":{"type":"string"}, "args":{"type":"array","items":{"type":"string"}},
                "command":{"type":"string"}, "shell":{"type":"string","enum":["default","bash","powershell","cmd","sh"]},
                "workdir":{"type":"string"}, "rows":{"type":"integer","minimum":1,"maximum":1000},
                "cols":{"type":"integer","minimum":1,"maximum":1000}
            }), &[]),
            schema("pty_write", "Send exact input to a running native PTY. Include a carriage return or newline to submit, or control characters such as \u{0003} for Ctrl+C. Input is not modified.", json!({"id":{"type":"string"},"data":{"type":"string"}}), &["id","data"]),
            schema("pty_read", "Read a bounded PTY output page by byte offset. content is a readable UTF-8 preview including terminal escape sequences; content_hex preserves exact bytes even across split Unicode characters. Continue at next_byte_offset; output_complete says the child and output drain have finished. Full output remains at output_path.", json!({"id":{"type":"string"},"byte_offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":PREVIEW_BYTES}}), &["id"]),
            schema("pty_resize", "Resize the native terminal and notify its process.", json!({"id":{"type":"string"},"rows":{"type":"integer","minimum":1,"maximum":1000},"cols":{"type":"integer","minimum":1,"maximum":1000}}), &["id","rows","cols"]),
            schema("pty_list", "List shared native PTY sessions, including exited sessions and their durable output paths.", json!({}), &[]),
            schema("pty_kill", "Explicitly terminate and reap a PTY process, draining output. cleanup removes the registry entry but preserves its output log. No automatic duration aborts.", json!({"id":{"type":"string"},"cleanup":{"type":"boolean"}}), &["id"]),
        ]
    }

    /// Called from a blocking worker, so OS terminal operations never block Tokio.
    pub fn execute(
        &self,
        name: &str,
        args: &Value,
        workdir: &Path,
        output_dir: &Path,
    ) -> Result<Value> {
        self.execute_at_generation(name, args, workdir, output_dir, self.generation())
    }

    pub fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Acquire)
    }

    /// Invalidate pending spawns before signalling every existing terminal.
    /// Reaping/output draining continues on the terminal's monitor thread.
    pub fn stop_all(&self) {
        let sessions = {
            let sessions = self.0.sessions.lock().unwrap_or_else(|e| e.into_inner());
            self.0.generation.fetch_add(1, Ordering::AcqRel);
            sessions.values().cloned().collect::<Vec<_>>()
        };
        for session in sessions {
            let _ = session.stop();
        }
    }

    pub fn execute_at_generation(
        &self,
        name: &str,
        args: &Value,
        workdir: &Path,
        output_dir: &Path,
        generation: u64,
    ) -> Result<Value> {
        self.execute_with_activity(
            name,
            args,
            workdir,
            output_dir,
            ExecutionContext {
                generation,
                activity: None,
                cancelled: None,
            },
        )
    }

    pub(crate) fn execute_with_activity(
        &self,
        name: &str,
        args: &Value,
        workdir: &Path,
        output_dir: &Path,
        context: ExecutionContext,
    ) -> Result<Value> {
        let allowed: &[&str] = match name {
            "pty_spawn" => &[
                "program", "args", "command", "shell", "workdir", "rows", "cols",
            ],
            "pty_write" => &["id", "data"],
            "pty_read" => &["id", "byte_offset", "limit"],
            "pty_resize" => &["id", "rows", "cols"],
            "pty_list" => &[],
            "pty_kill" => &["id", "cleanup"],
            _ => bail!("unknown PTY tool: {name}"),
        };
        let object = args
            .as_object()
            .context("PTY arguments must be an object")?;
        for key in object.keys() {
            if !allowed.contains(&key.as_str()) {
                bail!("unsupported argument: {key}");
            }
        }
        if name == "pty_spawn" {
            return self.spawn(
                args,
                workdir,
                output_dir,
                context.generation,
                context.activity,
                context.cancelled,
            );
        }
        if name == "pty_list" {
            let sessions = self.0.sessions.lock().unwrap_or_else(|e| e.into_inner());
            return Ok(json!({"sessions":sessions.values().map(|s| s.info()).collect::<Vec<_>>()}));
        }
        let session = self.session(text(args, "id")?)?;
        match name {
            "pty_write" => {
                let data = text(args, "data")?;
                if data.len() > PREVIEW_BYTES {
                    bail!("PTY input exceeds {PREVIEW_BYTES} bytes; send consecutive parts");
                }
                let mut state = session.state.lock().unwrap_or_else(|e| e.into_inner());
                while !state.input_ready && state.exit_code.is_none() {
                    state = session
                        .input_ready
                        .wait(state)
                        .unwrap_or_else(|e| e.into_inner());
                }
                if state.exit_code.is_some() {
                    bail!("PTY process has exited");
                }
                drop(state);
                let mut writer = session.writer.lock().unwrap_or_else(|e| e.into_inner());
                let writer = writer.as_mut().context("PTY input is closed")?;
                writer
                    .write_all(data.as_bytes())
                    .context("write PTY input")?;
                writer.flush()?;
                Ok(json!({"id":session.id,"written_bytes":data.len()}))
            }
            "pty_read" => {
                let offset = integer(args, "byte_offset", 0)?;
                let limit = integer(args, "limit", PREVIEW_BYTES as u64)?;
                if !(1..=PREVIEW_BYTES as u64).contains(&limit) {
                    bail!("limit must be between 1 and {PREVIEW_BYTES}");
                }
                // Sample completion before the file size: a true completion
                // flag must always describe a fully drained output snapshot.
                let mut info = session.info();
                let mut file = File::open(&session.output_path)?;
                let size = file.metadata()?.len();
                if offset > size {
                    bail!("byte_offset {offset} exceeds current output size {size}");
                }
                file.seek(SeekFrom::Start(offset))?;
                let mut bytes = Vec::new();
                file.take(limit.min(size - offset))
                    .read_to_end(&mut bytes)?;
                let next = offset + bytes.len() as u64;
                info["content"] = json!(String::from_utf8_lossy(&bytes));
                info["content_hex"] = json!(hex(&bytes));
                info["byte_offset"] = json!(offset);
                info["next_byte_offset"] = json!(next);
                info["size_bytes"] = json!(size);
                info["has_more"] = json!(next < size);
                Ok(info)
            }
            "pty_resize" => {
                if args.get("rows").is_none() || args.get("cols").is_none() {
                    bail!("pty_resize requires rows and cols");
                }
                let size = dimensions(args)?;
                session
                    .master
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .context("PTY process has exited")?
                    .resize(size)?;
                let mut state = session.state.lock().unwrap_or_else(|e| e.into_inner());
                state.rows = size.rows;
                state.cols = size.cols;
                drop(state);
                Ok(session.info())
            }
            "pty_kill" => {
                let cleanup = args
                    .get("cleanup")
                    .map(|v| v.as_bool().context("cleanup must be a boolean"))
                    .transpose()?
                    .unwrap_or(false);
                session.stop()?;
                session.join();
                let info = session.info();
                if cleanup {
                    self.0
                        .sessions
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&session.id);
                }
                Ok(info)
            }
            _ => unreachable!(),
        }
    }

    fn session(&self, id: &str) -> Result<Arc<Session>> {
        self.0
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .with_context(|| format!("unknown PTY session: {id}"))
    }

    fn spawn(
        &self,
        args: &Value,
        workdir: &Path,
        output_dir: &Path,
        generation: u64,
        activity: Option<ProcessActivity>,
        cancelled: Option<Arc<AtomicBool>>,
    ) -> Result<Value> {
        if generation != self.generation()
            || cancelled
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            bail!("PTY spawn cancelled by owner stop");
        }
        if let Some(value) = args.get("workdir") {
            value.as_str().context("workdir must be a string")?;
        }
        if !workdir.is_dir() {
            bail!("PTY workdir must be a directory");
        }
        let permit = self.0.processes.clone().try_acquire_owned().context(
            "native process capacity is full; finish or kill a running PTY/command before spawning",
        )?;
        let count = self.0.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.0.capacity_changed.send_replace(count);
        let permit = PtyPermit {
            _permit: permit,
            active: self.0.active.clone(),
            changed: self.0.capacity_changed.clone(),
        };
        let (mut command, program) = command(args)?;
        command.cwd(workdir);
        let size = dimensions(args)?;
        fs::create_dir_all(output_dir)?;
        let id = format!(
            "pty-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        );
        let output_path = output_dir.join(format!("{id}.pty.log"));
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&output_path)?;
        let pair = native_pty_system()
            .openpty(size)
            .context("open native PTY")?;
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let child = pair
            .slave
            .spawn_command(command)
            .context("spawn native PTY command")?;
        drop(pair.slave);
        let session = Arc::new(Session {
            id: id.clone(),
            program,
            process_id: child.process_id(),
            output_path,
            child: Mutex::new(child),
            master: Mutex::new(Some(pair.master)),
            writer: Mutex::new(Some(writer)),
            state: Mutex::new(State {
                rows: size.rows,
                cols: size.cols,
                input_ready: !cfg!(windows),
                ..State::default()
            }),
            input_ready: Condvar::new(),
            monitor: Mutex::new(None),
            activity,
            cancelled_spawn: cancelled.clone(),
        });
        let drain_session = session.clone();
        let drain = thread::spawn(move || drain_output(reader, file, &drain_session));
        let monitor_session = session.clone();
        let monitor = thread::spawn(move || monitor_child(&monitor_session, drain, permit));
        *session.monitor.lock().unwrap_or_else(|e| e.into_inner()) = Some(monitor);
        let info = session.info();
        let mut sessions = self.0.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if generation != self.generation()
            || cancelled
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            drop(sessions);
            // spawn_blocking cannot be aborted once admitted. A stop racing OS
            // creation must therefore terminate the newly created child itself.
            let _ = session.stop();
            session.join();
            bail!("PTY spawn cancelled by owner stop");
        }
        sessions.insert(id, session);
        Ok(info)
    }
}

impl Session {
    fn info(&self) -> Value {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        json!({"id":self.id,"program":self.program,"pid":self.process_id,"status":if state.exit_code.is_some() {if state.killed {"killed"} else {"exited"}} else {"running"},
            "exit_code":state.exit_code,"output_complete":state.output_complete,"output_error":state.output_error,
            "output_path":self.output_path,"rows":state.rows,"cols":state.cols})
    }
    fn stop(&self) -> Result<()> {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let running = child.try_wait()?.is_none();
        #[cfg(unix)]
        {
            let incomplete = !self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .output_complete;
            // The leader may already have exited while a background shell job
            // retains the slave. Its original group must remain killable until
            // the output drain is complete, including explicit kill/drop.
            if incomplete {
                if let Some(pid) = self.process_id {
                    // The native PTY child is a session/process-group leader. Kill
                    // its group so shell children cannot retain the output slave
                    // indefinitely after their parent is explicitly terminated.
                    if kill_process_group(pid)? {
                        self.state.lock().unwrap_or_else(|e| e.into_inner()).killed = true;
                    }
                } else if running {
                    child.kill().context("kill PTY process")?;
                }
            }
        }
        #[cfg(windows)]
        if running {
            child.kill().context("kill PTY process")?;
        }
        if running {
            self.state.lock().unwrap_or_else(|e| e.into_inner()).killed = true;
        }
        Ok(())
    }
    fn join(&self) {
        // Keep concurrent cleanup callers behind the same completed join,
        // rather than returning early after another caller takes its handle.
        let mut monitor = self.monitor.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = monitor.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let sessions = self.sessions.get_mut().unwrap_or_else(|e| e.into_inner());
        for session in sessions.values() {
            let _ = session.stop();
        }
        for session in sessions.values() {
            session.join();
        }
    }
}

fn monitor_child(session: &Session, drain: JoinHandle<()>, permit: PtyPermit) {
    loop {
        if session
            .cancelled_spawn
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            let _ = session.stop();
        }
        let status = session
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_wait();
        match status {
            Ok(Some(status)) => {
                session
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .exit_code = Some(status.exit_code());
                session.input_ready.notify_all();
                break;
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                session
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .output_error = Some(format!("wait for PTY child: {error}"));
                let mut child = session.child.lock().unwrap_or_else(|e| e.into_inner());
                let _ = child.kill();
                if let Ok(status) = child.wait() {
                    session
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .exit_code = Some(status.exit_code());
                }
                session.input_ready.notify_all();
                break;
            }
        }
    }
    session
        .writer
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    // ConPTY close requires a concurrently active output reader to avoid deadlock.
    session
        .master
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if drain.join().is_err() {
        session
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .output_error = Some("PTY output-drain thread panicked".to_owned());
    }
    // Darwin can report terminal EOF as soon as the session leader exits even
    // when a HUP-ignoring descendant survives. Once leader and terminal output
    // are both finished, reap the remaining group before releasing capacity.
    // Do this here rather than against an old completed session's reusable PID.
    #[cfg(unix)]
    if let Some(pid) = session.process_id {
        if let Err(error) = kill_process_group(pid) {
            session
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .output_error = Some(format!("clean up PTY process group: {error:#}"));
        }
    }
    drop(permit);
    session
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .output_complete = true;
}

#[cfg(unix)]
fn kill_process_group(pid: u32) -> Result<bool> {
    use nix::{
        sys::signal::{killpg, Signal},
        unistd::Pid,
    };
    match killpg(Pid::from_raw(pid as i32), Signal::SIGKILL) {
        Ok(()) => Ok(true),
        Err(nix::errno::Errno::ESRCH) => Ok(false),
        Err(error) => Err(error).context("kill PTY process group"),
    }
}

fn drain_output(mut reader: Box<dyn Read + Send>, mut file: File, session: &Session) {
    let mut buffer = [0u8; 8192];
    let mut disk_error = None;
    let mut activity = session.activity.clone().map(ProcessActivity::writer);
    #[cfg(windows)]
    let mut cursor_query = Vec::new();
    #[cfg(windows)]
    let mut inherited_cursor_answered = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if let Some(activity) = &mut activity {
                    activity.append(&buffer[..n]);
                }
                // portable-pty enables INHERIT_CURSOR on ConPTY. Without its
                // initial DSR response the console host can wait forever before
                // starting normal output. Our headless terminal starts at 1,1.
                #[cfg(windows)]
                if !inherited_cursor_answered {
                    cursor_query.extend_from_slice(&buffer[..n]);
                    if cursor_query.windows(4).any(|w| w == b"\x1b[6n") {
                        let mut writer = session.writer.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(writer) = writer.as_mut() {
                            let _ = writer.write_all(b"\x1b[1;1R");
                            let _ = writer.flush();
                        }
                        // Ordinary input cannot hold the writer until the
                        // console host receives this bootstrap response.
                        session
                            .state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .input_ready = true;
                        session.input_ready.notify_all();
                        inherited_cursor_answered = true;
                    }
                    if cursor_query.len() > 3 {
                        cursor_query.drain(..cursor_query.len() - 3);
                    }
                }
                if disk_error.is_none() {
                    if let Err(error) = file.write_all(&buffer[..n]) {
                        disk_error = Some(error.to_string());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            // Linux signals the closed slave with EIO, which is normal PTY EOF.
            Err(error) if cfg!(unix) && error.raw_os_error() == Some(5) => break,
            Err(error) => {
                disk_error = Some(format!("read PTY output: {error}"));
                break;
            }
        }
    }
    if let Some(activity) = &mut activity {
        activity.finish();
    }
    if let Err(error) = file.flush() {
        disk_error.get_or_insert(error.to_string());
    }
    if let Some(error) = disk_error {
        session
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .output_error = Some(error);
    }
}

fn dimensions(args: &Value) -> Result<PtySize> {
    let rows = integer(args, "rows", 24)?;
    let cols = integer(args, "cols", 80)?;
    if !(1..=1000).contains(&rows) || !(1..=1000).contains(&cols) {
        bail!("PTY rows and cols must be between 1 and 1000");
    }
    Ok(PtySize {
        rows: rows as u16,
        cols: cols as u16,
        pixel_width: 0,
        pixel_height: 0,
    })
}

fn command(args: &Value) -> Result<(CommandBuilder, String)> {
    if args.get("program").is_some() && args.get("command").is_some() {
        bail!("use program/args or command/shell, not both");
    }
    if let Some(program) = args.get("program") {
        let program = program.as_str().context("program must be a string")?;
        if program.trim().is_empty() {
            bail!("program cannot be empty");
        }
        if args.get("shell").is_some() {
            bail!("shell only applies to command");
        }
        let mut builder = CommandBuilder::new(program);
        if let Some(values) = args.get("args") {
            for value in values
                .as_array()
                .context("args must be an array of strings")?
            {
                builder.arg(value.as_str().context("args must contain strings")?);
            }
        }
        return Ok((builder, program.to_owned()));
    }
    if args.get("args").is_some() {
        bail!("args requires program");
    }
    let text = text(args, "command")?;
    let shell = args
        .get("shell")
        .map(|v| v.as_str().context("shell must be a string"))
        .transpose()?
        .unwrap_or("default");
    let shell = if shell == "default" {
        if cfg!(windows) {
            "powershell"
        } else {
            "sh"
        }
    } else {
        shell
    };
    let (program, prefix): (&str, &[&str]) = match shell {
        "powershell" => ("powershell.exe", &["-NoLogo", "-NoProfile", "-Command"]),
        "cmd" => ("cmd.exe", &["/D", "/S", "/C"]),
        "bash" => (
            if cfg!(windows) && Path::new("C:\\Program Files\\Git\\bin\\bash.exe").exists() {
                "C:\\Program Files\\Git\\bin\\bash.exe"
            } else {
                "bash"
            },
            &["-c"],
        ),
        "sh" => ("sh", &["-c"]),
        _ => bail!("unsupported shell: {shell}"),
    };
    let mut builder = CommandBuilder::new(program);
    builder.args(prefix);
    builder.arg(text);
    Ok((builder, program.to_owned()))
}
fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("{key} must be a string"))
}
fn integer(args: &Value, key: &str, default: u64) -> Result<u64> {
    args.get(key)
        .map(|v| {
            v.as_u64()
                .with_context(|| format!("{key} must be a nonnegative integer"))
        })
        .unwrap_or(Ok(default))
}
fn schema(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"type":"function","function":{"name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}})
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 15) as usize] as char);
    }
    output
}

#[cfg(test)]
mod activity_tests {
    use super::*;

    #[test]
    fn process_activity_preserves_split_unicode_and_independent_pipe_decoders() {
        let metrics = Arc::new(crate::metrics::Metrics::new(1));
        let activity = ProcessActivity {
            metrics: metrics.clone(),
            agent_id: "agent-001".into(),
        };
        let mut stdout = activity.clone().writer();
        let mut stderr = activity.writer();
        stdout.append(&[0xf0, 0x9f]);
        stderr.append(&[0xce]);
        assert_eq!(metrics.agent_activity("agent-001"), "");
        stdout.append(&[0x99, 0x82]);
        stderr.append(&[0xbb]);
        stdout.append(&[0xff, b'!', 0xe2]);
        stdout.finish();
        stderr.finish();
        assert_eq!(metrics.agent_activity("agent-001"), "🙂λ\u{fffd}!\u{fffd}");
        assert!(stdout.pending.is_empty());
        assert!(stderr.pending.is_empty());
    }
}
