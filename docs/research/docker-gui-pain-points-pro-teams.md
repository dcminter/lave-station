# Docker & Docker Compose Pain Points — Professional Team Edition

Research input for a native Linux Docker GUI, this version oriented toward professional development teams. The audience is assumed to be fluent in Docker: they know what an image is, they understand namespaces and layers, and they don't need the container/image relationship explained. What they struggle with is different — operational friction at scale, configuration sprawl across environments, dev/prod parity, per-seat licensing, and the gap between what the daemon knows and what any current tool surfaces. This document catalogs those problems, split between (1) friction inherent to Docker and Compose themselves and (2) gaps specific to the commercial Docker Desktop product. It describes problems only; solution design is left to the implementer.

Sources include Docker's own documentation, the `docker/roadmap` and `docker/desktop-linux` issue trackers, Docker community forums, and practitioner engineering write-ups; links are inline where claims trace to specific sources.

---

## Part 1: Friction inherent to Docker and Compose

Experienced users don't hold wrong mental models — they hold correct ones and still lose time, because the information needed to act on that model is scattered across `inspect`, `events`, `system df`, log files, and Compose's merge machinery. Each item below is a place where correct knowledge still requires tedious manual correlation.

### 1.1 Compose configuration sprawl: base files, overrides, profiles, and the opacity of the merged result

The canonical team setup — a base `compose.yaml` plus `compose.override.yaml` (auto-loaded), plus per-environment files layered with `-f`, plus profiles for optional services — works, but the merge rules grow hard to reason about as configuration accumulates, a limitation Docker's own documentation concedes ([Docker docs](https://docs.docker.com/compose/how-tos/multiple-compose-files/)). The failure mode is well described in team-oriented write-ups: files start clean, accrete environment-specific and commented-out blocks, and eventually "every edit feels risky" because no single file is authoritative — only the merged result is, and nothing renders that result for you except manually running `docker compose config` ([EnvManager](https://envmanager.com/blog/docker-compose-multiple-files)).

Concretely, teams struggle to answer: which file contributed this value? What does the effective configuration look like with *this* combination of `-f` flags, `--profile` selections, `COMPOSE_FILE`/`COMPOSE_PROFILES` environment variables, and `.env` interpolation? Which services will actually start given the active profiles ([OneUptime](https://oneuptime.com/blog/post/2026-01-25-docker-compose-profiles/view))? Path resolution adds a trap of its own: relative paths in merged files resolve against the first file's directory, which regularly breaks monorepo layouts. Monorepos amplify everything — multiple teams maintaining overlapping stacks, each needing a reference Compose file from the others, with Docker's docs acknowledging that "complexity moves from the code into the infrastructure and the configuration file."

### 1.2 Environment variable precedence and the three injection mechanisms

Professionals know the difference between `.env` (YAML interpolation), `env_file:` (container runtime injection), and `environment:` (inline) — and still get bitten, because the *precedence chain* across shell environment, `.env`, `--env-file`, `env_file:`, and `environment:` is long, partially documented, and version-sensitive ([env.dev](https://env.dev/guides/docker-compose-env-variables)). The recurring team-scale symptom is configuration that behaves differently in CI than locally because CI's shell exports or working directory differ, and the only diagnostic is again `docker compose config` plus `docker exec ... env` archaeology. The Compose V1→V2 transition and the 2025 jump to Compose v5 changed flag behavior at the margins, so long-lived team scripts encode assumptions that silently rot.

### 1.3 `depends_on` vs. readiness, and startup orchestration generally

Even teams that know `depends_on` only orders startup still spend real effort maintaining healthcheck blocks, `condition: service_healthy` wiring, and app-side retry logic — and diagnosing the crash-loops that appear when someone adds a service without them ([reponotes](https://reponotes.com/blog/docker-compose-up-what-it-does-flags-troubleshooting/)). Related: understanding *why* Compose chose to recreate (or not recreate) a given container on `up` requires knowing its config-hash diffing behavior; "stale container with old env" after a Compose file edit remains a routine time sink even for experts, as do orphan-container warnings after service renames.

### 1.4 Project identity, multi-project collisions, and port real estate

Teams run many stacks concurrently. Compose project naming (directory name by default, overridable via `-p` / `COMPOSE_PROJECT_NAME`) causes collisions when two repos share a directory name and duplicate stacks when one repo is run from two paths. Host port allocation across a dozen simultaneously running projects is unmanaged by anything: "port is already allocated" requires manually correlating `docker ps` output with host `ss`/`lsof`, and teams end up maintaining informal port-assignment conventions in wikis. Networks and volumes prefixed by project name accumulate across branches and experiments with no tooling that groups, ages, or attributes them.

### 1.5 Bind-mount ownership on Linux — still unsolved, just better understood

Professionals understand *why* UID/GID mismatches happen; that doesn't make the remediation less tedious. Teams standardize on one of several imperfect patterns — `user: "${UID}:${GID}"` in Compose, entrypoint chown scripts, build-time user matching, ACLs, userns-remap, or rootless Docker — each with tradeoffs, and the choice varies by project ([Dash0](https://www.dash0.com/faq/how-to-manage-permissions-for-docker-shared-volumes), [Medium](https://medium.com/@Modexa/7-docker-volume-ownership-fixes-uid-gid-the-python-way-23b59e703a83)). Two aggravators matter for teams specifically. First, SELinux (`:z`/`:Z`) and AppArmor create distro-dependent failures, so a Compose file that works on a Fedora laptop fails on Ubuntu CI. Second — and this is the parity trap — Docker Desktop's VM layer on Mac/Windows silently reconciles ownership, so mixed-platform teams ship Compose files that work for the Mac contingent and break for Linux users and in production ([Easton](https://eastondev.com/blog/en/posts/dev/20251217-docker-mount-permissions-guide/)). Diagnosis still bottoms out in comparing numeric IDs by hand across host and container.

### 1.6 Immutable container configuration and reconstruction risk

The destroy-and-recreate requirement for changing ports, env, mounts, or networks ([Dash0](https://www.dash0.com/faq/change-port-mapping-existing-docker-container)) is a known cost for professionals, but the risk profile differs from the hobbyist case: the danger is *incomplete reconstruction* — recreating a long-lived container while dropping a label, a network alias, a logging option, or a restart policy that was set months ago and lives only in `docker inspect` output. For Compose-managed services this is handled; for the long tail of ad-hoc containers (databases with test data, one-off tools), it's manual `inspect`-to-flags translation with no verification.

### 1.7 Disk economics: build cache, image sprawl, and prune semantics at team scale

On active dev machines and self-hosted CI runners, Docker's storage grows into the 20–50 GB+ range with build cache typically the largest and least-attributed consumer ([Dash0](https://www.dash0.com/faq/how-to-clean-up-docker-disk-space)). The prune command family's semantics (dangling vs. unused images; `system prune` not touching volumes or tagged images by default; `builder prune` being separate; `--volumes` being destructive; per-builder caches under buildx requiring `--builder` targeting per [Docker docs](https://docs.docker.com/engine/manage-resources/pruning/)) are known to professionals but remain operationally awkward: there is no good way to answer "what exactly will this reclaim, and what does each candidate belong to?" before pulling the trigger, and no attribution of cache/image bloat to projects or branches. Teams either over-prune (losing morning build speed) or under-prune (losing disks).

### 1.8 Logging is under-configured by default and hard to correlate

The default unbounded `json-file` driver fills disks unless every service carries `max-size`/`max-file` options — boilerplate that teams must remember per-service. Beyond rotation, the real professional pain is correlation: tailing and searching across the 5–15 services of a Compose stack, aligning timestamps around a failure, connecting an exit code (137 OOM-kill, 143 SIGTERM) to the daemon event and resource state that caused it. `docker compose logs` interleaves but doesn't search, filter by level, or persist; `docker events` is a separate raw stream nobody watches. Teams routinely bolt on Loki/Dozzle-class tooling for what is fundamentally local-dev log viewing.

### 1.9 Build performance and the buildx/multi-arch tax

Mixed-architecture teams (Apple Silicon laptops + amd64 CI + ARM production on Graviton) now face multi-platform builds as a routine concern, and the toolchain around it is genuinely painful: QEMU-emulated cross-builds that turn 10–20 minute builds into an hour ([Depot](https://depot.dev/blog/speed-up-docker-builds)), per-architecture layer caches that silently overwrite each other in registry cache configurations, `exec format error` failures from missing binfmt registration or host-arch binaries copied into target-arch stages, and cache keys sensitive to BuildKit versions ([DockerBuild.com](https://dockerbuild.com/tutorials/multi-arch-builds), [FixDevs](https://fixdevs.com/blog/docker-multi-platform-build-not-working/)). Even single-arch, local build cache behavior — what invalidated a layer, why a rebuild took 4 minutes instead of 10 seconds — is invisible without deliberate `--progress=plain` spelunking, and cache-efficient Dockerfile structure remains tribal knowledge enforced only in code review.

### 1.10 Local/CI parity and environment drift

The recurring team complaint underneath many of the above: the same Compose file behaves differently across a developer's Linux workstation, a teammate's Mac running Desktop's VM, and CI — due to ownership handling (§1.5), `host.docker.internal` availability (native on Desktop, manual `host-gateway` mapping on Linux Engine — [OneUptime](https://oneuptime.com/blog/post/2025-12-16-nginx-docker-localhost-host/)), `--network=host` meaning the VM rather than the host on Desktop, working-directory-relative `.env` pickup, and shell-environment leakage into interpolation. Onboarding a new developer onto a multi-service stack — the "works in under an hour, not two weeks" problem — is a widely-cited team goal precisely because drift makes it hard ([DEV Community](https://dev.to/teguh_coding/docker-compose-for-local-development-the-setup-that-makes-your-team-hate-excuses-3mjg)).

### 1.11 Secrets hygiene

Compose's local-dev story for secrets is weak: `secrets:` support exists but is awkward outside Swarm, so teams fall back to `.env` files that leak into repos, get shared over Slack, and drift per-developer. Auditing which containers received which sensitive values, and keeping secrets out of `docker inspect` output and image layers, is left entirely to team discipline.

### 1.12 Multiple daemons, contexts, and remote hosts

Professional workflows increasingly span daemons: local Engine, a shared dev server over SSH, staging boxes, rootless installs. `docker context` handles switching, but the active context is ambient global state with minimal visibility — commands quietly target the wrong daemon, and nothing provides a consolidated view of containers/images/disk across contexts. This is exactly the multi-environment capability Portainer sells to teams and that CLI users script around.

---

## Part 2: Docker Desktop pain points for professional teams

### 2.1 Licensing: the forcing function

For companies over 250 employees or $10M revenue, Docker Desktop requires a paid subscription — currently roughly $9 (Pro) / $15 (Team) / $24 (Business) per user per month ([Empiric Apps](https://www.empiricapps.com/zenithal/docker-desktop-license-cost), [Qovery](https://www.qovery.com/blog/4-best-docker-desktop-alternatives)). The 2021 licensing change is the single largest driver of the alternatives ecosystem (Podman Desktop, Rancher Desktop, Colima, OrbStack), and it created ongoing compliance overhead: subscriptions are tracked per named user via Docker Hub org membership, Docker can audit, and license-management guidance recommends monthly reconciliation and buffer seats ([USU](https://www.usu.com/en/blog/quick-guide-to-docker-licensing)). Critically for positioning: the license attaches to the *Desktop application*, not to Docker Engine — native Engine on Linux is free at any company size. A native Linux GUI that drives the stock Engine sits entirely outside the licensing perimeter, which for a team on Linux workstations converts directly into removed per-seat cost and removed compliance bookkeeping. Note also that migration-cost inertia is the main argument for staying ([USU](https://www.usu.com/en/blog/quick-guide-to-docker-licensing)) — meaning a replacement's switching cost matters as much as its features.

### 2.2 Feature gating within paid tiers

Beyond the license threshold itself, capabilities professionals actually want are tier-gated: synchronized file shares (the performant bind-mount mechanism) requires Pro/Team/Business ([Docker blog](https://www.docker.com/blog/announcing-synchronized-file-shares/)), and hardened-desktop features like Enhanced Container Isolation are Business-tier. Teams paying for Team tier still hit upsell boundaries, which compounds the resentment visible in community sentiment.

### 2.3 The VM architecture on Linux, and what it costs a team

Docker Desktop for Linux runs the engine inside a QEMU VM with VirtioFS file sharing, in a separate `desktop-linux` context disjoint from any native Engine ([Docker Linux FAQ](https://docs.docker.com/desktop/troubleshoot-and-support/faqs/linuxfaqs/)). For professional teams the costs are concrete: per-seat RAM/CPU reservation on every workstation; file-sharing overhead that Docker itself only claims is "near native" with tuning; dependency on correct `subuid`/`subgid` host configuration; a context split that routinely misdirects CLI commands and CI scripts; `--network=host` semantics that differ from native Engine; and — the parity issue from §1.5/§1.10 — mount-ownership behavior inside the VM that diverges from the production Linux hosts the team deploys to. The daemon's lifetime is also coupled to a GUI application rather than systemd, so a Desktop crash takes the whole container environment with it; the `desktop-linux` tracker documents startup hangs on common distros ([docker/desktop-linux #272](https://github.com/docker/desktop-linux/issues/272)), and Docker's own release notes list Resource Saver mode making `docker compose up` unresponsive as a known issue.

### 2.4 No Compose lifecycle management in the GUI

Desktop can only display stacks already started from a terminal: no launching a stack from a `compose.yaml`, no per-service `--build`/`--force-recreate` controls, no profile or override-file selection, no editing. Open on Docker's roadmap since 2020 ([docker/roadmap #71](https://github.com/docker/roadmap/issues/71)) with continuing forum requests ([Docker forums, 2025](https://forums.docker.com/t/feature-request-add-docker-compose-file-creation-and-management-to-docker-desktop-gui/150161)). For teams this matters beyond convenience: the Compose file *is* the team's shared source of truth, and a GUI that can't operate on it can't participate in the team's actual workflow — which is why Dockge (file-based, editor + lifecycle + real-time output), Portainer stacks, and similar tools are what teams actually deploy. Given §1.1, note that for professional users "Compose support" implicitly means multi-file, profile-aware, interpolation-aware support — handling only a single `compose.yaml` reproduces the gap at a higher level.

### 2.5 Read-only container configuration

Desktop's inspect views expose ports, env, and mounts without any modify path — a capability its predecessor Kitematic had and users have requested back since 2020 ([Docker forums](https://forums.docker.com/t/how-to-change-container-settings-in-the-dashboard/96862)). Combined with §1.6, the professionally valuable operation is *safe guided recreation with full config fidelity* — the thing Portainer implements and Desktop doesn't attempt.

### 2.6 No network, context, or multi-host management

No UI for creating networks, attaching/detaching containers, or visualizing topology (the gap the PortNavigator extension exists to fill — [GitHub](https://github.com/oslabs-beta/port-navigator)); no management of `docker context` or remote daemons, which for teams with shared dev servers is a daily need. Volume tooling remains inspection-oriented, with weak size attribution and no safe-deletion workflow.

### 2.7 Trust, telemetry, and surface area

Recurring professional complaints: telemetry that users report persisting despite opt-outs, extensions and AI/Hub features enabled by default, login nags, and update churn introducing regressions ([DEV Community](https://dev.to/volker_schukai/do-you-need-docker-desktop-for-linux-17ja/comments)). For teams this reads as governance risk, not just annoyance — an IT department evaluating what runs on every engineer's machine weighs data flows and attack surface. Desktop's officially supported Linux distro list is also short (specific Ubuntu/Debian/Fedora releases), leaving teams standardized on other distros unsupported.

---

## Divergence from the solo-developer edition, and rough priorities

Compared to the hobbyist version of this document, the center of gravity moves: conceptual-model issues drop out entirely, and three clusters rise to the top. First, **Compose as team infrastructure** — multi-file merge opacity, profiles, env precedence, project identity, and the absence of any GUI that operates on Compose files as the shared source of truth (§1.1–1.4, §2.4). Second, **parity and drift** — Linux-native mount ownership, Desktop-VM behavioral differences, CI divergence, onboarding cost (§1.5, §1.10, §2.3). Third, **the licensing wedge** — Desktop's per-seat cost and compliance overhead versus a free native Engine, which is the strongest adoption argument a native Linux GUI has with this audience (§2.1–2.2). Behind those: disk/build-cache economics and observability (§1.7–1.9), guided recreation with config fidelity (§1.6, §2.5), and multi-daemon/context awareness (§1.12, §2.6). Multi-arch build pain (§1.9) is real but partially out of scope for a local GUI; it's included because build-cache visibility and builder management are within reach even where CI is not.
