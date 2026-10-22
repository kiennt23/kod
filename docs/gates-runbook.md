# Phase 1.7 gates

`kod-gates` evaluates authoritative host records and reports seven required
GitHub checks: `inputs-current`, `requester-check`, `protected-paths`,
`tests`, `sast`, `dependency-scan`, and `secret-scan`. Scans execute the exact
PR head, or the verified merge candidate; approvals always bind to the PR
head. This branch does not change live GitHub rules or install a host.

## Build and host isolation

Build the binary from reviewed main, never from the PR being checked:

```sh
cargo build --release --locked -p kod-gates
docker build -t kod-gates --build-arg RUST_VERSION=$(sed -n 's/^channel = "\([^"]*\)".*/\1/p' rust-toolchain.toml) -f images/gates/Dockerfile .
docker image inspect kod-gates --format '{{.Id}}'
kod-gates scan-config --image <sha256-image-id> > scans.json
```

Review and adapt the inactive `gates/config.example.json` before installation.
Install the binary, policy, adapted configuration, `scans.json`, immutable run
evidence and current approval snapshots outside all PR workspaces. Protect
them with host permissions. Use a dedicated gate VM/host and local Docker
daemon, with `"labels": ["kod.role=gate"]` in its daemon configuration.
Verify the host inventory: no agent sandbox runs on this host or daemon.
The label is an enforced deployment marker, not proof of physical isolation.
Do not add that label to a development or sandbox daemon just to pass a check.

Every scan gets a fresh container, a read-only root, the source archive as
its sole read-only host mount, private disposable disk work/cache, no network, no
capabilities, no Docker socket, no credentials and bounded resources. The
wrapper copies a read-only prepared cache into its private disk volume; build
artifacts no longer consume tmpfs pages from the compiler's memory budget.
Container cleanup removes its anonymous work volume. Nothing written by a
scan survives or becomes a release/cache input. The image
contains Rust tests/clippy, cargo-deny 0.20.2 and gitleaks 8.24.3. Rust SAST
here means clippy; language-specific security rules require a reviewed
replacement scan configuration/image.

Each evaluation downloads its exact source archive once. Before scanning,
the trusted preparation entrypoint fetches that candidate's locked dependencies
and clones the current advisory DB into a private cache volume. It never runs
repository tests/build scripts and removes source-owned `.cargo` configuration
before invoking Cargo. Preparation uses the configured `package_network`
(default `kod-packages`): a dedicated Docker network with host-enforced egress
restricted to the package mirror/index and advisory source. Provision and
verify that network before deployment; merely naming a network does not
create an egress policy. Preparation has no credentials or host sockets.

The four scans remain networkless and mount prepared data read-only; dependency
changes no longer need a scanner image rebuild. The cache binds to the candidate
SHA, expires after 24 hours, and is removed before successful check publication.
Any dependency fetch, advisory refresh, expiry or cleanup error fails the gates.
Image-build caches are only seeds for preparation; the connected scan path
refuses to run without prepared data. Rebuild the tool image when its pinned
toolchain or scanner versions change. The image toolchain comes from the build
argument derived from `rust-toolchain.toml`; the wrapper has no fixed version.
The trusted deny/gitleaks configs live under `/opt/kod`, and source-owned
gitleaks config, ignore files and inline allow comments cannot suppress a
finding. Scanner output is discarded so planted credentials cannot leak
through check logs; the audit/check summary identifies the failing scan.

## Host record contracts

The host publish operation produces an immutable `Evidence` record:

```json
{
  "repo": "acme/app",
  "pr": 42,
  "head": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "run": "run-1",
  "story": "PAY-123",
  "team": "core",
  "pins": ["brief-rev-3", "design-rev-2", "api-rev-4"],
  "policy_version": "<Policy::version() recorded by this run>",
  "runner_config_version": "<immutable runner configuration version>"
}
```

Do not derive this record from a PR description, branch metadata, a sandbox
artifact or a file contributed by the PR. A human PR needs the same trusted
story/input binding. `runner_config_version` identifies the original runner;
the actual gate config, scanner commands/image and policy versions are also
hashed and recorded by the gate itself.

The read-only `HostRecords` bridge re-reads a trusted JSON snapshot on each
lookup. Its producer is responsible for consuming tracker and approved-doc
events and resolving all required inputs, including requirements, design
with threat model and API contracts. For example:

```json
{
  "story": "PAY-123",
  "requester": "product-owner",
  "required": ["brief-link", "design-link", "api-link"],
  "current": {
    "brief-link": "brief-rev-3",
    "design-link": "design-rev-2",
    "api-link": "api-rev-4"
  }
}
```

Unknown revisions fail. An empty required set fails. New or superseded
required inputs fail until the run's pins match exactly. The CLI bridge is
not a real tracker adapter: the library also accepts the `Tracker` contract
directly, and 6c/6b must connect and accept the real tracker/approval source.

## Invocation and event ordering

Use a dedicated GitHub App installation token with repository contents and
pull requests and metadata read, and checks write. Reviewer permissions are
queried from GitHub; read-only outsider approvals cannot satisfy separation.
The agent and scanner never receive the token.

```sh
kod-gates run \
  --config /etc/kod/gates/config.json \
  --scans /etc/kod/gates/scans.json \
  --policy /etc/kod/policy \
  --evidence /var/lib/kod/gates/run-1.json \
  --records /var/lib/kod/gates/PAY-123.json \
  --audit /var/lib/kod/audit.db \
  --docker /usr/bin/docker
```

Set `KOD_GITHUB_TOKEN` through the host's credential service; never put it in
arguments or a PR workflow. Exit zero means every check passed; any failed
check or infrastructure error exits nonzero. Before any evaluation the
runner starts a new pending generation of all checks. Completion updates
those same check runs, after audit intent. An audit outage prevents updates.

The trusted event ingester must verify delivery authentication and schedule
reevaluation for PR creation/new commits, review submission/dismissal, base
updates and artifact approvals. It uses the same exclusive file lock as the
CLI: `<audit-path>.gates.lock`. Atomic snapshot replacement and evaluation
are serialized under that lock. On artifact approval, first call
`Gates::invalidate` for every affected PR and live merge candidate; only
after those pending updates are durable may the new approval become current.
Then replace snapshots and reevaluate. Persist deliveries until completion;
a crash leaves checks pending and retries the event. A missing or unhealthy
ingester must stop queue admission; it must never leave a connected pilot
running on unattended successful checks.

The merge queue uses one PR per group. On an authenticated `merge_group`
event, bind that group's candidate SHA to its constituent PR's current head
in a trusted file and pass `--candidate <file>`:

```json
{"pr":42,"head":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
```

Never accept this binding from a PR comment or an unsigned request. The
runner rechecks the PR/reviews/story after scans and checks artifact revisions
again before reporting successes. The merge queue candidate receives all
seven checks, with current inputs and PR-head approvals evaluated again.
This is the connected event-ingester contract for 6b; this sub-plan's
acceptance exercises it with fake tracker/forge records.

## GitHub activation

Generate a ruleset using the dedicated App's integration ID:

```sh
kod-gates ruleset --app-id <app-id> > ruleset.json
```

The generated ruleset is disabled by default. Review it with
`gates/repository-settings.json`, `gates/README.md` and `.github/CODEOWNERS`.
The ruleset pins every check to that App, forbids bypass actors and force
push/deletion, requires CODEOWNERS, resolved threads, stale approval
dismissal and approval of the most recent push, and requires a merge queue.
The separate repository setting disables auto-merge.

An administrator may install the disabled ruleset for inspection:

```sh
gh api --method POST repos/<owner>/<repo>/rulesets --input ruleset.json
gh api --method PATCH repos/<owner>/<repo> --input gates/repository-settings.json
```

Do not set its enforcement to active until the gate host, event ingester,
package egress policy and independent reviewer topology pass connected
acceptance. After activation, verify the active rules through GitHub's API
and exercise a real queue candidate before enabling delegation. Merge queue availability depends on
repository ownership and GitHub plan; if unavailable, do not remove the
merge-time freshness requirement to make activation pass. In particular,
verify support before applying these rules to the personal `kiennt23/kod`
repository. Live activation is not claimed by the offline acceptance suite.

`gates/config.example.json` protects every path because this repo is the harness.
The existing policy needs the owner and backup once named; protected changes
also need two distinct current human approvers, including the configured
CODEOWNER. The PR author and bot reviews do not count. This single-owner
example cannot pass an owner-authored PR: the owner is required by policy but
cannot self-approve. A second reviewer alone does not resolve that conflict.
Keep the profile inactive until reviewers are appointed and the owner decides
the policy for owner-authored changes. This PR does not relax self-approval or
allow bypasses. For product
repos, keep the trusted owners in sync with CODEOWNERS and configure protected
prefixes for embedded tests and any repository-specific sensitive code.

## Bypass audit and compliance

No routine bypass is configured. If an administrator changes rules or uses
an exceptional organization-level bypass, the trusted GitHub audit/event
ingester records each observation before advancing its durable cursor:

```sh
kod-gates record-bypass --audit /var/lib/kod/audit.db \
  --repo acme/app --delivery <verified-delivery-id> --actor <login> \
  --sha <full-commit-sha> --ruleset <ruleset-id>
```

Repeated events may produce repeated audit entries with the same delivery
ID; no event may disappear. Restrict this command to the trusted ingester.
Checkpoint and verify the audit log as in the audit runbook. Evidence export
destination, retention and access remain compliance-profile decisions; this
change neither deletes evidence nor selects an export destination.

## Acceptance

```sh
cargo test -p kod-gates
KOD_DOCKER_TESTS=1 KOD_GATE_TEST_IMAGE=<image-digest> \
  KOD_DOCKER_BIN=/usr/bin/docker cargo test -p kod-gates
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

The real-container tests call the isolated execution layer on the developer
daemon, without labeling it as a production gate host. They prove fresh
workspaces, no inherited token/socket, read-only host data, failure and timeout
cleanup, and real scanner rejection of bad tests, lint errors, prohibited
dependency licenses and planted secrets. The normal production path separately
requires the gate-only daemon marker. Connected activation still requires
verified events, a real App token, enforced rules and host-inventory evidence.
