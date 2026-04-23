# Performance Benchmarks

Thebe now ships with a local benchmark harness aimed at tracking the real user-facing path for the current architecture and running the same measurements across multiple framework fixtures:

1. SSR request throughput and latency against a built example app.
2. Runtime bootstrap time in the browser.
3. Reactive DOM patch latency for a bound value.

The harness currently includes:

- `thebe-counter` — `examples/counter-app/`
- `next-counter` — `benchmarks/fixtures/next-counter/`
- `nuxt-counter` — `benchmarks/fixtures/nuxt-counter/`
- `sveltekit-counter` — `benchmarks/fixtures/sveltekit-counter/`
- `solidstart-counter` — `benchmarks/fixtures/solidstart-counter/`

All targets implement the same fixture contract documented in `benchmarks/fixtures/README.md` so SSR, bootstrap, and DOM patch numbers come from the same harness logic.

## What Gets Measured

### SSR

The runner starts the configured target server, issues a short concurrent request burst against `/`, and records:

- requests per second
- p50 latency
- p95 latency
- response size

### Runtime Bootstrap

Each fixture records a browser `PerformanceMeasure` named `framework-bench:bootstrap` from the framework-defined bootstrap start to the point where the page is interactive enough for the benchmark hooks to be installed.

The harness loads the page in headless Chromium several times and reports p50/p95 for that measure.

### DOM Patch Latency

The harness opens the page, calls `window.__frameworkBench.writeCount(nextValue)`, verifies the final DOM value through `window.__frameworkBench.readCount()`, and records:

- total sample duration
- per-mutation latency in microseconds

It also verifies that the final rendered count matches the written value so the benchmark does not silently time a no-op path.

## Commands

Install the one local browser dependency once:

```sh
just perf-install
```

Run the full harness:

```sh
just perf
```

List the configured targets:

```sh
just perf-list
```

Run a specific target:

```sh
just perf-target next-counter
```

Run again without rebuilding the example first:

```sh
just perf-quick
```

Run a specific target without rebuilding it first:

```sh
just perf-target-quick thebe-counter
```

Save a local baseline:

```sh
just perf-baseline before-cache
```

Compare a new run against that baseline:

```sh
just perf-compare before-cache
```

Compare a specific target against a saved baseline:

```sh
just perf-target-compare next-counter thebe-current
```

## Output

Reports are written locally under `benchmarks/results/` and are intentionally git-ignored:

- `benchmarks/results/latest.json` — most recent run
- `benchmarks/results/latest.html` — browser-friendly copy of the most recent run
- `benchmarks/results/latest-matrix.html` — combined HTML matrix built from the latest report for each configured target
- `benchmarks/results/latest-<target>.json` — most recent run for a specific target
- `benchmarks/results/latest-<target>.html` — browser-friendly copy of the most recent run for a specific target
- `benchmarks/results/reports/*.json` — timestamped historical runs
- `benchmarks/results/reports/*.html` — timestamped HTML reports, including baseline comparison tables when `--compare-to` is used
- `benchmarks/results/reports/matrix-*.html` — timestamped combined matrix pages built from the currently available latest target reports
- `benchmarks/results/baselines/*.json` — named local baselines

This keeps machine-specific benchmark numbers out of the repository while still making regressions and improvements easy to track on the same machine. The matrix HTML refreshes every time a target report is written, so once each framework has a recent local run you can open one page and compare the full set side by side.

## Notes

- The harness expects port `3000` to be free.
- Different targets may use different local ports; the configured target URL must be free before a run starts.
- The default run rebuilds the `thebe-counter` target in release mode by invoking `thebe build` from `examples/counter-app/`.
- Benchmark numbers are only meaningful when compared on the same machine and under similar thermal/load conditions.
