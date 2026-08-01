# Container Daemon Integration — Best Practices

Guidance for implementing a native Linux application that controls Docker, Podman, or
containerd. Written for an implementing agent; assumes no prior context from the
conversation that produced it.

---

## 1. Core rule: speak the API, not the CLI

**Do not shell out to `docker`, `podman`, or `nerdctl` in production code paths.**

The CLI is itself a client of the daemon's API. Going direct gives you:

- No process-spawn cost per operation
- Stable typed responses instead of parsed text
- Working streams for events, logs, stats, and attach
- Real error codes instead of exit statuses

Shelling out with `--format '{{json .}}'` is acceptable only for a throwaway spike.
If it survives into the codebase, it is a bug.

**Exception:** one-shot host setup commands that are genuinely not API operations
(e.g. `containerd-rootless-setuptool.sh install`) may be invoked as subprocesses,
but only from an explicit user-triggered setup flow, never implicitly.

---

## 2. Choosing a target

| Target | Transport | Endpoint |
|---|---|---|
| Docker Engine (rootful) | HTTP/JSON over `AF_UNIX` | `/var/run/docker.sock` |
| Docker Engine (rootless) | HTTP/JSON over `AF_UNIX` | `$XDG_RUNTIME_DIR/docker.sock` |
| Podman (Docker-compatible) | HTTP/JSON over `AF_UNIX` | `$XDG_RUNTIME_DIR/podman/podman.sock` |
| Podman (libpod, richer) | HTTP/JSON over `AF_UNIX` | same socket, `/v4.0.0/libpod/...` paths |
| containerd (rootful) | gRPC | `/run/containerd/containerd.sock` |
| containerd (rootless) | gRPC | `$XDG_RUNTIME_DIR/containerd/containerd.sock` |

**Default to the Docker Engine API.** It gets you image building, networking, and
volume management for free. Only drop to containerd if the application genuinely
needs snapshot-level or namespace-level control — otherwise you will reimplement
large parts of Docker badly.

Supporting Docker *and* Podman is nearly free: probe both socket paths at startup and
use the Docker-compatible surface. Do not assume one or the other is present.

### Client libraries

| Language | Library |
|---|---|
| Go | `github.com/docker/docker/client`; `github.com/containerd/containerd/client` |
| Rust | `bollard` |
| Python | `docker` (docker-py) |
| Java | `docker-java` |
| .NET | `Docker.DotNet` |
| C / C++ | libcurl with `CURLOPT_UNIX_SOCKET_PATH`, or a generated OpenAPI client |

Before committing to a library, verify it supports **connection hijacking** (§5) and
**incremental response bodies** (§6). Libraries that fail these have to be replaced
mid-project, which is expensive.

---

## 3. Connection and configuration

### Honour the user's environment — always

Resolution order for the endpoint:

1. Explicit configuration in your own application settings
2. `DOCKER_HOST` environment variable
3. Active Docker context from `~/.docker/contexts/` (`~/.docker/config.json` names it)
4. Rootless socket at `$XDG_RUNTIME_DIR/docker.sock`
5. Rootful socket at `/var/run/docker.sock`

Never hardcode `/var/run/docker.sock` as the only option. Remote and SSH contexts are
common, and rootless users have no socket there at all.

### Negotiate the API version

Do not hardcode a version prefix like `/v1.43/`.

```
GET /_ping   →   response header: API-Version: 1.44
```

Use the lower of (your maximum supported, daemon's reported) version. Most maintained
client libraries do this automatically — confirm yours does rather than assuming.

### containerd requires a namespace on every call

Unrelated to kernel namespaces. Every gRPC call needs a `containerd-namespace` value
in the request metadata:

- `default` — nerdctl
- `moby` — Docker
- `k8s.io` — Kubernetes / CRI

Make this **user-configurable**. Hardcoding `default` will silently show an empty
container list on any mixed host.

---

## 4. Rootless mode

Assume rootless is a first-class target. It is the direction the ecosystem is moving
and the failure modes are unfamiliar to most developers.

### 4.1 Everything runs inside a RootlessKit namespace

Rootless daemons run inside their own user, mount, and network namespaces. The socket
is bind-mounted out to the host, so **connecting is trivial** — but paths and IPs
returned over that socket are valid *inside* the namespace, not on the host.

Affected: bundle directories, snapshot mountpoints, container rootfs paths, anything
under `/run/containerd/io.containerd.runtime.v2.task/`.

**If your application only issues API calls** — list, create, start, stop, pull,
subscribe to events — this does not affect you. Prefer to stay in that category.

**If you must touch container filesystems directly**, enter the namespace first:

```bash
nsenter -U --preserve-credentials -m -n \
  -t $(cat $XDG_RUNTIME_DIR/containerd-rootless/child_pid)
```

The maintainable pattern is a small helper binary (or a re-exec of your own binary
with a flag) that runs inside the namespace and communicates back over a pipe or its
own socket. Do **not** attempt to make every code path namespace-aware.

Reference implementations worth reading: `containerd-rootless.sh` and nerdctl's
`rootlessutil` package.

### 4.2 Networking

- **Container IPs are not routable from the host.** Do not attempt direct connections
  to container addresses. Everything goes through published ports.
- **Ports below 1024 fail** unless `net.ipv4.ip_unprivileged_port_start=0` is set or
  `CAP_NET_BIND_SERVICE` is granted to the rootlesskit binary. Detect this and produce
  a specific error message; the raw failure is opaque.
- **Dynamic port management** goes through the RootlessKit port API at
  `$XDG_RUNTIME_DIR/containerd-rootless/api.sock` (JSON over HTTP, `/v1/ports`).
  Only needed if you add or remove published ports on running containers.
- **Source IPs are rewritten** to the gateway address by default. Anything doing
  IP-based logging or access control inside containers will see wrong values.
- Throughput is lower than rootful bridge networking. `pasta` outperforms
  `slirp4netns`; with slirp4netns, `--mtu 65520` helps significantly.

### 4.3 Resource limits need cgroup v2 + delegation

Memory, CPU, and pid limits are only enforced with cgroup v2 **and** systemd
delegation:

```ini
# /etc/systemd/system/user@.service.d/delegate.conf
[Service]
Delegate=cpu cpuset io memory pids
```

On cgroup v1 rootless, limits do not work at all.

**Probe at startup** rather than discovering this at runtime:

```
/sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/cgroup.controllers
```

Report available controllers to the user. Silently accepting a limit that will not be
enforced is the worst possible behaviour here.

### 4.4 Snapshotters vary by kernel

- `overlayfs` — requires unprivileged overlayfs, kernel 5.11+ (or patched distro kernels)
- `fuse-overlayfs` — fallback, works more widely, slower
- `native` — full copy per layer; slow and disk-hungry

Query the plugin/snapshotter list at startup instead of assuming. **Warn the user
explicitly if `native` is active** — they will otherwise blame your application for
the disk usage and pull times.

Rootless also cannot set arbitrary file ownership inside images, so images expecting
specific UIDs may misbehave.

### 4.5 Lifecycle

Rootless daemons run as **systemd user units** under `systemd --user`.

- They terminate when the user's last session ends unless `loginctl enable-linger`
  is set. If your application is a daemon or expects containers to outlive logout,
  check for lingering and prompt the user.
- Manage the service via the **session** D-Bus (`DBUS_SESSION_BUS_ADDRESS`), not the
  system bus. No polkit interaction required.

### 4.6 Operations that will simply fail

Handle these as expected conditions with clear messages, not as bugs:

- `--privileged` and most capability additions beyond the default set
- Device passthrough (`/dev/dri`, `/dev/nvidia*`); GPU workloads need extra setup
- Mounting host paths the invoking user cannot already read
- Anything requiring `CAP_SYS_ADMIN` on the host
- `ping` inside containers, unless `net.ipv4.ping_group_range` is set
- Writes to `/sys` and most of `/proc`
- NFS and some FUSE mounts inside containers

runc surfaces these as bare `EPERM`. Mapping the common cases to human-readable
explanations is significant user-facing value and should be treated as a feature, not
an afterthought.

---

## 5. Streaming and hijacked connections

### Attach and exec hijack the connection

`POST /containers/{id}/attach` and the exec start endpoint upgrade the HTTP connection
to a raw bidirectional stream. Your HTTP library **must** expose the underlying socket
after the upgrade. Many do not. Verify this before choosing a library.

### Log framing is multiplexed

When a container has **no TTY**, stdout and stderr are interleaved on a single stream
with an 8-byte header per frame:

```
byte 0:    stream type (1 = stdout, 2 = stderr)
bytes 1-3: zero padding
bytes 4-7: payload length, big-endian uint32
```

You must demultiplex this. Writing the raw stream to a terminal produces visible
garbage between lines. When the container **does** have a TTY, the stream is raw with
no framing — branch on the container's TTY setting.

---

## 6. Streaming JSON endpoints

`/events`, image pull progress, and `/containers/{id}/stats` return newline-delimited
JSON over a chunked response that **never terminates**.

Requirements:

- The HTTP client must expose the body incrementally. Any library that buffers to
  completion will hang forever.
- Parse per line, not per document.
- Implement reconnection with backoff. The daemon restarts; your event subscription
  will drop.
- On reconnect, use the `since` parameter to avoid losing events in the gap.

Prefer subscribing to `/events` over polling. Polling `/containers/json` on a timer is
a common and avoidable design mistake.

---

## 7. Security

### Socket access is root-equivalent

Anyone who can reach a rootful `docker.sock` can start a privileged container that
mounts the host filesystem. This is a full privilege-escalation path.

- **Do not run your application as root.** Rely on `docker` group membership, or
  preferably target rootless.
- **Be explicit in the UI** about what socket access grants. Users routinely do not
  realise `docker` group membership is equivalent to root.
- Prefer rootless Docker or Podman where the application's requirements allow it. In
  rootless mode access control is just file permissions on a `0600` socket owned by
  the user, which is a genuinely better security posture.

### containerd has no authentication

None at all. Access control is entirely socket file permissions. Do not expose it over
TCP. Do not proxy it.

### Packaging

Flatpak and Snap sandbox socket access away by default. You will need an explicit
permission (`--filesystem=xdg-run/docker.sock` or equivalent). Decide early whether
those packaging models are appropriate for this application — for many container tools
they are not.

### Daemon lifecycle management

If the application needs to start or stop the daemon as a service, go through
systemd's D-Bus API with polkit for authorisation. **Do not invoke `sudo`.**

---

## 8. Startup capability probe

Perform once at startup and cache. Surface the results to the user rather than
failing later with opaque errors.

- [ ] Resolve endpoint per §3; report clearly which one was selected
- [ ] `GET /_ping` — reachability and API version negotiation
- [ ] Rootful vs rootless detection (socket path, daemon `SecurityOptions`)
- [ ] Docker vs Podman vs containerd
- [ ] Active snapshotter / storage driver; warn on `native`
- [ ] cgroup version and delegated controllers
- [ ] `net.ipv4.ip_unprivileged_port_start` if low ports are needed
- [ ] Kernel version if unprivileged overlayfs matters
- [ ] Lingering status if container persistence beyond logout is expected
- [ ] containerd namespace, if applicable

---

## 9. Consider building on nerdctl instead

If targeting **containerd in rootless mode** specifically, evaluate building against
nerdctl's Go packages rather than raw `containerd/client`.

nerdctl already solves namespace re-entry, the RootlessKit port driver, snapshotter
detection, and cgroup probing. Going direct means reimplementing a substantial portion
of RootlessKit integration.

If you do go direct, implement in this order:

1. Namespace re-entry helper (§4.1)
2. Startup capability probe (§8)
3. Everything else

Most remaining issues are clear error messages layered on those two foundations.

---

## Quick reference: rules

1. Use the API, never the CLI, in production paths
2. Honour `DOCKER_HOST` and Docker contexts
3. Negotiate the API version; never hardcode it
4. Always send a containerd namespace; make it configurable
5. In rootless, assume returned paths are namespace-local
6. In rootless, never connect directly to container IPs
7. Verify library support for hijacking and incremental bodies before committing
8. Demultiplex non-TTY log streams
9. Subscribe to events; do not poll
10. Never run as root; never `sudo`; never expose containerd over TCP
11. Probe capabilities at startup and report them
12. Translate `EPERM` into human explanations
