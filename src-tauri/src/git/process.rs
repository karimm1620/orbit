use std::{
    env,
    ffi::{OsStr, OsString},
    io::{self, Read},
    path::Path,
    process::{Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::{fd::AsRawFd, unix::process::CommandExt};

use crate::error::OrbitError;

const STDERR_LIMIT: usize = 64 * 1024;
const NO_LAZY_FETCH_ENVIRONMENT: &str = "GIT_NO_LAZY_FETCH";
const MUTATION_READER_GRACE: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub struct GitOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitMutationTermination {
    Exited,
    TimedOut,
    OutputTooLarge,
    RunnerFailed,
}

#[derive(Debug)]
pub(crate) struct GitMutationOutput {
    pub status: Option<ExitStatus>,
    pub stderr: Vec<u8>,
    pub termination: GitMutationTermination,
}

#[derive(Clone)]
pub struct GitRunner {
    executable: OsString,
    no_lazy_fetch_capability: Arc<OnceLock<Result<(), OrbitError>>>,
    #[cfg(test)]
    environment: Vec<(OsString, OsString)>,
    #[cfg(test)]
    restore_no_lazy_fetch_environment: bool,
    #[cfg(test)]
    mutation_deadline_override: Option<Duration>,
}

impl Default for GitRunner {
    fn default() -> Self {
        Self {
            executable: OsString::from("git"),
            no_lazy_fetch_capability: Arc::new(OnceLock::new()),
            #[cfg(test)]
            environment: Vec::new(),
            #[cfg(test)]
            restore_no_lazy_fetch_environment: true,
            #[cfg(test)]
            mutation_deadline_override: None,
        }
    }
}

impl GitRunner {
    #[cfg(test)]
    pub fn with_executable(executable: impl Into<OsString>) -> Self {
        Self {
            executable: executable.into(),
            no_lazy_fetch_capability: Arc::new(OnceLock::new()),
            environment: Vec::new(),
            restore_no_lazy_fetch_environment: true,
            mutation_deadline_override: None,
        }
    }

    #[cfg(test)]
    pub fn with_environment(
        mut self,
        name: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        self.environment.push((name.into(), value.into()));
        self
    }

    #[cfg(test)]
    pub fn without_no_lazy_fetch_environment(mut self) -> Self {
        self.restore_no_lazy_fetch_environment = false;
        self
    }

    #[cfg(test)]
    pub fn with_mutation_deadline(mut self, deadline: Duration) -> Self {
        self.mutation_deadline_override = Some(deadline);
        self
    }

    pub fn version(&self) -> Result<String, OrbitError> {
        let output = self.run(None, "detect_git", ["--version"], 4 * 1024)?;
        let output = self.require_success("detect_git", output)?;
        let version = std::str::from_utf8(&output.stdout)
            .map_err(|_| OrbitError::internal("detect_git", "Git returned an invalid version."))?
            .trim();

        if version.is_empty() {
            return Err(OrbitError::internal(
                "detect_git",
                "Git returned an empty version.",
            ));
        }

        Ok(version.to_owned())
    }

    pub fn require_no_lazy_fetch(&self) -> Result<(), OrbitError> {
        self.no_lazy_fetch_capability
            .get_or_init(|| {
                let output = self.run(
                    None,
                    "detect_git_capabilities",
                    ["--no-pager", "--no-lazy-fetch", "--version"],
                    4 * 1024,
                )?;

                if output.status.success() {
                    Ok(())
                } else {
                    Err(OrbitError::git_capability_unavailable())
                }
            })
            .clone()
    }

    pub fn run<I, S>(
        &self,
        current_dir: Option<&Path>,
        operation: &'static str,
        args: I,
        stdout_limit: usize,
    ) -> Result<GitOutput, OrbitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_inner(current_dir, operation, args, stdout_limit, None)
    }

    pub(crate) fn run_with_deadline<I, S>(
        &self,
        current_dir: Option<&Path>,
        operation: &'static str,
        args: I,
        stdout_limit: usize,
        deadline: Duration,
    ) -> Result<GitOutput, OrbitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_inner(current_dir, operation, args, stdout_limit, Some(deadline))
    }

    pub(crate) fn run_mutation<I, S>(
        &self,
        current_dir: &Path,
        operation: &'static str,
        args: I,
        stdout_limit: usize,
        deadline: Duration,
        termination_grace: Duration,
    ) -> Result<GitMutationOutput, OrbitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        #[cfg(test)]
        let deadline = self.mutation_deadline_override.unwrap_or(deadline);

        let mut command = Command::new(&self.executable);
        command
            .args(args)
            .current_dir(current_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(unix)]
        command.process_group(0);

        #[cfg(test)]
        command.envs(self.environment.iter().cloned());
        scrub_git_environment(&mut command, self.test_environment_names());
        #[cfg(not(test))]
        command.env(NO_LAZY_FETCH_ENVIRONMENT, "1");
        #[cfg(test)]
        if self.restore_no_lazy_fetch_environment {
            command.env(NO_LAZY_FETCH_ENVIRONMENT, "1");
        }

        let mut child = command
            .spawn()
            .map_err(|error| OrbitError::git_spawn(operation, &error))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| OrbitError::internal(operation, "Git stdout was unavailable."))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| OrbitError::internal(operation, "Git stderr was unavailable."))?;

        let cancelled = Arc::new(AtomicBool::new(false));
        let output_exceeded = Arc::new(AtomicBool::new(false));
        let stdout_done = Arc::new(AtomicBool::new(false));
        let stderr_done = Arc::new(AtomicBool::new(false));
        let stdout_reader = spawn_mutation_reader(
            stdout,
            stdout_limit,
            Arc::clone(&cancelled),
            Arc::clone(&output_exceeded),
            Arc::clone(&stdout_done),
        );
        let stderr_reader = spawn_mutation_reader(
            stderr,
            STDERR_LIMIT,
            Arc::clone(&cancelled),
            Arc::clone(&output_exceeded),
            Arc::clone(&stderr_done),
        );

        let (status, mut termination) =
            wait_for_mutation_child(&mut child, deadline, termination_grace, &output_exceeded);
        wait_for_mutation_readers(&stdout_done, &stderr_done, MUTATION_READER_GRACE);
        cancelled.store(true, Ordering::Release);
        let stdout = join_mutation_reader(stdout_reader);
        let stderr = join_mutation_reader(stderr_reader);

        let stdout_failed = stdout.is_err();
        let (stderr, stderr_failed) = match stderr {
            Ok(read) => (read.bytes, false),
            Err(_) => (Vec::new(), true),
        };
        if (stdout_failed || stderr_failed) && termination == GitMutationTermination::Exited {
            termination = GitMutationTermination::RunnerFailed;
        }

        Ok(GitMutationOutput {
            status,
            stderr,
            termination,
        })
    }

    fn run_inner<I, S>(
        &self,
        current_dir: Option<&Path>,
        operation: &'static str,
        args: I,
        stdout_limit: usize,
        deadline: Option<Duration>,
    ) -> Result<GitOutput, OrbitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(&self.executable);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(test)]
        command.envs(self.environment.iter().cloned());
        scrub_git_environment(&mut command, self.test_environment_names());
        #[cfg(not(test))]
        command.env(NO_LAZY_FETCH_ENVIRONMENT, "1");
        #[cfg(test)]
        if self.restore_no_lazy_fetch_environment {
            command.env(NO_LAZY_FETCH_ENVIRONMENT, "1");
        }

        if let Some(current_dir) = current_dir {
            command.current_dir(current_dir);
        }

        let mut child = command
            .spawn()
            .map_err(|error| OrbitError::git_spawn(operation, &error))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| OrbitError::internal(operation, "Git stdout was unavailable."))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| OrbitError::internal(operation, "Git stderr was unavailable."))?;

        let stdout_reader = thread::spawn(move || read_bounded(stdout, stdout_limit));
        let stderr_reader = thread::spawn(move || read_bounded(stderr, STDERR_LIMIT));
        let (status, timed_out) = wait_for_child(&mut child, operation, deadline)?;
        let stdout = join_reader(operation, stdout_reader)?;
        let stderr = join_reader(operation, stderr_reader)?;

        if timed_out {
            return Err(OrbitError::git_timed_out(operation));
        }

        if stdout.exceeded || stderr.exceeded {
            return Err(OrbitError::output_too_large(operation));
        }

        Ok(GitOutput {
            status,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
        })
    }

    pub fn require_success(
        &self,
        operation: &'static str,
        output: GitOutput,
    ) -> Result<GitOutput, OrbitError> {
        if output.status.success() {
            Ok(output)
        } else {
            Err(OrbitError::git_failed(operation, &output.stderr))
        }
    }

    #[cfg(test)]
    fn test_environment_names(&self) -> impl Iterator<Item = OsString> + '_ {
        self.environment.iter().map(|(name, _)| name.clone())
    }

    #[cfg(not(test))]
    fn test_environment_names(&self) -> impl Iterator<Item = OsString> + '_ {
        std::iter::empty()
    }
}

fn wait_for_child(
    child: &mut std::process::Child,
    operation: &'static str,
    deadline: Option<Duration>,
) -> Result<(ExitStatus, bool), OrbitError> {
    let Some(deadline) = deadline else {
        return child
            .wait()
            .map(|status| (status, false))
            .map_err(|error| OrbitError::internal(operation, error.to_string()));
    };

    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| OrbitError::internal(operation, error.to_string()))?
        {
            return Ok((status, false));
        }
        if started.elapsed() >= deadline {
            // `kill` can race with a natural exit. `wait` is still mandatory so a
            // timed-out Git child is reaped before control returns to the caller.
            let _ = child.kill();
            let status = child
                .wait()
                .map_err(|error| OrbitError::internal(operation, error.to_string()))?;
            return Ok((status, true));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_mutation_child(
    child: &mut std::process::Child,
    deadline: Duration,
    termination_grace: Duration,
    output_exceeded: &AtomicBool,
) -> (Option<ExitStatus>, GitMutationTermination) {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return (Some(status), GitMutationTermination::Exited),
            Ok(None) => {}
            Err(_) => {
                return (
                    terminate_mutation_process(child, termination_grace),
                    GitMutationTermination::RunnerFailed,
                );
            }
        }

        if output_exceeded.load(Ordering::Acquire) {
            return (
                terminate_mutation_process(child, termination_grace),
                GitMutationTermination::OutputTooLarge,
            );
        }
        if started.elapsed() >= deadline {
            return (
                terminate_mutation_process(child, termination_grace),
                GitMutationTermination::TimedOut,
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn terminate_mutation_process(
    child: &mut std::process::Child,
    termination_grace: Duration,
) -> Option<ExitStatus> {
    signal_process_group(child, TerminationSignal::Terminate);
    let grace_started = Instant::now();
    while grace_started.elapsed() < termination_grace {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break,
        }
    }

    signal_process_group(child, TerminationSignal::Kill);
    child.wait().ok()
}

enum TerminationSignal {
    Terminate,
    Kill,
}

#[cfg(unix)]
fn signal_process_group(child: &mut std::process::Child, signal: TerminationSignal) {
    let signal = match signal {
        TerminationSignal::Terminate => libc::SIGTERM,
        TerminationSignal::Kill => libc::SIGKILL,
    };
    let process_group = -(child.id() as libc::pid_t);
    // The process group was created by this runner immediately before spawn.
    // ESRCH is harmless when Git wins the race and exits naturally.
    unsafe {
        libc::kill(process_group, signal);
    }
}

#[cfg(not(unix))]
fn signal_process_group(child: &mut std::process::Child, signal: TerminationSignal) {
    if matches!(signal, TerminationSignal::Kill) {
        let _ = child.kill();
    }
}

fn wait_for_mutation_readers(stdout_done: &AtomicBool, stderr_done: &AtomicBool, grace: Duration) {
    let started = Instant::now();
    while started.elapsed() < grace
        && (!stdout_done.load(Ordering::Acquire) || !stderr_done.load(Ordering::Acquire))
    {
        thread::sleep(Duration::from_millis(2));
    }
}

fn scrub_git_environment(
    command: &mut Command,
    additional_names: impl IntoIterator<Item = OsString>,
) {
    for name in env::vars_os().map(|(name, _)| name).chain(additional_names) {
        if is_git_environment_name(&name) {
            command.env_remove(name);
        }
    }
}

fn is_git_environment_name(name: &OsStr) -> bool {
    name.to_string_lossy()
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("GIT_"))
}

struct BoundedRead {
    bytes: Vec<u8>,
    exceeded: bool,
}

#[cfg(unix)]
fn spawn_mutation_reader<R>(
    reader: R,
    limit: usize,
    cancelled: Arc<AtomicBool>,
    exceeded: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
) -> thread::JoinHandle<io::Result<BoundedRead>>
where
    R: Read + AsRawFd + Send + 'static,
{
    thread::spawn(move || {
        let result = read_bounded_cancellable(reader, limit, &cancelled, &exceeded);
        done.store(true, Ordering::Release);
        result
    })
}

#[cfg(not(unix))]
fn spawn_mutation_reader<R>(
    reader: R,
    limit: usize,
    cancelled: Arc<AtomicBool>,
    exceeded: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
) -> thread::JoinHandle<io::Result<BoundedRead>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let result = read_bounded_cancellable(reader, limit, &cancelled, &exceeded);
        done.store(true, Ordering::Release);
        result
    })
}

#[cfg(unix)]
fn read_bounded_cancellable<R>(
    mut reader: R,
    limit: usize,
    cancelled: &AtomicBool,
    exceeded: &AtomicBool,
) -> io::Result<BoundedRead>
where
    R: Read + AsRawFd,
{
    let descriptor = reader.as_raw_fd();
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }

    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let remaining = limit.saturating_sub(bytes.len());
                let retained = read.min(remaining);
                bytes.extend_from_slice(&buffer[..retained]);
                if retained < read {
                    exceeded.store(true, Ordering::Release);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) => return Err(error),
        }
    }

    Ok(BoundedRead {
        bytes,
        exceeded: exceeded.load(Ordering::Acquire),
    })
}

#[cfg(not(unix))]
fn read_bounded_cancellable<R>(
    reader: R,
    limit: usize,
    _cancelled: &AtomicBool,
    exceeded: &AtomicBool,
) -> io::Result<BoundedRead>
where
    R: Read,
{
    let read = read_bounded(reader, limit)?;
    if read.exceeded {
        exceeded.store(true, Ordering::Release);
    }
    Ok(read)
}

fn join_mutation_reader(
    reader: thread::JoinHandle<io::Result<BoundedRead>>,
) -> io::Result<BoundedRead> {
    reader
        .join()
        .map_err(|_| io::Error::other("Git output reader panicked"))?
}

fn read_bounded(mut reader: impl Read, limit: usize) -> io::Result<BoundedRead> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    let mut exceeded = false;

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }

        let remaining = limit.saturating_sub(bytes.len());
        let retained = read.min(remaining);
        bytes.extend_from_slice(&buffer[..retained]);
        exceeded |= retained < read;
    }

    Ok(BoundedRead { bytes, exceeded })
}

fn join_reader(
    operation: &'static str,
    reader: thread::JoinHandle<io::Result<BoundedRead>>,
) -> Result<BoundedRead, OrbitError> {
    reader
        .join()
        .map_err(|_| OrbitError::internal(operation, "A Git output reader stopped unexpectedly."))?
        .map_err(|error| OrbitError::internal(operation, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_missing_git_executable() {
        let runner = GitRunner::with_executable("orbit-git-does-not-exist");
        let error = runner.version().expect_err("missing executable");

        assert_eq!(error.code, "git_not_found");
    }

    #[test]
    fn bounds_process_output() {
        let runner = GitRunner::default();
        let output = runner
            .run(None, "detect_git", ["--version"], 1)
            .expect_err("version output should exceed one byte");

        assert_eq!(output.code, "git_command_failed");
    }

    #[test]
    fn identifies_git_environment_variables() {
        assert!(is_git_environment_name(OsStr::new("GIT_DIR")));
        assert!(is_git_environment_name(OsStr::new("git_work_tree")));
        assert!(is_git_environment_name(OsStr::new("GIT_CONFIG_KEY_0")));
        assert!(!is_git_environment_name(OsStr::new("PATH")));
        assert!(!is_git_environment_name(OsStr::new("ORBIT_GIT_DIR")));
    }

    #[test]
    fn restores_the_internal_no_lazy_fetch_policy_after_scrubbing() {
        let runner = GitRunner::with_executable("env")
            .with_environment("GIT_DIR", "/tmp/untrusted")
            .with_environment(NO_LAZY_FETCH_ENVIRONMENT, "0");

        let output = runner
            .run(
                None,
                "inspect_environment",
                std::iter::empty::<&str>(),
                64 * 1024,
            )
            .expect("environment process should run");
        let output = runner
            .require_success("inspect_environment", output)
            .expect("environment process should succeed");
        let environment = String::from_utf8(output.stdout).expect("environment should be UTF-8");

        assert!(environment
            .lines()
            .any(|line| line == "GIT_NO_LAZY_FETCH=1"));
        assert!(!environment.lines().any(|line| line.starts_with("GIT_DIR=")));
    }

    #[test]
    fn detects_the_secure_history_capability() {
        GitRunner::default()
            .require_no_lazy_fetch()
            .expect("development Git should support --no-lazy-fetch");
    }

    #[cfg(unix)]
    #[test]
    fn deadline_terminates_and_reaps_the_child() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("orbit-timeout-test-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).expect("create timeout fixture directory");
        let pid_file = root.join("pid");
        let executable = root.join("slow-git");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec sleep 10\n",
                pid_file.display()
            ),
        )
        .expect("write timeout fixture");
        let mut permissions = fs::metadata(&executable)
            .expect("timeout fixture metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).expect("make timeout fixture executable");

        let runner = GitRunner::with_executable(&executable);
        let started = Instant::now();
        let error = runner
            .run_with_deadline(
                None,
                "read_file_diff",
                std::iter::empty::<&str>(),
                64,
                // The fixture must be scheduled once to record its PID before
                // the deadline fires. Keep the production timeout policy out
                // of this scheduling-sensitive cleanup regression.
                Duration::from_secs(1),
            )
            .expect_err("fixture should exceed the deadline");

        assert_eq!(error.code, "git_command_timed_out");
        assert!(started.elapsed() < Duration::from_secs(3));
        let pid = fs::read_to_string(&pid_file).expect("fixture should record its PID");
        assert!(
            !Path::new("/proc").join(pid).exists(),
            "timed-out child must be reaped before return"
        );
        fs::remove_dir_all(root).expect("remove timeout fixture");
    }

    #[cfg(unix)]
    #[test]
    fn mutation_deadline_terminates_the_git_process_group_and_descendant() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "orbit-mutation-timeout-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create mutation timeout fixture");
        let parent_pid = root.join("parent-pid");
        let child_pid = root.join("child-pid");
        let executable = root.join("slow-git");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nsleep 30 &\nprintf '%s' \"$!\" > '{}'\nwait\n",
                parent_pid.display(),
                child_pid.display()
            ),
        )
        .expect("write mutation timeout fixture");
        let mut permissions = fs::metadata(&executable)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).expect("make fixture executable");

        let output = GitRunner::with_executable(&executable)
            .with_mutation_deadline(Duration::from_millis(150))
            .run_mutation(
                &root,
                "stage_file",
                std::iter::empty::<&str>(),
                64,
                Duration::from_secs(120),
                Duration::from_millis(250),
            )
            .expect("mutation process should start");

        assert_eq!(output.termination, GitMutationTermination::TimedOut);
        for pid_file in [&parent_pid, &child_pid] {
            let pid = fs::read_to_string(pid_file).expect("fixture PID");
            for _ in 0..50 {
                if !Path::new("/proc").join(pid.trim()).exists() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            assert!(
                !Path::new("/proc").join(pid.trim()).exists(),
                "ordinary mutation descendant should not remain alive"
            );
        }
        fs::remove_dir_all(root).expect("remove mutation timeout fixture");
    }

    #[cfg(unix)]
    #[test]
    fn mutation_timeout_never_removes_a_possible_git_lock() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "orbit-mutation-lock-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join(".git")).expect("create lock fixture");
        let executable = root.join("locking-git");
        fs::write(
            &executable,
            "#!/bin/sh\n: > .git/index.lock\ntrap '' TERM\nsleep 30\n",
        )
        .expect("write locking fixture");
        let mut permissions = fs::metadata(&executable)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).expect("make fixture executable");

        let output = GitRunner::with_executable(&executable)
            .with_mutation_deadline(Duration::from_millis(100))
            .run_mutation(
                &root,
                "stage_all",
                std::iter::empty::<&str>(),
                64,
                Duration::from_secs(120),
                Duration::from_millis(100),
            )
            .expect("mutation process should start");

        assert_eq!(output.termination, GitMutationTermination::TimedOut);
        assert!(
            root.join(".git/index.lock").exists(),
            "runner must not delete a lock it does not own"
        );
        fs::remove_dir_all(root).expect("remove lock fixture");
    }

    #[cfg(unix)]
    #[test]
    fn mutation_output_overflow_stops_the_process_group_with_a_bounded_result() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "orbit-mutation-output-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create output fixture");
        let executable = root.join("noisy-git");
        fs::write(
            &executable,
            "#!/bin/sh\nwhile :; do printf '0123456789abcdef'; done\n",
        )
        .expect("write noisy fixture");
        let mut permissions = fs::metadata(&executable)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).expect("make fixture executable");

        let started = Instant::now();
        let output = GitRunner::with_executable(&executable)
            .run_mutation(
                &root,
                "stage_all",
                std::iter::empty::<&str>(),
                1024,
                Duration::from_secs(10),
                Duration::from_millis(200),
            )
            .expect("mutation process should start");

        assert_eq!(output.termination, GitMutationTermination::OutputTooLarge);
        assert!(started.elapsed() < Duration::from_secs(2));
        fs::remove_dir_all(root).expect("remove output fixture");
    }

    #[cfg(unix)]
    #[test]
    fn escaped_descendant_cannot_hold_mutation_pipe_readers_open() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "orbit-mutation-escaped-reader-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create escaped-reader fixture");
        let executable = root.join("escaping-git");
        fs::write(&executable, "#!/bin/sh\nsetsid sleep 2 &\nexit 0\n")
            .expect("write escaped-reader fixture");
        let mut permissions = fs::metadata(&executable)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).expect("make fixture executable");

        let started = Instant::now();
        let output = GitRunner::with_executable(&executable)
            .run_mutation(
                &root,
                "stage_all",
                std::iter::empty::<&str>(),
                64,
                Duration::from_secs(10),
                Duration::from_millis(200),
            )
            .expect("mutation process should start");

        assert_eq!(output.termination, GitMutationTermination::Exited);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "escaped process retaining pipes must not extend Orbit's wait"
        );
        fs::remove_dir_all(root).expect("remove escaped-reader fixture");
    }

    #[cfg(unix)]
    #[test]
    fn mutation_runner_scrubs_untrusted_git_environment_and_restores_its_policy() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "orbit-mutation-environment-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create environment fixture");
        let marker = root.join("environment-ok");
        let executable = root.join("environment-git");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\n[ -z \"${{GIT_DIR+x}}\" ] || exit 20\n[ \"$GIT_NO_LAZY_FETCH\" = 1 ] || exit 21\n: > '{}'\n",
                marker.display()
            ),
        )
        .expect("write environment fixture");
        let mut permissions = fs::metadata(&executable)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).expect("make fixture executable");

        let output = GitRunner::with_executable(&executable)
            .with_environment("GIT_DIR", "/tmp/untrusted")
            .with_environment(NO_LAZY_FETCH_ENVIRONMENT, "0")
            .run_mutation(
                &root,
                "stage_file",
                std::iter::empty::<&str>(),
                64,
                Duration::from_secs(2),
                Duration::from_millis(100),
            )
            .expect("mutation process should start");

        assert_eq!(output.termination, GitMutationTermination::Exited);
        assert!(output.status.is_some_and(|status| status.success()));
        assert!(marker.exists());
        fs::remove_dir_all(root).expect("remove environment fixture");
    }

    #[cfg(unix)]
    #[test]
    fn maps_a_missing_secure_history_capability() {
        let error = GitRunner::with_executable("false")
            .require_no_lazy_fetch()
            .expect_err("false cannot support Git global options");

        assert_eq!(error.code, "git_capability_unavailable");
        assert_eq!(error.operation, "detect_git_capabilities");
    }
}
