# IPTV media server

This project provides a small Python media server for an IPoE IPTV
deployment. It authenticates against the EPG service, refreshes channel and
programme metadata, serves M3U/XMLTV data, and proxies live HLS and
time-shift/replay streams.

The Python server remains the current operational primary while the Rust
startup path is validated on the dedicated IPTV interface. The Rust executable
now owns the complete startup composition, but live provider/DHCP execution
and supervised cutover are intentionally not claimed by the offline gates.
Keep Python running and follow the [deployment and migration runbook](#deployment-and-migration-runbook)
before any change of primary.

The repository does not ship credentials, account identifiers, device
addresses, capture files, APKs, or deployment-specific endpoints. Supply
those values at runtime for an authorized service.

## Install

```bash
uv sync
```

The public console commands are:

```text
iptv-server
iptv-logo-catalog
```

## Configure credentials and deployment values

Set these environment variables before starting the server. The values below
are placeholders and must be replaced with credentials for your own account.

```bash
export IPTV_DHCP_USER='your-dhcp-user'
export IPTV_DHCP_PASSWORD='your-dhcp-password'
export IPTV_USER_ID='your-epg-user-id'
export IPTV_PASSWORD='your-epg-password'
export IPTV_AUTH_URL='http://your-eds-host:port/EDS/jsp/AuthenticationURL'
export IPTV_STB_ID_PREFIX='your-stb-id-prefix'
export IPTV_INTERFACE='eth0'
```

Optional settings:

| Setting | Purpose |
| --- | --- |
| `IPTV_EPG_MAC` | Authorized set-top-box MAC used for EPG authentication; defaults to the selected interface MAC. |

The Python CLI reads the four credentials, `IPTV_AUTH_URL`, and `IPTV_EPG_MAC`
through its options and reads `IPTV_STB_ID_PREFIX` straight from the
environment. It doesn't read `IPTV_INTERFACE`, so the commands below pass it
with `--interface`. Supply credentials through the environment only. Option
values such as `--password` are visible to every local user in `ps` and
`/proc/<pid>/cmdline`.

The DHCP implementation uses the selected interface MAC as the Ethernet
source. It only places the configured set-top-box identity in BOOTP fields;
it does not spoof the Ethernet source address.

### Deployment credentials file

The reference deployment keeps these values in `var/iptv-e2e.env`. Git ignores
all of `var/`. Keep the file owned by the operator with mode `600`, and never
print, copy, paste, or commit it. Each non-blank line is one single-quoted
assignment such as `IPTV_INTERFACE='eth1'`. The allowed keys are
`IPTV_INTERFACE`, `IPTV_DHCP_USER`, `IPTV_DHCP_PASSWORD`, `IPTV_USER_ID`,
`IPTV_PASSWORD`, `IPTV_STB_ID_PREFIX`, `IPTV_AUTH_URL`, and the optional
`IPTV_EPG_MAC`. Don't add comments, `export`, or unquoted values, and keep `$`
and backticks out of the values. That's the format the
[opt-in real E2E gate](#opt-in-real-e2e-gate) enforces when it reads the same
file as data.

## Run the Python media server (current primary)

For a trial with HTTP bound to loopback, with the variables above exported
(it still takes over the interface, so the single-primary rule below applies):

```bash
sudo -E uv run iptv-server \
  --interface "$IPTV_INTERFACE" \
  --logo-catalog var/logo-catalog/catalog.json \
  --host 127.0.0.1 \
  --port 8080
```

The reference deployment uses a dedicated IPTV interface, `eth1` (the CLI
default is `eth0`), and listens on `0.0.0.0:8080` so players on the LAN can
reach it. The server has no authentication, so `0.0.0.0` exposes it on every
interface, including the IPTV side; limit access with a host firewall where
that matters. From the repository root, load the credentials file in a
subshell so the values never reach your interactive shell or terminal:

```bash
(
  set +x
  set -a
  . ./var/iptv-e2e.env
  set +a
  exec sudo -E uv run iptv-server \
    --interface "$IPTV_INTERFACE" \
    --logo-catalog var/logo-catalog/catalog.json \
    --host 0.0.0.0 \
    --port 8080
)
```

`set +x` stops an inherited trace from echoing the assignments. Because every
value is single-quoted, `set -a` only exports the values; nothing inside them
is expanded.

Startup is all or nothing. Python sends a raw DHCP DISCOVER and REQUEST on the
interface, applies the lease address and the two provider routes with
`ip addr replace` and `ip route replace`, logs in to the EPG service, and loads
the channel list before it accepts HTTP. Raw sockets and interface changes need
root, hence `sudo -E`. If a step fails, the process exits. Python never removes
the address or routes it set, never renews the lease, and doesn't check for
another process on the same interface; see the
[single-primary rule](#single-primary-rule-and-migration-lock).

The server exposes:

| URL | Purpose |
| --- | --- |
| `GET /healthz` | Service status: `{"status": "ok", "channels": <count>}` |
| `GET /channels.m3u` | Live-channel M3U configuration |
| `GET /xmltv.xml` | XMLTV channel and programme metadata |
| `GET /stream/<channel-id>.m3u8` | Live HLS entry point; adding `?utc=<time>` serves catch-up instead |
| `GET /replay/<channel-id>/<programme-id>.m3u8` | Replay HLS entry point |
| `GET /catchup/<channel-id>.m3u8?utc=<time>` | Catch-up HLS entry point; `start=<time>` also works |
| `GET /logos/<channel-id>` | Locally cached channel logo |
| `GET /_hls/<token>` | Internal proxy route used by rewritten playlists |
| `POST /metadata/sync` | Refresh channels, rebuild the logo catalog through the metadata converter, fetch programmes, and rewrite the guide cache |

Errors are JSON `{"error": "<message>"}` with status 503, or 502 when the HLS
proxy fails.

The service keeps upstream HLS URLs in memory and rewrites playlists through
its own short-lived proxy routes. It does not persist upstream authentication
parameters. M3U and XMLTV links are built from the request `Host` header, so
give players the address they actually use, for example
`http://<server-address>:8080/channels.m3u`. Python logs in again and reloads
channels on the first feed or playback request after 600 seconds. Programmes
only change when `POST /metadata/sync` runs; at startup the guide comes from
`var/logo-catalog/guide.json`.

Checks against the running server (the last command contacts the provider and
the third-party metadata converter):

```bash
curl -fsS http://127.0.0.1:8080/healthz
curl -fsS -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8080/channels.m3u
curl -fsS -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8080/xmltv.xml
curl -fsS -X POST http://127.0.0.1:8080/metadata/sync
```

Stop the server with Ctrl-C or SIGTERM. The address and provider routes stay
on the interface after it exits.

## Build the optional logo catalog

The catalog is generated from a live channel response and is intentionally a
runtime artifact. Do not commit it or any downloaded logos:

```bash
sudo -E uv run iptv-logo-catalog \
  --interface "$IPTV_INTERFACE" \
  --output var/logo-catalog/catalog.json
```

The builder runs the same DHCP, interface setup, and EPG login as the server,
so it counts as an interface owner. Run it only while no server owns the
interface; on a running primary, use `POST /metadata/sync` instead.

## Public-release hygiene

The repository ignores local captures, device dumps, APKs, native libraries,
logs, keystores, generated catalogs, reverse-engineering workspaces, and
runtime caches. Review the ignore rules before adding files with `git add -A`.

Only publish source and configuration templates. Never publish account
credentials, authentication tokens, signed playback URLs, DHCP captures,
device MAC addresses, private network addresses, or files copied from a
subscriber device.

## Deployment and migration runbook

### Current state

- Python is the operational primary. It owns the IPTV interface, the DHCP
  lease, the provider routes, and the EPG session, and it serves the players.
  It stays authoritative until every [cutover](#eventual-primary-cutover)
  check passes.
- Rust now has a real startup composition. It performs exact Python preflight,
  takes the instance lock and held listener, checks capabilities before DHCP,
  authenticates and refreshes metadata, serves the existing routes, and drains
  on SIGINT/SIGTERM. Offline tests do not prove live provider/DHCP success, so
  Python remains the operational primary until the supervised cutover checks
  below pass.
- The Rust production path composes configuration, capability preflight, raw
  DHCP and interface setup, the startup gate, the EPG client, scheduled
  refresh with atomic snapshots and a warm guide cache, M3U/XMLTV rendering,
  bounded HLS proxying, the route set, and graceful drain. The real provider
  surface remains an explicit, supervised validation step.
- `scripts/verify.sh` builds `rust/target/release/iptv-media-server` as a
  verification artifact. Its `--help` and `--version` are the authoritative
  CLI reference; the [configuration mapping](#configuration-mapping) records
  the Python-compatible environment and flag names.

### Single-primary rule and migration lock

Only one process may own the IPTV interface at a time. Ownership covers the
raw DHCP exchange, the interface address and provider routes, and the EPG
session. Two owners would send DHCP with the same identity, replace each
other's address and routes, and invalidate each other's login. So:

- Never run Python and Rust at the same time on the same interface or account,
  and never run two Python servers.
- Treat `iptv-logo-catalog` as an owner too (see above).
- Before another process takes the interface, stop the current primary and
  confirm it has exited.

Python has no guard of its own and will start next to another owner, so the
operator enforces the rule on the Python side.

On the Rust side, the startup gate in `rust/src/startup_gate.rs` is the
migration lock. The production `main` calls it before any interface mutation.
It runs three checks in this order
and stops at the first refusal:

1. Python preflight (`rust/src/python_scan.rs`). It reads every
   `/proc/<pid>/cmdline` and treats a process as a Python server candidate
   when an argument names the expected entry point (`iptv-server`) or
   `iptv_client.service_cli`. Any candidate refuses startup: one matching the
   expected identity (`python conflict`), one with a different interpreter,
   user, working directory, interface, bind, or cache and state paths
   (`python identity mismatch`), a launch through `uv run`, `sudo`,
   `python -m`, or another wrapper (`python wrapper mismatch`), or more than
   one candidate (`multiple python candidates`). A `/proc` field it can't read
   or decode refuses too (`proc field inaccessible`); retry if a process
   exited mid-scan. It never reads `/proc/<pid>/environ` and never signals or
   kills anything. It only sees the processes that `/proc` shows it, so with
   `/proc` mounted `hidepid=invisible`, a private PID namespace, or systemd
   `ProtectProc=invisible`, a running Python server may go unseen and the
   operator rule is the only guard.
2. Instance lock (`rust/src/lifecycle.rs`). It creates `instance.lock` in the
   startup state directory (no option selects it yet) with an atomic
   create-new, writes `{"version":1,"owner":"iptv-media-server"}`, and keeps
   the file open. An existing valid record refuses with
   `instance lock busy`, an unparsable one with `instance lock malformed`,
   another version or owner with `instance lock mismatch`, and an unusable
   directory with `instance lock inaccessible`. A normal exit removes the
   file. It's a plain file rather than a kernel lock, so a crash, abort, or
   SIGKILL leaves it behind; delete a leftover lock only after confirming that
   no `iptv-media-server` process is running. The lock stops a second Rust
   instance and does nothing about Python.
3. Port hold. It binds `--host` and `--port` and hands that same listener to
   the HTTP server, with no probe-and-release window. An address that's in
   use, such as Python still listening on `0.0.0.0:8080`, refuses with
   `port conflict` and releases the lock it just took.

Every refusal turns into the fixed startup error `service unavailable` (exit
status 1), without process names, paths, or credentials.

### Capability preflight and interface state

Raw DHCP needs `CAP_NET_RAW` for an `AF_PACKET` socket on the interface, and
interface setup needs `CAP_NET_ADMIN` for `ip addr replace` and
`ip route replace`.

- Python gets both by running as root through `sudo -E`.
- The Rust DHCP module (`rust/src/dhcp/linux.rs`) reads its effective set
  (`CapEff` in `/proc/self/status`) and requires both capabilities before it
  opens a raw socket, and again before the first `ip` command. If either is
  missing it fails with `required Linux network capability is unavailable`,
  having sent nothing and changed nothing.
- Run the future Rust service as root or as a dedicated user that holds both
  capabilities in its ambient set, for example systemd
  `AmbientCapabilities=CAP_NET_RAW CAP_NET_ADMIN` with a matching
  `CapabilityBoundingSet=`. File capabilities from `setcap` on the binary
  aren't enough, because the `ip` child processes keep capabilities across
  `exec` only through the ambient set.
- Don't grant either capability for tests or benchmarks. No gate in this
  README needs them.

Both implementations change the configured interface the same way, using the
lease from the DHCP ACK:

```text
ip addr replace <lease-address>/<prefix> dev <interface>
ip route replace <provider-prefix> via <lease-router> dev <interface> src <lease-address>
```

The route command runs once for each of the two built-in provider prefixes,
which are defined in `iptv_client/dhcp.py` and `rust/src/dhcp/linux.rs`. In
Rust, repeatable `--route CIDR[,via=IP][,src=IP]` options replace that pair. A
`replace` touches only its own entry, so other addresses and routes, including
the default route, stay as they are. Rust gives each DHCP stage and each `ip`
command 10 seconds and discards command output. Neither implementation renews
the lease or removes what it set. Python never cleans up, and Rust
deliberately keeps applied state after a startup failure past the first
change, at shutdown, and at lease expiry (`LifecycleResult::Preserved` in
`rust/src/dhcp/mod.rs`).

Baseline and failure response:

1. Before any change of owner, record the interface state and keep the output
   local, since it contains private addresses. Replace `eth1` with your IPTV
   interface:

   ```bash
   ip -br address show dev eth1
   ip route show dev eth1
   ```

2. A failure before the first change (missing capability, no DHCP reply, an
   invalid lease, or a failed address command) leaves the host untouched. Fix
   the cause and start the primary again.
3. A failure after the first change leaves the address and any routes applied
   so far in place. Don't start a second process to repair it. Compare with
   the baseline and restart the authoritative primary, which reapplies its
   own entries with `replace`. Remove by hand only what neither the baseline
   nor the new lease explains.
4. The verification gates never get this far. `scripts/verify.sh` and
   `scripts/test-mock.sh` run in-process with loopback-only mock listeners,
   the mock benchmark opens no sockets, the netns gate runs in a throwaway
   namespace, and the real E2E gate is excluded unless explicitly enabled.
   None of them needs capabilities or changes host addresses or routes unless
   the operator explicitly runs the real gate.

### Configuration mapping

| Setting | Python `iptv-server` | Rust `iptv-media-server` |
| --- | --- | --- |
| IPTV interface | `--interface` (default `eth0`; no environment variable) | `--interface` or `IPTV_INTERFACE` (default `eth0`) |
| DHCP login | `--dhcp-user` and `--dhcp-password`, or `IPTV_DHCP_USER` and `IPTV_DHCP_PASSWORD` | Same; both required and non-empty |
| EPG login | `--user-id` and `--password`, or `IPTV_USER_ID` and `IPTV_PASSWORD` | Same, plus the aliases `--epg-user` and `--epg-password`; both required |
| Authentication URL | `--authentication-url` or `IPTV_AUTH_URL` | Same |
| EPG MAC | `--epg-mac` or `IPTV_EPG_MAC`; defaults to the interface MAC | Same |
| STB ID | `--stb-id` (default empty) | Same |
| STB ID prefix | `IPTV_STB_ID_PREFIX` only | `--stb-id-prefix` or `IPTV_STB_ID_PREFIX` |
| HTTP bind | `--host` (default `127.0.0.1`) and `--port` (default `8080`) | Same; the port must be 1 to 65535 |
| Guide window | `--guide-past-days 7`, `--guide-future-days 2`, `--guide-timezone Asia/Shanghai` | Same defaults |
| Guide cache | `--guide-cache var/logo-catalog/guide.json` | Same list file plus a `<path>.meta.json` sidecar |
| Logo catalog | `--logo-catalog var/logo-catalog/catalog.json`; rebuilt by `POST /metadata/sync` | Same path; read-only and optional |
| Metadata converter | `--metadata-converter-url` | Removed; the option is rejected |
| Programme refresh | Manual `POST /metadata/sync` | `--refresh-interval-seconds` (default `3600`, must be positive); no sync route |
| Provider routes | The two built-in prefixes | The built-in prefixes, or repeatable `--route CIDR[,via=IP][,src=IP]` |
| Upstream HLS allowlist | None | Repeatable `--upstream-allow-host HOST[:PORT]` and `--upstream-allow-cidr CIDR`; the CIDR list defaults to the two provider prefixes |
| Shutdown grace | None | `--shutdown-grace-seconds` (default `60`, must be positive); the drain never waits longer than 60 seconds |

Rust exits with status 1 and `error: service unavailable` when a credential is
missing or the port, refresh interval, or shutdown grace is out of range, and
with status 2 and `error: invalid arguments` for an unknown option such as
`--metadata-converter-url` or a value that doesn't parse. Neither message
echoes values. Both implementations default the authentication URL to a
non-routable placeholder, so always set `IPTV_AUTH_URL`.

The Rust guide list keeps the Python format, which the guide cache fixtures
cover. The sidecar holds a schema version, the fetch time, the SHA-256 of the
list, and a secret-free configuration fingerprint. Rust only reuses a cache as
a warm start when the sidecar matches and the cache is at most twice
`--refresh-interval-seconds` old. A list without a matching sidecar doesn't
qualify, and that includes any list Python writes. Neither does a cache from a
different guide window, timezone, authentication origin, account, or STB
identity. Rust never deletes an invalid cache, and Python ignores the sidecar.

Rust reads an existing `catalog.json` within fixed bounds (4 MiB of catalog
JSON, 100,000 entries, 8 MiB per logo file) and serves logos with
`Cache-Control: public, max-age=86400`. It never downloads or builds logos, so
keep using `iptv-logo-catalog` or Python's `POST /metadata/sync` for that.

### Endpoint compatibility

The Rust router (`rust/src/http/router.rs`) keeps every Python player route, so
player URLs don't change at cutover:

| Route | Rust behavior |
| --- | --- |
| `GET /healthz` | Additive JSON: `status` (`starting`, `ok`, or `draining`), `ready`, `stale`, `channels`, `programmes`, `refreshed_at`, `last_refresh_error`, `failed_windows`, `lease_expires_at`. 200 while starting or ok, 503 while draining. |
| `GET /channels.m3u`, `GET /xmltv.xml` | Same feeds, rendered from one immutable snapshot for the request `Host`; 503 until the first snapshot exists |
| `GET /stream/<channel-id>.m3u8`, `/replay/<channel-id>/<programme-id>.m3u8`, `/catchup/<channel-id>.m3u8` | Same routes and query parameters (`utc`, `start`) |
| `GET /logos/<channel-id>` | Served from the local catalog only; accepts one `Range: bytes=<start>-<end>` |
| `GET /_hls/<token>` | Internal proxy route |
| `POST /metadata/sync` | Absent, so it returns 404 |

Every route answers `HEAD`, a wrong method gets 405, and an unknown path gets
404. Errors use the fixed body
`{"error": "<operation-code>", "message": "<fixed text>"}` with 503, or 502 for
HLS proxy failures, and never carry upstream URLs, tokens, credentials, or
MACs. While draining, `/healthz` keeps answering and every other route refuses
new requests with 503.

In the health body, `lease_expires_at` is the last successful refresh plus the
refresh interval. `stale` is true when the last cycle failed, published with
failed programme windows, or couldn't persist the cache, or once
`lease_expires_at` has passed. `last_refresh_error` is `refresh` or
`persistence`, and a fully successful cycle resets `failed_windows`. A failed
cycle keeps serving the last good snapshot.

The next refresh cycle starts `--refresh-interval-seconds` after the previous
one finishes, so cycles never overlap. After a failed cycle the retry delay is
120 seconds and doubles with each further consecutive failure, capped at the
refresh interval (an interval under 60 seconds is used as is).
A cycle publishes a new snapshot when at least one programme window succeeds,
carrying earlier programmes forward for the windows that failed. Rust sends no
heartbeat, keepalive, or `/log/epgplay` playback reports.

### Eventual-primary cutover

Rust becomes the primary only after every check below passes, in order. Until
then Python stays authoritative. The offline implementation check now passes;
the supervised provider/interface check remains unrun.

1. Startup: `main` wires configuration, capability preflight, the startup
   gate, DHCP, interface setup, EPG login, the first refresh, serving, and
   shutdown; it refuses before any DHCP when a Python server is visible; and
   `IPTV_ENABLE_REAL_E2E=1 bash scripts/test-real-e2e.sh` can launch the
   executable when the operator supplies the isolated credential file.
2. Offline gates: `bash scripts/verify.sh` passes in full, including both
   fixture checks, along with `bash scripts/test-mock.sh`,
   `bash scripts/test-real-e2e-selftest.sh`, and the netns gate.
3. Service definition: root or ambient `CAP_NET_RAW` and `CAP_NET_ADMIN`, an
   open-file limit of at least two descriptors per expected concurrent stream
   (each stream holds a player connection and an upstream connection), and
   supervisor rules that never start both servers, for example mutual
   `Conflicts=` between two systemd units.
4. Supervised real-surface run on the dedicated interface, with Python stopped
   and the baseline recorded: the DHCP lease and the provider routes are in
   place; `/healthz` reports `ready` with nonzero channels and programmes; M3U
   and XMLTV load in a player; live, replay, and catch-up all play; one
   scheduled refresh completes; and shutdown drains, exits, and removes
   `instance.lock`.
5. Players keep their existing URLs (same host and port) and play normally for
   an agreed observation period.

The mock benchmark is host-specific evidence, not a cutover check or an SLA.

Once checks 1 to 3 pass, the switch itself is:

1. Record the interface baseline.
2. Stop Python and confirm it's gone: `pgrep -f iptv-server` prints nothing,
   and `ss -ltn 'sport = :8080'` lists no listener.
3. Start Rust on the same interface, host, and port with the mapped
   configuration.
4. Run check 4. If anything fails, roll back.
5. Keep the Python checkout and `var/` for rollback. Removing the Python entry
   points is a separate, owner-approved release decision.

### Rollback and keep-running path

Python stays the primary for the whole rewrite. The offline gates, the netns
gate, and the mock benchmark don't touch the IPTV interface, the provider, or
port 8080, so run them while Python serves players. Only a supervised
real-surface Rust run (cutover check 4) needs Python stopped.

Roll back whenever a Rust check fails or you're unsure:

1. Stop Rust through its supervisor. The lifecycle drain refuses new requests
   and waits at most `--shutdown-grace-seconds` (never more than 60 seconds)
   for active ones; the lock is released when the server exits.
2. Confirm that no Rust process remains (`pgrep -f iptv-media-server` prints
   nothing). Only then delete a leftover `instance.lock`.
3. Compare the interface with the baseline.
4. Start Python with exactly the command it used before.
5. Check `/healthz`, `/channels.m3u`, and `/xmltv.xml`, and run
   `POST /metadata/sync` if the guide needs fresh programmes. Python reads the
   guide list and logo catalog that Rust leaves behind and ignores the
   sidecar.

## Rust local verification and release preflight

The Rust package is pinned to Rust 1.87.0 in `rust-toolchain.toml`. Prepare a
host explicitly; verification never installs tools, resolves dependencies, or
changes network state. These commands are preparation only; `cargo fetch`
downloads the locked crates that the later `--offline` runs need:

```bash
rustup toolchain install 1.87.0 --profile minimal --component rustfmt --component clippy
rustup run 1.87.0 cargo fetch --manifest-path rust/Cargo.toml --locked
uv sync --offline
cc --version
ld --version
```

Run the full local gate from any directory:

```bash
bash scripts/verify.sh
```

It refuses a missing or untracked `rust/Cargo.lock`, a missing fixture
generator or fixture directory, a missing `rustup` or `uv`, or a `rust/target/`
that isn't ignored. It then returns to the repository root and runs these gates
in order: tool version checks; the pinned rustfmt check; a locked Cargo check;
locked tests, binaries, and examples with all features
(`--tests --bins --examples --all-features`, so benchmarks aren't part of the
test run); strict all-target/all-feature clippy with warnings denied; two
offline `uv` fixture-generator checks with a stable fixture hash; and a locked
release build of `iptv-media-server`. The generator runs with `--check`, so
tracked fixtures are never rewritten. `rust/Cargo.lock` is committed and must
remain tracked.

Known limitation: the fixture generator imports PyCryptodome
(`Crypto.Cipher.DES`). When the offline Python environment doesn't provide it,
the first generator check fails with
`ModuleNotFoundError: No module named 'Crypto'` and `verify.sh` stops there.
The Rust stages before it have already run, but the second fixture check and
the release build haven't. It isn't a fixture change. Fix it during host
preparation (`uv sync --offline` from a cache that holds PyCryptodome), not
inside verification, or record the fixture checks as unverified. The
release-build stage can still run on its own:

```bash
rustup run 1.87.0 cargo --offline build --manifest-path rust/Cargo.toml --locked --release --bin iptv-media-server
```

For the Rust test suite alone, use the offline mock entrypoint:

```bash
bash scripts/test-mock.sh
```

It runs this command, and its integration tests start in-process mock provider
listeners on loopback only. It contacts no real provider, installs nothing,
and needs no DHCP capabilities:

```text
rustup run 1.87.0 cargo test --offline --manifest-path rust/Cargo.toml --locked --tests --bins --examples --all-features
```

The raw DHCP implementation pins `dhcproto` to 0.14.0 because the 0.15.0
dependency graph requires Rust 1.88. DHCP option decisions remain in the
compatibility parser: unlike `dhcproto`'s map decoder, it preserves the
Python client's first-occurrence behavior for duplicate options.

These checks cover compilation, formatting, lints, deterministic fixture
parity, and offline tests. They don't prove live DHCP, provider compatibility,
real HLS serving, capability admission, or production deployment.

Release hygiene is local and explicit: review `git status --short` and
`git diff --check` before staging; publish source, templates, the tracked
`rust/Cargo.lock`, and deterministic fixtures only. Keep `rust/target/`, local
evidence, captures, credentials, tokens, signed playback URLs, device MACs,
private addresses, and generated runtime output untracked. The verification
scripts print commands and pass/fail results, but redact command diagnostics
that contain URLs, addresses, MACs, tokens, credentials, or repository paths.

## Opt-in network namespace gate

```bash
IPTV_ENABLE_NETNS_E2E=1 bash scripts/test-netns-e2e.sh
```

An unset or empty `IPTV_ENABLE_NETNS_E2E` is `SKIP` (exit 0). Any value other
than `1` is `REFUSE` (exit 2), and so is a non-Linux host. A missing `unshare`
or `ip` is `SKIP`. When enabled, the gate creates an anonymous user and network
namespace with `unshare --user --map-root-user --net`, brings up only loopback
inside it, and runs the public-route integration test
`public_routes_cover_feeds_playback_tokens_head_405_404_and_shutdown` from
`rust/tests/media_server_integration.rs` offline with one test thread. The
namespace disappears when the command exits. It needs unprivileged user
namespaces (or root) and the fetched Cargo dependencies, but no host
capabilities. It doesn't exercise raw DHCP, interface addresses, or provider
routes, and it never touches the IPTV interface.

## Opt-in Rust mock benchmark

The repeatable mock benchmark is separate from verification and tests. It uses
`AppState`, `build_router`, the snapshot render cache, playlist rewriting, and
HLS token registration through `tower::ServiceExt::oneshot`; it opens no
sockets and contacts no provider. The workload is exactly 1,000 streams,
1,000 channels, 1,000 programmes, 5,000 bounded in-memory token resources,
and 4,000,000 logical bits per second. The 10-second warmup and 60-second
measurement window are workload metadata; the benchmark issues the exact
120,000 XMLTV requests without sleeping, so host scheduling does not add a
fixed wait to local verification.

Run it only when the opt-in is explicit:

```bash
IPTV_ENABLE_MOCK_BENCH=1 bash scripts/bench-mock.sh
```

An unset `IPTV_ENABLE_MOCK_BENCH` is `SKIP` (exit 0). Any other value,
including an empty one, is `REFUSE` (exit 2), and so is a missing `rustup`,
`python3`, or `timeout`. The wrapper runs this command under a 90-second
deadline (TERM, then KILL 5 seconds later):

```text
rustup run 1.87.0 cargo bench --offline --locked --manifest-path rust/Cargo.toml --bench media_server -- --assert --offline
```

The deadline includes compilation, so on a cold `rust/target/` build the
benchmark first:

```bash
rustup run 1.87.0 cargo bench --offline --locked --manifest-path rust/Cargo.toml --bench media_server --no-run
```

Its stdout classification is `PASS` (exit 0), `FAIL` (exit 1), `TIMEOUT`
(exit 124), or `CLEANUP_FAILURE` (exit 1). On `PASS` the benchmark's redacted
machine-readable report is published to `var/bench/mock-report.json`
atomically; any other result replaces that file with
`{"schema_version":1,"classification":"<result>"}`. The report schema is
version 1 and contains fixed workload counts, SHA-256 digests of bounded
input/output bodies, sorted integer-nanosecond p50/p95/p99/max timing values,
request/error/disconnect counts, `/proc/self/status` RSS observations, and
stable failure codes. It excludes timestamps, PIDs, paths, URLs, tokens,
credentials, and raw diagnostics. Timing and RSS thresholds are host-sensitive
and the bitrate is logical only; no physical media is generated. Read a report
as evidence about the host that produced it, not as an SLA.

## Opt-in real E2E gate

The real E2E gate is deliberately excluded from `scripts/verify.sh` and
`scripts/test-mock.sh`. It is an explicit, non-default gate for the Rust
startup path:

```bash
IPTV_ENABLE_REAL_E2E=1 bash scripts/test-real-e2e.sh
```

An unset `IPTV_ENABLE_REAL_E2E` is `SKIP` (exit 0); an empty value or anything
other than `1` is `REFUSE` (exit 2). Run it from a clean shell. It refuses when
any of `IPTV_DHCP_USER`, `IPTV_DHCP_PASSWORD`, `IPTV_USER_ID`,
`IPTV_PASSWORD`, `IPTV_EPG_MAC`, `IPTV_STB_ID_PREFIX`, or `IPTV_AUTH_URL` is
already set, because credentials may come only from `var/iptv-e2e.env`.

When enabled, the gate requires Linux; `curl`, `grep`, `mktemp`, `setsid`,
`stat`, and `timeout`; and a regular, non-symlink, owner-readable
`var/iptv-e2e.env` with
no group or world permission bits (mode `600`). The file is parsed as quoted
data, never sourced. Every non-blank line must be `KEY='value'` with an
allowlisted key. Duplicate, unknown, or malformed assignments are refused, and
so are empty, placeholder (including the sample values in this README), and
unsafe-expansion values and a missing required key (every allowlisted key
except `IPTV_EPG_MAC`).
Values are never printed. Next the gate requires an executable
`rust/target/release/iptv-media-server`. With valid opt-in credentials it may
then launch the Rust service against the configured provider, so use it only
after stopping Python and recording the interface baseline. The repository
does not run this live path by default.

An enabled run launches the Rust server through
`setsid env -i` with only the allowlisted variables, on the configured
interface and bound to `127.0.0.1`, with output in an isolated mode-`700`
temporary directory, under a fixed 300-second deadline. Cleanup sends TERM to
the process group, waits up to 10 seconds, and then sends KILL. Such a run uses
the real interface and provider, so the single-primary rule applies: stop
Python first. The script itself never calls `sudo`, `ip`, or `unshare`.

Outcomes are `SKIP` (unset opt-in), `REFUSE` (unsafe, unsupported, or
unavailable prerequisite), `FAIL` (server failure), `TIMEOUT` (the 300-second
deadline), `CLEANUP_FAILURE` (cleanup could not complete), and `PASS`. Run the
offline parser and classification checks with:

```bash
bash scripts/test-real-e2e-selftest.sh
```

The self-test copies the gate into a temporary directory with a stub
`rust/src/main.rs` and synthetic env files. It checks the `SKIP` and `REFUSE`
classifications and that no value or temporary path leaks, and it never reads
the real `var/iptv-e2e.env`.

The plan-compatible aliases are `scripts/e2e-netns.sh` and
`scripts/e2e-real.sh`. They delegate to the same gates; the real alias accepts
`IPTV_RUN_REAL_E2E=1`, while `IPTV_RUN_REAL_E2E=0` is an explicit `SKIP`.

## Migration dry run and systemd template

The migration dry run performs static checks only:

```bash
bash scripts/migration-dry-run.sh --check
bash scripts/migration-dry-run.sh --occupied-state
```

`--check` exits successfully without starting or stopping a process, changing
an interface, changing a route, or writing state. `--occupied-state` returns
`REFUSE` and exit 2 to model an existing migration lock; it never attempts a
cutover. The checked-in `systemd/iptv-media-server.service` declares
`CAP_NET_RAW` and `CAP_NET_ADMIN`, a 10-second stop grace, and a 65,536-file
limit. It remains a deployment template until a supervised real-surface run
passes; installing it is not part of offline verification.
