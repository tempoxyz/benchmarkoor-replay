# benchmarkoor-replay

`benchmarkoor-replay` is a standalone CLI for replaying benchmarkoor newline-delimited JSON-RPC
Engine API fixture files against a bare Reth node. It is built for local baremetal runs with
schelk-managed datadir recovery.

The local workflow does not require Docker, Podman, ZFS, S3, GitHub Actions, or a TUI.

## Build

```sh
cargo build
```

For a shell-visible binary:

```sh
cargo install --path .
```

## Suites

First-class suites:

- `perf-devnet-3/24358000`
- `jochemnet/24402727`

The selected suite carries network, block, genesis, snapshot, fixture, release, fork, context,
and test-type metadata. `benchmarkoor-tests` YAML is read from `--metadata-root` when present.
By default, cache data lives under the platform cache directory as `benchmarkoor-replay`.
An existing `benchreplay` cache is reused automatically when the new cache does not exist.

## Status

```sh
benchmarkoor-replay status --datadir /schelk/reth
```

Status reports the fixture cache, suite identity, schelk runtime state, baseline marker, Reth
processes, resource profile, and hazards such as unwritable drop caches or inconsistent schelk
state.

## Fixtures

Index an existing local fixture tree:

```sh
benchmarkoor-replay fixtures download \
  --url /home/ubuntu/projects/amsterdam-stateful-v4.0.0/repricings_stateful/perf-devnet-3
```

Download from the suite fixture URL:

```sh
benchmarkoor-replay fixtures download
```

Discover the matching asset from a gas-benchmarks release:

```sh
benchmarkoor-replay fixtures download \
  --release https://github.com/NethermindEth/gas-benchmarks/releases/tag/amsterdam-repricings-v4.1.0
```

Search and inspect:

```sh
benchmarkoor-replay fixtures list-tests --opcode CALL --gas-bucket 210M --cache-strategy NO_CACHE
benchmarkoor-replay fixtures list-tests --contains sload --command
benchmarkoor-replay fixtures show-test '<filename>.txt'
```

`--command` prints a copy-pasteable `benchmarkoor-replay run` command including the selected suite,
context, fork, test type, metadata root, cache root, Engine API endpoint, and custom binaries.

## Replay

Run a single indexed test:

```sh
benchmarkoor-replay --engine-url http://127.0.0.1:8551 --jwt-secret /path/to/jwt.hex \
  run --test '<filename>.txt' --mode full
```

Replay raw fixture files directly:

```sh
benchmarkoor-replay replay --dry-run ./funding.txt ./setup/example.txt ./testing/example.txt
```

Replay modes:

- `prerun`: gas-bump plus funding files
- `funding`: funding file only
- `setup`: setup file only
- `testing`: testing file only
- `full`: setup then testing, with no recovery between them

Run many tests:

```sh
benchmarkoor-replay run-many --contains sload --limit 10 --mode full --drop-caches
```

`run-many` recovers with schelk between tests. Use `--no-schelk` for dry fixture inspection.

For setup-unmeasured, testing-measured runs, use the native measured mode:

```sh
benchmarkoor-replay run-many --contains sload --limit 10 --repetitions 3 \
  --mode setup-then-testing \
  --restart-node-command 'systemctl restart reth' \
  --drop-caches \
  --json
```

`setup-then-testing` recovers to the promoted post-prerun schelk baseline before every test
repetition, replays setup without including it in the testing timer, optionally runs a node
restart command, optionally drops Linux page cache, then measures only testing replay. The JSON
output prints one object per test repetition with setup/testing elapsed seconds, request counts,
newPayload counts, exact `gasUsed` summed from Engine API payloads, and testing gas/sec.
`cache_drop` and `node_restart` fields report whether those controls were requested and succeeded.

## Snapshots

Import the selected suite snapshot:

```sh
benchmarkoor-replay snapshot import --datadir /schelk/reth --migrate-v2
```

Snapshot import downloads the suite snapshot and genesis when needed, extracts the archive,
normalizes one-level nested datadir layouts, optionally runs `reth db migrate-v2`, then verifies:

- the genesis JSON has `config.chainId`
- `reth db stats` succeeds with the selected genesis
- the Finish stage checkpoint equals the expected suite block
- headers exist for block `0` and the expected head, using static files or MDBX

For safety, import refuses to extract over a non-empty datadir unless `--force` is passed.
Snapshot archives are cached beside the datadir with the suite slug in the filename, so
different suites do not collide on `snapshot.tar.zst`.

## Baselines

Prepare without promoting:

```sh
benchmarkoor-replay baseline prepare
```

Promote only through the explicit baseline command:

```sh
benchmarkoor-replay baseline promote-prerun
```

Verify the marker:

```sh
benchmarkoor-replay baseline verify --datadir /schelk/reth
```

Baseline dry-runs print replay actions but skip schelk mount/promote and marker writes.

## Schelk Helpers

```sh
benchmarkoor-replay schelk mount
benchmarkoor-replay schelk recover --drop-caches
benchmarkoor-replay schelk full-recover --yes
```

`schelk full-recover` requires `--yes` because it overwrites scratch from virgin.
