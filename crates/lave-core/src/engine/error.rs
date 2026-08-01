//! Daemon failures, translated into something a person can act on.
//!
//! `docs/container_daemon_integration.md` §4.6 and rule 12: runc and the daemon surface
//! most refusals as a bare `EPERM` or an opaque connection failure. Mapping the common
//! cases to explanations is a feature, not an afterthought.

use std::io::ErrorKind;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    #[error("{explanation}")]
    Unreachable {
        explanation: String,
        /// What the user can do about it, when there is something.
        hint: Option<String>,
    },

    #[error("{message}")]
    Api { status: u16, message: String },

    #[error("{what} no longer exists")]
    NotFound { what: String },

    #[error("the daemon sent something unexpected: {0}")]
    Protocol(String),
}

impl EngineError {
    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        match self {
            EngineError::Unreachable { hint, .. } => hint.as_deref(),
            _ => None,
        }
    }

    /// Whether retrying later could plausibly succeed. Drives reconnection.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            EngineError::Unreachable { .. } => true,
            EngineError::Api { status, .. } => *status >= 500,
            EngineError::NotFound { .. } | EngineError::Protocol(_) => false,
        }
    }

    /// Explain a connection failure against a known socket path.
    #[must_use]
    pub fn unreachable(endpoint: &Path, kind: ErrorKind) -> Self {
        let socket = endpoint.display();
        match kind {
            ErrorKind::PermissionDenied => EngineError::Unreachable {
                explanation: format!("{socket} exists, but this user may not read it"),
                hint: Some(
                    "Socket access is root-equivalent, so it is restricted to the `docker` \
                     group. Add your user to that group and log in again, or run a rootless \
                     daemon."
                        .to_owned(),
                ),
            },
            ErrorKind::NotFound => EngineError::Unreachable {
                explanation: format!("there is no socket at {socket}"),
                hint: Some(
                    "The daemon may not be running. Start it, or point Lave Station \
                     elsewhere with --docker-host."
                        .to_owned(),
                ),
            },
            ErrorKind::ConnectionRefused => EngineError::Unreachable {
                explanation: format!("{socket} exists, but nothing is listening on it"),
                hint: Some(
                    "The daemon is probably stopped. A stale socket file is left behind \
                     when it exits uncleanly."
                        .to_owned(),
                ),
            },
            _ => EngineError::Unreachable {
                explanation: format!("could not connect to {socket}"),
                hint: None,
            },
        }
    }

    /// Translate a `bollard` failure, keeping the socket path in the message because
    /// the raw errors do not name it.
    #[must_use]
    pub fn from_bollard(error: &bollard::errors::Error, endpoint: &Path) -> Self {
        use bollard::errors::Error;

        match error {
            Error::SocketNotFoundError(path) => {
                EngineError::unreachable(Path::new(path), ErrorKind::NotFound)
            }
            Error::IOError { err } => EngineError::unreachable(endpoint, err.kind()),
            Error::DockerResponseServerError {
                status_code,
                message,
            } => match *status_code {
                404 => EngineError::NotFound {
                    what: first_line(message).to_owned(),
                },
                403 => EngineError::Unreachable {
                    explanation: format!("the daemon refused the request: {}", first_line(message)),
                    hint: Some(
                        "Rootless daemons refuse privileged operations outright; see \
                         the daemon's own logs for the specific capability."
                            .to_owned(),
                    ),
                },
                status => EngineError::Api {
                    status,
                    message: first_line(message).to_owned(),
                },
            },
            Error::JsonDataError { message, .. } => EngineError::Protocol(message.clone()),
            Error::JsonSerdeError { err } => EngineError::Protocol(err.to_string()),
            Error::HyperResponseError { err } => EngineError::Unreachable {
                explanation: format!("lost the connection to {}: {err}", endpoint.display()),
                hint: None,
            },
            Error::HyperLegacyError { err } => EngineError::Unreachable {
                explanation: format!("could not talk to {}: {err}", endpoint.display()),
                hint: None,
            },
            Error::UnsupportedURISchemeError { uri } => EngineError::Unreachable {
                explanation: format!("{uri} is not an address this version can use"),
                hint: Some("This version supports local sockets only.".to_owned()),
            },
            other => EngineError::Unreachable {
                explanation: other.to_string(),
                hint: None,
            },
        }
    }
}

/// Daemon messages are sometimes multi-line; the first line carries the meaning.
fn first_line(message: &str) -> &str {
    message.lines().next().unwrap_or(message).trim()
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn permission_denied_explains_the_docker_group() {
        let error = EngineError::unreachable(
            Path::new("/var/run/docker.sock"),
            ErrorKind::PermissionDenied,
        );

        assert!(error.to_string().contains("/var/run/docker.sock"));
        let hint = error.hint().expect("permission errors carry a hint");
        assert!(
            hint.contains("docker"),
            "hint should name the group: {hint}"
        );
        assert!(error.is_transient());
    }

    #[test]
    fn a_missing_socket_suggests_starting_the_daemon() {
        let error =
            EngineError::unreachable(Path::new("/run/user/1000/docker.sock"), ErrorKind::NotFound);

        assert!(error.to_string().contains("no socket"));
        assert!(error.hint().expect("hint").contains("not be running"));
    }

    #[test]
    fn a_refused_connection_is_distinguished_from_a_missing_one() {
        let refused = EngineError::unreachable(
            Path::new("/var/run/docker.sock"),
            ErrorKind::ConnectionRefused,
        );
        let missing =
            EngineError::unreachable(Path::new("/var/run/docker.sock"), ErrorKind::NotFound);

        assert_ne!(refused.to_string(), missing.to_string());
        assert!(refused.to_string().contains("nothing is listening"));
    }

    #[test]
    fn unmapped_kinds_still_name_the_socket_and_carry_no_false_hint() {
        let error =
            EngineError::unreachable(Path::new("/var/run/docker.sock"), ErrorKind::TimedOut);

        assert!(error.to_string().contains("/var/run/docker.sock"));
        assert_eq!(error.hint(), None);
    }

    #[test]
    fn a_404_becomes_not_found_and_is_not_retried() {
        let error = EngineError::from_bollard(
            &bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                message: "No such container: abc".to_owned(),
            },
            Path::new("/var/run/docker.sock"),
        );

        assert!(matches!(error, EngineError::NotFound { .. }));
        assert!(!error.is_transient());
    }

    #[test]
    fn server_errors_are_transient_but_client_errors_are_not() {
        let socket = Path::new("/var/run/docker.sock");
        let server = EngineError::from_bollard(
            &bollard::errors::Error::DockerResponseServerError {
                status_code: 500,
                message: "boom".to_owned(),
            },
            socket,
        );
        let client = EngineError::from_bollard(
            &bollard::errors::Error::DockerResponseServerError {
                status_code: 409,
                message: "conflict".to_owned(),
            },
            socket,
        );

        assert!(server.is_transient());
        assert!(!client.is_transient());
    }

    #[test]
    fn a_403_explains_itself_rather_than_reporting_a_bare_refusal() {
        let error = EngineError::from_bollard(
            &bollard::errors::Error::DockerResponseServerError {
                status_code: 403,
                message: "operation not permitted".to_owned(),
            },
            Path::new("/var/run/docker.sock"),
        );

        assert!(error.hint().is_some(), "403 should be explained");
    }

    #[test]
    fn a_missing_socket_error_uses_the_path_bollard_reports() {
        let error = EngineError::from_bollard(
            &bollard::errors::Error::SocketNotFoundError("/elsewhere.sock".to_owned()),
            Path::new("/var/run/docker.sock"),
        );

        assert!(error.to_string().contains("/elsewhere.sock"));
    }

    #[test]
    fn multi_line_daemon_messages_are_reduced_to_their_first_line() {
        let error = EngineError::from_bollard(
            &bollard::errors::Error::DockerResponseServerError {
                status_code: 500,
                message: "it broke\nstack trace follows\n  frame 1".to_owned(),
            },
            Path::new("/var/run/docker.sock"),
        );

        assert_eq!(error.to_string(), "it broke");
    }
}
