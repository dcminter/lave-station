# Docker & Docker Compose Pain Points

Research input for a native Linux Docker GUI aimed at solo developers and hobbyists. This document catalogs the operations people demonstrably struggle with, split into two parts: (1) things that are unintuitive about Docker and Compose themselves, regardless of tooling, and (2) gaps and frustrations specific to the commercial Docker Desktop application. It deliberately describes problems, not solutions — feature design is left to the implementer.

Sources are drawn from Docker's own documentation and blog, the Docker community forums, GitHub issues on `docker/roadmap` and `docker/desktop-linux`, and a wide spread of practitioner troubleshooting guides. Links are included inline where a claim traces to a specific source.

---

## Part 1: Things that are unintuitive about Docker itself

These are conceptual and operational stumbling blocks that hit almost everyone. A GUI can't change how Docker works, but each of these represents a place where users currently hold a wrong mental model, lose data, or resort to Stack Overflow — which makes them the highest-value places for a UI to surface state that the CLI keeps invisible.

### 1.1 The image / container / Dockerfile relationship

The single most common beginner confusion is what an image actually *is*. Users understand "I write a Dockerfile" and "I run a container," but the read-only, layered image sitting between the two is opaque ([TestDriven.io](https://testdriven.io/blog/docker-for-beginners/)). Practitioner write-ups repeatedly describe the same failure modes: running an image that was never built, editing a Dockerfile and not understanding why the running container didn't change, and not grasping that many containers can be spawned from one image. The "image is a recipe, container is a cake" analogy appears in nearly every beginner guide precisely because the CLI does nothing to make the relationship visible.

Related sub-confusions: image layers and what invalidates the build cache; the ambiguity of the `latest` tag (it's just a tag, not "newest," and it silently changes underneath you); and the fact that `docker run` will auto-pull a missing image but `docker build` is a separate, manual step.

### 1.2 Ephemerality and where data actually lives

Users modify a running container (install a package, tweak a config, write data), delete or recreate the container, and lose everything. That containers are disposable by design and persistence must be opted into via volumes is consistently cited as the concept that "trips up almost everyone" ([Python in Plain English](https://python.plainenglish.io/10-essential-docker-concepts-i-wish-someone-explained-to-me-on-day-one-d554837a31bd)). The `--rm` flag deleting everything on exit catches beginners by surprise, as does accidental volume deletion ([Medium](https://medium.com/@laurarx1997/first-day-learning-docker-this-is-what-claude-summarized-for-me-4c7106d98822)).

The three storage mechanisms — bind mounts, named volumes, anonymous volumes — are poorly distinguished in users' minds. Anonymous volumes in particular accumulate silently, are hard to associate back to the container that created them, and hold data users don't know exists until a prune deletes it (or fails to free the space they expected).

### 1.3 Bind-mount file ownership (the Linux-specific killer)

On Linux, containers share the host kernel's numeric UID/GID space with no reconciliation. A container running as root writes root-owned files into your bind-mounted project directory; a container running as a non-root user can't write to your directory at all. The result is the classic "permission denied" loop: files you can't edit without `sudo`, containers that crash on startup because they can't write to a mount ([Dash0](https://www.dash0.com/faq/how-to-manage-permissions-for-docker-shared-volumes), [linuxvox](https://linuxvox.com/blog/understanding-user-file-ownership-in-docker-how-to-avoid-changing-permissions-of-linked-volumes/)).

This is worse for your target audience than for Mac/Windows users, because Docker Desktop's VM layer quietly papers over ownership mismatches on those platforms — Linux users get the raw kernel behavior ([Easton](https://eastondev.com/blog/en/posts/dev/20251217-docker-mount-permissions-guide/)). SELinux (Fedora/RHEL, `:z`/`:Z` labels) and AppArmor (Ubuntu) add a second, even more opaque layer of permission failures on exactly the distros a Linux GUI will run on. Diagnosing any of this requires knowing to compare `id` output on the host against `ls -n` on the mount — knowledge users acquire only after hours of frustration.

### 1.4 Networking: localhost isn't localhost

Docker networking generates a disproportionate share of "why can't I connect" questions, nearly all reducible to a few unintuitive facts:

`localhost` inside a container is the container, not the host. Users run a database on the host, try to reach it from a container at `127.0.0.1`, and hang ([Easton](https://eastondev.com/blog/en/posts/dev/20251217-docker-host-access/)). The escape hatch, `host.docker.internal`, works out of the box on Docker Desktop but **not** on native Linux Docker Engine, where it requires a manual `--add-host=host.docker.internal:host-gateway` — a platform inconsistency that breaks copy-pasted Compose files constantly ([OneUptime](https://oneuptime.com/blog/post/2025-12-16-nginx-docker-localhost-host/)). Conversely, host services bound only to `127.0.0.1` are unreachable from containers even with the right address.

`EXPOSE` doesn't publish anything — it's documentation. Actual port publishing happens with `-p` at runtime, and the distinction between "exposed" and "published" ports costs people hours ([Python in Plain English](https://python.plainenglish.io/10-essential-docker-concepts-i-wish-someone-explained-to-me-on-day-one-d554837a31bd)).

Container-to-container connectivity depends on shared networks and on the fact that Compose service names become DNS hostnames — elegant once known, mysterious before. The default bridge network behaves differently (no DNS by name) than user-defined networks, and "port already allocated" errors require correlating host port usage across containers and host processes manually.

### 1.5 Container configuration is immutable

You cannot change the port mappings, environment variables, mounts, network mode, or most other settings of an existing container — not even a stopped one. The only supported path is destroy-and-recreate with the new flags, preserving volumes and carefully reconstructing every other option from `docker inspect` ([Dash0](https://www.dash0.com/faq/change-port-mapping-existing-docker-container), [HostZealot](https://www.hostzealot.com/blog/about-servers/docker-ports-configuration-and-usage)). This violates every intuition users bring from configuring normal applications, and doing the recreation correctly by hand (getting all the original flags right) is genuinely error-prone. It is also, notably, an operation where third-party GUIs like Portainer differentiate themselves by automating the stop → remove → recreate → start dance.

### 1.6 Disk usage is invisible and cleanup semantics are confusing

Docker never cleans up after itself: stopped containers, unused images, anonymous volumes, and build cache accumulate to tens of gigabytes before anyone notices, and on Linux it all hides in `/var/lib/docker` where `df` output doesn't obviously explain where the space went ([Dash0](https://www.dash0.com/faq/how-to-clean-up-docker-disk-space), [openillumi](https://openillumi.com/en/en-docker-system-prune-disk-space/)).

The cleanup commands are a minefield of subtle semantics. `docker system prune` removes *dangling* images only, not unused tagged ones — the most common misconception about it. `-a` extends to all unused images; `--volumes` extends to anonymous volumes and can permanently destroy data; `docker builder prune` is a separate command for build cache, which is often the largest and least-known consumer. The dangling-vs-unused distinction, the `RECLAIMABLE` column of `docker system df`, and which prune touches what are all things users learn only after either running out of disk or deleting something they needed.

### 1.7 Compose: `depends_on` doesn't mean "ready"

`depends_on` controls start *order*, not readiness. Apps that boot faster than their database see connection refused and crash-loop, and the fix (healthchecks plus `condition: service_healthy`, or app-side retries) is non-obvious ([reponotes](https://reponotes.com/blog/docker-compose-up-what-it-does-flags-troubleshooting/)). This is probably the single most common Compose gotcha.

### 1.8 Compose: three environment-variable mechanisms that people conflate

Compose has `.env` (interpolation into the YAML itself), `env_file:` (variables injected into the container), and `environment:` (inline overrides) — three mechanisms with different scopes that users chronically mix up. "Variable is not set, defaulting to blank string" warnings, values present in the file but absent at runtime, and confusion over precedence are all recurring symptoms ([env.dev](https://env.dev/guides/docker-compose-env-variables), [reponotes](https://reponotes.com/blog/docker-compose-up-what-it-does-flags-troubleshooting/)). Working directory matters too: `.env` is only picked up from where Compose runs.

### 1.9 Compose: "why didn't my change apply?"

`docker compose up` follows an order of operations (load config → resolve images → create infrastructure → recreate changed containers → start) that users don't know, so stale code, stale images, and un-applied edits are constant complaints. `up` doesn't rebuild images unless told to (`--build`); it doesn't recreate containers unless it detects config drift; volumes keep old data across recreations. The related cluster of confusion covers `up` vs `start`, `down` vs `stop` (with `down` deleting networks and, with `-v`, volumes), orphan-container warnings, and project naming (two directories with the same name colliding, or the same file run from different directories producing duplicate stacks).

### 1.10 Compose: YAML and versioning traps

YAML itself supplies a layer of gotchas: unquoted port mappings like `80:80` being parsed as base-60 numbers, tab characters producing cryptic parse errors, and indentation mistakes ([DevBolt](https://www.devbolt.dev/blog/fix-docker-compose-errors)). On top of that sits versioning confusion: `docker-compose` (V1, Python, hyphenated) vs `docker compose` (V2, plugin) still breaks old tutorials and scripts, and the obsolete top-level `version:` field generates warnings that alarm users who copied it from dated guides ([hivebook](https://hivebook.wiki/wiki/docker-compose-v3-gotchas)).

### 1.11 Logs and debugging are an afterthought for users

Beginners forget container logs exist and guess at failures instead of reading `docker logs` ([Jeevi Academy](https://www.jeeviacademy.com/beginner-mistakes-in-docker-and-how-to-avoid-them/)). When they do find logs, they hit follow-on problems: the default `json-file` driver grows without bound and eats disk unless size limits are configured; exit codes (137 = OOM-killed, 143 = SIGTERM) are cryptic; and understanding *why* a container died means correlating `docker inspect`, logs, and events by hand. `docker exec -it <container> sh` as the standard "get inside and look" move is not discoverable.

### 1.12 CLI ergonomics generally

`docker run` invocations grow into multi-line flag soup that users save in shell history or scripts because there's no other record of how a container was started. The 2017 CLI reorganization means two parallel command vocabularies exist (`docker ps` vs `docker container ls`), with official material inconsistently using both ([TestDriven.io](https://testdriven.io/blog/docker-for-beginners/)). Converting a working `docker run` command into a Compose service is a common enough need that dedicated tools (composerize, Dockge's built-in converter) exist for it.

---

## Part 2: Pain points specific to Docker Desktop

Docker Desktop's Linux edition has structural problems on Linux specifically, plus UI gaps shared across all platforms. The existence and popularity of Portainer, Dockge, lazydocker, and Podman Desktop is itself evidence of the demand these gaps create.

### 2.1 On Linux, Docker Desktop runs everything inside a VM

This is the headline issue for your project. Docker Desktop for Linux does not use the host's Docker Engine — it spins up a QEMU virtual machine and runs the engine, containers, and images inside it, with VirtioFS bridging file access ([Docker's Linux FAQ](https://docs.docker.com/desktop/troubleshoot-and-support/faqs/linuxfaqs/)). Docker justifies this as feature parity and security, but for Linux users the consequences are all cost:

The VM reserves memory and CPU that must be managed with resource sliders, on an OS that could run containers natively at zero overhead. File sharing between host and containers goes through virtiofsd, with performance that Docker itself describes as only "near native" with the right tuning, and which depends on host `subuid`/`subgid` configuration being correct. Docker Desktop installs a separate `desktop-linux` context alongside any existing Engine's `default` context, so images, containers, and volumes exist in two disjoint worlds — a notorious source of "where did my containers go" confusion when the active context silently differs from what the user expects. And bind-mount ownership behaves differently inside the VM than on native Engine, so containers developed under Desktop can break when deployed to a real Linux host.

Community reception of Desktop on Linux reflects this: long-time Linux Docker users widely regard it as a resource-hungry VM wrapper around something their OS already does, with recurring reports of stability problems ([DEV Community discussion](https://dev.to/volker_schukai/do-you-need-docker-desktop-for-linux-17ja/comments)). A native GUI that talks to the host's own dockerd via the socket sidesteps this entire category — worth stating explicitly in the document your implementer reads, because it's the core differentiation.

### 2.2 You can't launch or manage a Compose stack from the GUI

Docker Desktop displays Compose stacks only *after* they've been started from the CLI. There is no way to point the GUI at a `compose.yml` and bring the stack up, no way to create or edit a Compose file in the app, and no per-service rebuild/recreate controls beyond basic start/stop of what already exists. This has been an open community request on Docker's roadmap since 2020 ([docker/roadmap #71](https://github.com/docker/roadmap/issues/71) — users resort to distributing shell scripts so non-CLI colleagues can start stacks) and continues to generate forum feature requests ([Docker forums, 2025](https://forums.docker.com/t/feature-request-add-docker-compose-file-creation-and-management-to-docker-desktop-gui/150161)).

This is the gap that Dockge exists to fill: a stack-oriented manager offering create/edit/start/stop of `compose.yaml`, real-time pull/up/down progress, an interactive editor, and `docker run` → Compose conversion — while keeping the files as plain files on disk rather than trapping them in an internal database. The same gap motivated Spin ("the most heavily utilized Docker GUI lacks Docker Compose functionality") and is an open feature request against Podman Desktop too ([podman-desktop #15708](https://github.com/podman-desktop/podman-desktop/issues/15708)). For solo devs and self-hosters — who live in Compose files — this is arguably the most-wanted single capability.

### 2.3 Container settings are view-only

Docker Desktop shows ports, environment variables, and mounts in its inspect view but offers no way to change any of them — a regression users have complained about since Kitematic (Docker's earlier GUI) was retired, since Kitematic *did* let you edit ports, env vars, and volumes via a settings pane ([Docker forums](https://forums.docker.com/t/how-to-change-container-settings-in-the-dashboard/96862)). Because of §1.5, "editing" really means guided recreation — Portainer implements exactly this (stop → remove → recreate with modified config → start), and its existence proves users want the operation wrapped in a UI rather than performed as CLI archaeology.

### 2.4 No real network or volume management

The UI provides no meaningful way to create networks, connect or disconnect containers from networks, or visualize which containers share which networks — a gap large enough that PortNavigator was built as a Desktop *extension* just to add network visualization and management ([PortNavigator](https://github.com/oslabs-beta/port-navigator)). Volume support is browse-oriented (and content browsing was historically paywalled before being made free); associating anonymous volumes with their containers, inspecting sizes, and safely identifying deletable volumes remain weak.

### 2.5 Bloat, telemetry, upsell, and trust

A consistent thread in community sentiment: Desktop grows features (extensions enabled by default, Kubernetes, AI assistant, Docker Hub integration, login prompts) that many local-dev users experience as clutter, alongside telemetry that users report being difficult to genuinely disable ([DEV Community](https://dev.to/volker_schukai/do-you-need-docker-desktop-for-linux-17ja/comments)). Several capabilities are gated behind paid subscriptions — synchronized file shares, the higher-performance bind-mount mechanism, requires Pro/Team/Business ([Docker blog](https://www.docker.com/blog/announcing-synchronized-file-shares/)) — and the license itself requires payment for use in larger companies, which pushed many teams off Desktop entirely and seeded the ecosystem of alternatives. Solo devs aren't hit by licensing, but they inherit the nagging and the perception problem.

### 2.6 Reliability and platform fit on Linux

Desktop for Linux carries known operational rough edges: getting stuck on the "Starting" screen after install on some distros ([docker/desktop-linux #272](https://github.com/docker/desktop-linux/issues/272)); `docker compose up` becoming unresponsive while the app is in Resource Saver mode (a documented known issue in Docker's own release notes); UI state occasionally disagreeing with the engine (e.g., port mappings shown by `docker ps` but missing from the UI, per forum reports); and distro support that is officially limited to a short list of Ubuntu/Debian/Fedora versions, leaving Arch, Mint, and other common hobbyist distros in unsupported territory. The daemon-in-a-VM design also means the whole container environment dies when the Desktop app misbehaves — unlike native Engine, where dockerd runs as a systemd service independent of any GUI.

---

## Quick reference: the pain points ranked by likely impact for solo Linux devs

For prioritization purposes, a rough ordering of the above by frequency-times-severity for the target audience: (1) Compose stack lifecycle not manageable from a GUI (§2.2), (2) bind-mount permissions on Linux (§1.3), (3) disk usage opacity and prune semantics (§1.6), (4) immutable container config / guided recreation (§1.5, §2.3), (5) networking mental model, especially container↔host on native Linux (§1.4), (6) `depends_on` vs readiness and the "why didn't my change apply" Compose cluster (§1.7, §1.9), (7) env-var mechanism confusion (§1.8), (8) volume/ephemerality visibility (§1.2), (9) logs and failure diagnosis (§1.11), (10) image/container model clarity for newcomers (§1.1). Docker Desktop's VM architecture (§2.1) isn't a feature to build so much as the structural weakness a native GUI automatically avoids — but it deserves prominence in how the project positions itself.
