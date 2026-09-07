use std::io;

use serde::Serialize;

const MAX_DIAGNOSTIC_CHARS: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrbitError {
    pub code: &'static str,
    pub title: &'static str,
    pub message: String,
    pub operation: &'static str,
    pub recoverable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

impl OrbitError {
    pub fn git_spawn(operation: &'static str, error: &io::Error) -> Self {
        if error.kind() == io::ErrorKind::NotFound {
            return Self {
                code: "git_not_found",
                title: "Git is unavailable",
                message: "Orbit could not find the native Git executable. Install Git and make sure it is available on PATH.".into(),
                operation,
                recoverable: true,
                details: None,
            };
        }

        Self {
            code: "internal_error",
            title: "Could not start Git",
            message: "Orbit could not start the native Git process.".into(),
            operation,
            recoverable: true,
            details: Some(sanitize_diagnostic(&error.to_string())),
        }
    }

    pub fn not_a_repository() -> Self {
        Self {
            code: "not_a_repository",
            title: "Not a Git repository",
            message: "Choose a directory inside a Git working tree.".into(),
            operation: "open_repository",
            recoverable: true,
            details: None,
        }
    }

    pub fn repository_unavailable(message: impl Into<String>) -> Self {
        Self {
            code: "repository_unavailable",
            title: "Repository unavailable",
            message: message.into(),
            operation: "read_repository",
            recoverable: true,
            details: None,
        }
    }

    pub fn unsupported(operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            code: "unsupported_repository_state",
            title: "Repository state is not supported",
            message: message.into(),
            operation,
            recoverable: true,
            details: None,
        }
    }

    pub fn git_capability_unavailable() -> Self {
        Self {
            code: "git_capability_unavailable",
            title: "Git needs an update",
            message: "Secure commit-history loading requires a Git version that supports disabling lazy object fetching.".into(),
            operation: "detect_git_capabilities",
            recoverable: true,
            details: None,
        }
    }

    pub fn history_session_unavailable() -> Self {
        Self {
            code: "history_session_unavailable",
            title: "Commit history needs to restart",
            message: "This commit-history session is invalid, expired, or has already advanced. Refresh history to start again.".into(),
            operation: "read_commit_history",
            recoverable: true,
            details: None,
        }
    }

    pub fn invalid_history_request(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_history_request",
            title: "Commit history request is invalid",
            message: message.into(),
            operation: "read_commit_history",
            recoverable: true,
            details: None,
        }
    }

    pub fn change_set_unavailable() -> Self {
        Self {
            code: "change_set_unavailable",
            title: "Changes need to refresh",
            message: "This change list is invalid, expired, or has been replaced. Refresh the repository changes to continue.".into(),
            operation: "read_repository_changes",
            recoverable: true,
            details: None,
        }
    }

    pub fn changes_refresh_superseded() -> Self {
        Self {
            code: "changes_refresh_superseded",
            title: "A newer refresh is available",
            message: "This repository-changes request was superseded by a newer refresh.".into(),
            operation: "read_repository_changes",
            recoverable: true,
            details: None,
        }
    }

    pub fn git_failed(operation: &'static str, stderr: &[u8]) -> Self {
        let detail = String::from_utf8_lossy(stderr);
        let detail = sanitize_diagnostic(detail.trim());

        Self {
            code: "git_command_failed",
            title: "Git could not read the repository",
            message: "The native Git command failed while Orbit was reading repository state."
                .into(),
            operation,
            recoverable: true,
            details: (!detail.is_empty()).then_some(detail),
        }
    }

    pub fn output_too_large(operation: &'static str) -> Self {
        Self {
            code: "git_command_failed",
            title: "Repository response is too large",
            message: "Git returned more data than Orbit's bounded reader accepts.".into(),
            operation,
            recoverable: true,
            details: None,
        }
    }

    pub fn internal(operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            code: "internal_error",
            title: "Orbit encountered an internal error",
            message: message.into(),
            operation,
            recoverable: false,
            details: None,
        }
    }
}

fn sanitize_diagnostic(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
        .take(MAX_DIAGNOSTIC_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_missing_executable_without_exposing_a_raw_error() {
        let error = OrbitError::git_spawn(
            "detect_git",
            &io::Error::new(io::ErrorKind::NotFound, "secret executable path"),
        );

        assert_eq!(error.code, "git_not_found");
        assert_eq!(error.operation, "detect_git");
        assert_eq!(error.details, None);
    }

    #[test]
    fn sanitizes_and_bounds_git_diagnostics() {
        let mut stderr = vec![b'x'; MAX_DIAGNOSTIC_CHARS + 20];
        stderr[0] = 0x1b;

        let error = OrbitError::git_failed("read_status", &stderr);
        let details = error.details.expect("diagnostic detail");

        assert_eq!(details.chars().count(), MAX_DIAGNOSTIC_CHARS);
        assert!(!details.contains('\u{1b}'));
    }
}
