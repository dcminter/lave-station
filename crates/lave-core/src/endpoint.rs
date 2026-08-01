//! Resolving which container daemon to talk to.
//!
//! Order is fixed by `docs/container_daemon_integration.md` §3: command line, then
//! `DOCKER_HOST`, then the active Docker context, then the rootless socket, then the
//! rootful socket. A Podman socket is probed last.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A daemon address this version knows how to talk to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Unix(PathBuf),
}

impl Endpoint {
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Endpoint::Unix(path) => path,
        }
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Endpoint::Unix(path) => write!(f, "unix://{}", path.display()),
        }
    }
}

/// How the endpoint was chosen. Shown to the user so a surprising target is explicable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointSource {
    CommandLine,
    DockerHostEnv,
    DockerContext(String),
    RootlessSocket,
    RootfulSocket,
    PodmanSocket,
}

impl std::fmt::Display for EndpointSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EndpointSource::CommandLine => f.write_str("--docker-host option"),
            EndpointSource::DockerHostEnv => f.write_str("DOCKER_HOST environment variable"),
            EndpointSource::DockerContext(name) => write!(f, "Docker context \"{name}\""),
            EndpointSource::RootlessSocket => f.write_str("rootless socket"),
            EndpointSource::RootfulSocket => f.write_str("rootful socket"),
            EndpointSource::PodmanSocket => f.write_str("Podman socket"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub endpoint: Endpoint,
    pub source: EndpointSource,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("{origin} names a {scheme}:// endpoint; this version supports local sockets only")]
    UnsupportedScheme {
        scheme: String,
        raw: String,
        origin: String,
    },

    #[error("{origin} is not a usable daemon address: \"{raw}\"")]
    Malformed { raw: String, origin: String },

    #[error("Docker context \"{name}\" {reason}")]
    ContextUnusable { name: String, reason: String },

    #[error("no container daemon socket found; looked in {}", .probed.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))]
    NoDaemonFound { probed: Vec<PathBuf> },
}

/// Environment variable lookup, injected so resolution is testable.
pub trait EnvSource {
    fn var(&self, key: &str) -> Option<String>;
}

/// Filesystem inspection, injected so resolution is testable.
pub trait PathProbe {
    fn exists(&self, path: &Path) -> bool;
    fn read_to_string(&self, path: &Path) -> Option<String>;
    fn list_dir(&self, path: &Path) -> Vec<PathBuf>;
}

pub struct SystemEnv;

impl EnvSource for SystemEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

pub struct SystemPaths;

impl PathProbe for SystemPaths {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn read_to_string(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn list_dir(&self, path: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        entries.flatten().map(|entry| entry.path()).collect()
    }
}

/// Pick the daemon to talk to, reporting how the choice was made.
///
/// # Errors
///
/// If an address is malformed or names an unsupported scheme, if the selected Docker
/// context is missing or unusable, or if no socket was found anywhere.
pub fn resolve(
    cli_host: Option<&str>,
    env: &dyn EnvSource,
    paths: &dyn PathProbe,
) -> Result<Resolved, ResolveError> {
    if let Some(raw) = cli_host {
        let endpoint = parse_host(raw, "the --docker-host option")?;
        return Ok(Resolved {
            endpoint,
            source: EndpointSource::CommandLine,
        });
    }

    if let Some(raw) = non_empty(env.var("DOCKER_HOST")) {
        let endpoint = parse_host(&raw, "DOCKER_HOST")?;
        return Ok(Resolved {
            endpoint,
            source: EndpointSource::DockerHostEnv,
        });
    }

    if let Some(resolved) = resolve_context(env, paths)? {
        return Ok(resolved);
    }

    probe_sockets(env, paths)
}

/// The active context, if one is selected and is not the built-in `default`.
fn resolve_context(
    env: &dyn EnvSource,
    paths: &dyn PathProbe,
) -> Result<Option<Resolved>, ResolveError> {
    let Some(home) = non_empty(env.var("HOME")).map(PathBuf::from) else {
        return Ok(None);
    };
    let docker_dir = home.join(".docker");

    let name = if let Some(name) = non_empty(env.var("DOCKER_CONTEXT")) {
        name
    } else {
        let config_path = docker_dir.join("config.json");
        let Some(text) = paths.read_to_string(&config_path) else {
            return Ok(None);
        };
        let config: DockerConfig =
            serde_json::from_str(&text).map_err(|error| ResolveError::ContextUnusable {
                name: config_path.display().to_string(),
                reason: format!("could not be parsed: {error}"),
            })?;
        let Some(name) = config
            .current_context
            .and_then(|name| non_empty(Some(name)))
        else {
            return Ok(None);
        };
        name
    };

    // "default" is the built-in context, which means "use the usual sockets".
    if name == "default" {
        return Ok(None);
    }

    let meta_root = docker_dir.join("contexts").join("meta");
    let host = context_host(&name, &meta_root, paths)?;
    let endpoint = parse_host(&host, &format!("Docker context \"{name}\""))?;
    Ok(Some(Resolved {
        endpoint,
        source: EndpointSource::DockerContext(name),
    }))
}

/// Docker names context directories by a hash of the context name, so match on the
/// recorded name instead of recomputing it.
fn context_host(
    name: &str,
    meta_root: &Path,
    paths: &dyn PathProbe,
) -> Result<String, ResolveError> {
    for dir in paths.list_dir(meta_root) {
        let Some(text) = paths.read_to_string(&dir.join("meta.json")) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<ContextMeta>(&text) else {
            continue;
        };
        if meta.name != name {
            continue;
        }
        return meta
            .endpoints
            .get("docker")
            .and_then(|endpoint| endpoint.host.clone())
            .and_then(|host| non_empty(Some(host)))
            .ok_or_else(|| ResolveError::ContextUnusable {
                name: name.to_owned(),
                reason: "does not define a Docker endpoint".to_owned(),
            });
    }

    Err(ResolveError::ContextUnusable {
        name: name.to_owned(),
        reason: "is selected but was not found".to_owned(),
    })
}

fn probe_sockets(env: &dyn EnvSource, paths: &dyn PathProbe) -> Result<Resolved, ResolveError> {
    let runtime_dir = non_empty(env.var("XDG_RUNTIME_DIR")).map(PathBuf::from);

    let mut candidates: Vec<(PathBuf, EndpointSource)> = Vec::new();
    if let Some(dir) = runtime_dir.as_ref() {
        candidates.push((dir.join("docker.sock"), EndpointSource::RootlessSocket));
    }
    candidates.push((
        PathBuf::from("/var/run/docker.sock"),
        EndpointSource::RootfulSocket,
    ));
    if let Some(dir) = runtime_dir.as_ref() {
        candidates.push((
            dir.join("podman").join("podman.sock"),
            EndpointSource::PodmanSocket,
        ));
    }

    for (path, source) in &candidates {
        if paths.exists(path) {
            return Ok(Resolved {
                endpoint: Endpoint::Unix(path.clone()),
                source: source.clone(),
            });
        }
    }

    Err(ResolveError::NoDaemonFound {
        probed: candidates.into_iter().map(|(path, _)| path).collect(),
    })
}

fn parse_host(raw: &str, origin: &str) -> Result<Endpoint, ResolveError> {
    let trimmed = raw.trim();

    if let Some(path) = trimmed.strip_prefix("unix://") {
        return unix_endpoint(path, trimmed, origin);
    }
    if trimmed.starts_with('/') {
        return unix_endpoint(trimmed, trimmed, origin);
    }
    if let Some((scheme, _)) = trimmed.split_once("://") {
        return Err(ResolveError::UnsupportedScheme {
            scheme: scheme.to_owned(),
            raw: trimmed.to_owned(),
            origin: origin.to_owned(),
        });
    }

    Err(ResolveError::Malformed {
        raw: trimmed.to_owned(),
        origin: origin.to_owned(),
    })
}

fn unix_endpoint(path: &str, raw: &str, origin: &str) -> Result<Endpoint, ResolveError> {
    if path.is_empty() {
        return Err(ResolveError::Malformed {
            raw: raw.to_owned(),
            origin: origin.to_owned(),
        });
    }
    Ok(Endpoint::Unix(PathBuf::from(path)))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.trim().is_empty())
}

#[derive(Deserialize)]
struct DockerConfig {
    #[serde(rename = "currentContext")]
    current_context: Option<String>,
}

#[derive(Deserialize)]
struct ContextMeta {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Endpoints", default)]
    endpoints: BTreeMap<String, ContextEndpoint>,
}

#[derive(Deserialize)]
struct ContextEndpoint {
    #[serde(rename = "Host")]
    host: Option<String>,
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeEnv(HashMap<String, String>);

    impl FakeEnv {
        fn with(mut self, key: &str, value: &str) -> Self {
            self.0.insert(key.to_owned(), value.to_owned());
            self
        }
    }

    impl EnvSource for FakeEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    #[derive(Default)]
    struct FakePaths {
        files: HashMap<PathBuf, String>,
        sockets: Vec<PathBuf>,
    }

    impl FakePaths {
        fn with_file(mut self, path: &str, contents: &str) -> Self {
            self.files.insert(PathBuf::from(path), contents.to_owned());
            self
        }

        fn with_socket(mut self, path: &str) -> Self {
            self.sockets.push(PathBuf::from(path));
            self
        }
    }

    impl PathProbe for FakePaths {
        fn exists(&self, path: &Path) -> bool {
            self.sockets.iter().any(|socket| socket == path) || self.files.contains_key(path)
        }

        fn read_to_string(&self, path: &Path) -> Option<String> {
            self.files.get(path).cloned()
        }

        fn list_dir(&self, path: &Path) -> Vec<PathBuf> {
            let mut dirs: Vec<PathBuf> = self
                .files
                .keys()
                .filter_map(|file| file.parent())
                .filter(|parent| parent.parent() == Some(path))
                .map(Path::to_path_buf)
                .collect();
            dirs.sort();
            dirs.dedup();
            dirs
        }
    }

    fn context_meta(name: &str, host: &str) -> String {
        format!(r#"{{"Name":"{name}","Endpoints":{{"docker":{{"Host":"{host}"}}}}}}"#)
    }

    #[test]
    fn command_line_wins_over_everything() {
        let env = FakeEnv::default().with("DOCKER_HOST", "unix:///from/env.sock");
        let paths = FakePaths::default().with_socket("/var/run/docker.sock");

        let resolved = resolve(Some("unix:///from/cli.sock"), &env, &paths).expect("resolves");

        assert_eq!(
            resolved.endpoint,
            Endpoint::Unix(PathBuf::from("/from/cli.sock"))
        );
        assert_eq!(resolved.source, EndpointSource::CommandLine);
    }

    #[test]
    fn docker_host_wins_over_context_and_sockets() {
        let env = FakeEnv::default()
            .with("DOCKER_HOST", "unix:///from/env.sock")
            .with("HOME", "/home/dev");
        let paths = FakePaths::default()
            .with_file(
                "/home/dev/.docker/config.json",
                r#"{"currentContext":"remote"}"#,
            )
            .with_socket("/var/run/docker.sock");

        let resolved = resolve(None, &env, &paths).expect("resolves");

        assert_eq!(resolved.source, EndpointSource::DockerHostEnv);
        assert_eq!(
            resolved.endpoint,
            Endpoint::Unix(PathBuf::from("/from/env.sock"))
        );
    }

    #[test]
    fn active_context_wins_over_sockets() {
        let env = FakeEnv::default().with("HOME", "/home/dev");
        let paths = FakePaths::default()
            .with_file(
                "/home/dev/.docker/config.json",
                r#"{"currentContext":"work"}"#,
            )
            .with_file(
                "/home/dev/.docker/contexts/meta/aa11/meta.json",
                &context_meta("other", "unix:///wrong.sock"),
            )
            .with_file(
                "/home/dev/.docker/contexts/meta/bb22/meta.json",
                &context_meta("work", "unix:///work.sock"),
            )
            .with_socket("/var/run/docker.sock");

        let resolved = resolve(None, &env, &paths).expect("resolves");

        assert_eq!(
            resolved.source,
            EndpointSource::DockerContext("work".to_owned())
        );
        assert_eq!(
            resolved.endpoint,
            Endpoint::Unix(PathBuf::from("/work.sock"))
        );
    }

    #[test]
    fn docker_context_env_overrides_the_config_file() {
        let env = FakeEnv::default()
            .with("HOME", "/home/dev")
            .with("DOCKER_CONTEXT", "work");
        let paths = FakePaths::default()
            .with_file(
                "/home/dev/.docker/config.json",
                r#"{"currentContext":"idle"}"#,
            )
            .with_file(
                "/home/dev/.docker/contexts/meta/bb22/meta.json",
                &context_meta("work", "unix:///work.sock"),
            );

        let resolved = resolve(None, &env, &paths).expect("resolves");

        assert_eq!(
            resolved.source,
            EndpointSource::DockerContext("work".to_owned())
        );
    }

    #[test]
    fn the_default_context_falls_through_to_sockets() {
        let env = FakeEnv::default().with("HOME", "/home/dev");
        let paths = FakePaths::default()
            .with_file(
                "/home/dev/.docker/config.json",
                r#"{"currentContext":"default"}"#,
            )
            .with_socket("/var/run/docker.sock");

        let resolved = resolve(None, &env, &paths).expect("resolves");

        assert_eq!(resolved.source, EndpointSource::RootfulSocket);
    }

    #[test]
    fn a_missing_config_file_falls_through_to_sockets() {
        let env = FakeEnv::default().with("HOME", "/home/dev");
        let paths = FakePaths::default().with_socket("/var/run/docker.sock");

        let resolved = resolve(None, &env, &paths).expect("resolves");

        assert_eq!(resolved.source, EndpointSource::RootfulSocket);
    }

    #[test]
    fn the_rootless_socket_is_preferred_over_the_rootful_one() {
        let env = FakeEnv::default().with("XDG_RUNTIME_DIR", "/run/user/1000");
        let paths = FakePaths::default()
            .with_socket("/run/user/1000/docker.sock")
            .with_socket("/var/run/docker.sock");

        let resolved = resolve(None, &env, &paths).expect("resolves");

        assert_eq!(resolved.source, EndpointSource::RootlessSocket);
        assert_eq!(
            resolved.endpoint,
            Endpoint::Unix(PathBuf::from("/run/user/1000/docker.sock"))
        );
    }

    #[test]
    fn podman_is_probed_last() {
        let env = FakeEnv::default().with("XDG_RUNTIME_DIR", "/run/user/1000");
        let paths = FakePaths::default().with_socket("/run/user/1000/podman/podman.sock");

        let resolved = resolve(None, &env, &paths).expect("resolves");

        assert_eq!(resolved.source, EndpointSource::PodmanSocket);
    }

    #[test]
    fn no_socket_anywhere_reports_what_was_probed() {
        let env = FakeEnv::default().with("XDG_RUNTIME_DIR", "/run/user/1000");
        let paths = FakePaths::default();

        let error = resolve(None, &env, &paths).expect_err("nothing to find");

        let ResolveError::NoDaemonFound { probed } = error else {
            panic!("expected NoDaemonFound, got {error:?}");
        };
        assert_eq!(
            probed,
            vec![
                PathBuf::from("/run/user/1000/docker.sock"),
                PathBuf::from("/var/run/docker.sock"),
                PathBuf::from("/run/user/1000/podman/podman.sock"),
            ]
        );
    }

    #[test]
    fn remote_schemes_are_rejected_with_the_scheme_named() {
        for scheme in ["tcp", "ssh", "http", "https", "npipe"] {
            let raw = format!("{scheme}://elsewhere:2375");
            let error = resolve(Some(&raw), &FakeEnv::default(), &FakePaths::default())
                .expect_err("remote schemes are unsupported");

            let ResolveError::UnsupportedScheme {
                scheme: reported, ..
            } = error
            else {
                panic!("expected UnsupportedScheme, got {error:?}");
            };
            assert_eq!(reported, scheme);
        }
    }

    #[test]
    fn a_bare_absolute_path_is_accepted() {
        let resolved = resolve(
            Some("/var/run/docker.sock"),
            &FakeEnv::default(),
            &FakePaths::default(),
        )
        .expect("resolves");

        assert_eq!(
            resolved.endpoint,
            Endpoint::Unix(PathBuf::from("/var/run/docker.sock"))
        );
    }

    #[test]
    fn nonsense_addresses_are_rejected() {
        for raw in ["", "   ", "unix://", "not-an-address"] {
            let error = resolve(Some(raw), &FakeEnv::default(), &FakePaths::default())
                .expect_err("nonsense is rejected");
            assert!(
                matches!(error, ResolveError::Malformed { .. }),
                "expected Malformed for {raw:?}, got {error:?}"
            );
        }
    }

    #[test]
    fn an_empty_docker_host_is_ignored() {
        let env = FakeEnv::default().with("DOCKER_HOST", "");
        let paths = FakePaths::default().with_socket("/var/run/docker.sock");

        let resolved = resolve(None, &env, &paths).expect("resolves");

        assert_eq!(resolved.source, EndpointSource::RootfulSocket);
    }

    #[test]
    fn a_selected_but_missing_context_is_an_error() {
        let env = FakeEnv::default().with("HOME", "/home/dev");
        let paths = FakePaths::default()
            .with_file(
                "/home/dev/.docker/config.json",
                r#"{"currentContext":"gone"}"#,
            )
            .with_socket("/var/run/docker.sock");

        let error = resolve(None, &env, &paths).expect_err("missing context is an error");

        assert!(
            matches!(error, ResolveError::ContextUnusable { ref name, .. } if name == "gone"),
            "got {error:?}"
        );
    }

    #[test]
    fn a_malformed_config_file_is_an_error_rather_than_a_silent_fallback() {
        let env = FakeEnv::default().with("HOME", "/home/dev");
        let paths = FakePaths::default()
            .with_file("/home/dev/.docker/config.json", "{ not json")
            .with_socket("/var/run/docker.sock");

        let error = resolve(None, &env, &paths).expect_err("malformed config is an error");

        assert!(
            matches!(error, ResolveError::ContextUnusable { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn a_malformed_meta_file_does_not_hide_a_later_match() {
        let env = FakeEnv::default().with("HOME", "/home/dev");
        let paths = FakePaths::default()
            .with_file(
                "/home/dev/.docker/config.json",
                r#"{"currentContext":"work"}"#,
            )
            .with_file(
                "/home/dev/.docker/contexts/meta/aa11/meta.json",
                "{ not json",
            )
            .with_file(
                "/home/dev/.docker/contexts/meta/bb22/meta.json",
                &context_meta("work", "unix:///work.sock"),
            );

        let resolved = resolve(None, &env, &paths).expect("resolves past the bad file");

        assert_eq!(
            resolved.endpoint,
            Endpoint::Unix(PathBuf::from("/work.sock"))
        );
    }

    #[test]
    fn a_context_without_a_docker_endpoint_is_an_error() {
        let env = FakeEnv::default().with("HOME", "/home/dev");
        let paths = FakePaths::default()
            .with_file(
                "/home/dev/.docker/config.json",
                r#"{"currentContext":"work"}"#,
            )
            .with_file(
                "/home/dev/.docker/contexts/meta/bb22/meta.json",
                r#"{"Name":"work","Endpoints":{}}"#,
            );

        let error = resolve(None, &env, &paths).expect_err("unusable context");

        assert!(
            matches!(error, ResolveError::ContextUnusable { ref reason, .. }
                if reason.contains("does not define")),
            "got {error:?}"
        );
    }

    #[test]
    fn a_remote_context_names_the_context_in_the_error() {
        let env = FakeEnv::default().with("HOME", "/home/dev");
        let paths = FakePaths::default()
            .with_file(
                "/home/dev/.docker/config.json",
                r#"{"currentContext":"cloud"}"#,
            )
            .with_file(
                "/home/dev/.docker/contexts/meta/cc33/meta.json",
                &context_meta("cloud", "ssh://user@host"),
            );

        let error = resolve(None, &env, &paths).expect_err("ssh is unsupported");

        assert!(
            error.to_string().contains("cloud"),
            "error should name the context: {error}"
        );
    }

    #[test]
    fn endpoints_render_as_urls() {
        let endpoint = Endpoint::Unix(PathBuf::from("/var/run/docker.sock"));
        assert_eq!(endpoint.to_string(), "unix:///var/run/docker.sock");
        assert_eq!(endpoint.path(), Path::new("/var/run/docker.sock"));
    }
}
