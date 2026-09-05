use std::{
    env,
    ffi::{OsStr, OsString},
    io::{self, Read},
    path::Path,
    process::{Command, ExitStatus, Stdio},
    thread,
};

use crate::error::OrbitError;

const STDERR_LIMIT: usize = 64 * 1024;
const NO_LAZY_FETCH_ENVIRONMENT: &str = "GIT_NO_LAZY_FETCH";

#[derive(Debug)]
pub struct GitOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone)]
pub struct GitRunner {
    executable: OsString,
    #[cfg(test)]
    environment: Vec<(OsString, OsString)>,
}

impl Default for GitRunner {
    fn default() -> Self {
        Self {
            executable: OsString::from("git"),
            #[cfg(test)]
            environment: Vec::new(),
        }
    }
}

impl GitRunner {
    #[cfg(test)]
    pub fn with_executable(executable: impl Into<OsString>) -> Self {
        Self {
            executable: executable.into(),
            environment: Vec::new(),
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
        let mut command = Command::new(&self.executable);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(test)]
        command.envs(self.environment.iter().cloned());
        scrub_git_environment(&mut command, self.test_environment_names());
        command.env(NO_LAZY_FETCH_ENVIRONMENT, "1");

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
        let status = child
            .wait()
            .map_err(|error| OrbitError::internal(operation, error.to_string()))?;
        let stdout = join_reader(operation, stdout_reader)?;
        let stderr = join_reader(operation, stderr_reader)?;

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
}
