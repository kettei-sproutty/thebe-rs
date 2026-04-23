# Benchmark Fixture Contract

Framework comparison fixtures under `benchmarks/fixtures/` are expected to expose the same benchmark surface so `scripts/perf/run.cjs` can measure them without target-specific browser logic.

## Required Page Contract

Each target's benchmark page must:

1. Serve the main comparison page at `/`.
2. Expose a browser global named `window.__frameworkBench`.
3. Record a `PerformanceMeasure` named `framework-bench:bootstrap`.

## `window.__frameworkBench`

The object must have this shape:

```js
window.__frameworkBench = {
  framework: "name",
  async writeCount(nextValue) {
    // resolve only after the DOM reflects the new value
  },
  readCount() {
    return document
      .querySelector("[data-framework-bench-counter]")
      ?.textContent?.trim() ?? null;
  }
};
```

The runner uses `writeCount()` for the DOM patch benchmark and validates the final DOM value through `readCount()`.

## Bootstrap Measure

Each target should emit these marks at the framework-defined start and the point where the app is interactive enough for the benchmark hooks to be installed:

```js
performance.mark("framework-bench:bootstrap:start");
performance.mark("framework-bench:bootstrap:ready");
performance.measure(
  "framework-bench:bootstrap",
  "framework-bench:bootstrap:start",
  "framework-bench:bootstrap:ready"
);
```

The exact implementation point will differ by framework, but it should represent “client code is ready and the benchmark hooks are installed,” not first paint.

## Current Targets

- `thebe-counter` — current Thebe reference fixture in `examples/counter-app/`
- `next-counter` — current Next.js reference fixture in `benchmarks/fixtures/next-counter/`
- `nuxt-counter` — Nuxt reference fixture in `benchmarks/fixtures/nuxt-counter/`
- `sveltekit-counter` — SvelteKit reference fixture in `benchmarks/fixtures/sveltekit-counter/`
- `solidstart-counter` — SolidStart reference fixture in `benchmarks/fixtures/solidstart-counter/`

Future targets such as Astro and Leptos should implement the same contract before being added to `scripts/perf/targets.cjs`.
