#!/usr/bin/env node

const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawn } = require("child_process");
const {
  BENCHMARK_BOOTSTRAP_MEASURE,
  BENCHMARK_GLOBAL,
  getTarget,
  listTargets,
} = require("./targets.cjs");

let chromium;
try {
  ({ chromium } = require("playwright"));
} catch (error) {
  console.error("Playwright is required for the perf runner.");
  console.error("Install it once with: npm install --prefix scripts/perf");
  console.error(error && error.message ? error.message : error);
  process.exit(1);
}

const ROOT_DIR = path.resolve(__dirname, "..", "..");
const PERF_DIR = path.join(ROOT_DIR, "benchmarks", "results");
const REPORTS_DIR = path.join(PERF_DIR, "reports");
const BASELINES_DIR = path.join(PERF_DIR, "baselines");
const DEFAULTS = {
  target: "thebe-counter",
  skipBuild: false,
  ssrConcurrency: 8,
  ssrDurationMs: 3_000,
  ssrWarmupRequests: 5,
  bootstrapSamples: 8,
  domPatchSamples: 8,
  domPatchIterations: 1_000,
  compareTo: null,
  saveBaseline: null,
};

async function main() {
  if (typeof fetch !== "function") {
    throw new Error("Node 18+ is required because the perf runner uses fetch().");
  }

  const options = parseArgs(process.argv.slice(2));
  if (options.listTargets) {
    printTargets();
    return;
  }

  const target = getTarget(ROOT_DIR, options.target);
  ensureDir(REPORTS_DIR);
  ensureDir(BASELINES_DIR);

  await assertServerNotRunning(target);

  if (!options.skipBuild) {
    await buildTarget(target);
  } else if (!fs.existsSync(target.start.command)) {
    throw new Error(
      `--skip-build was passed, but no release binary exists at ${target.start.command}`
    );
  }

  const report = {
    generatedAt: new Date().toISOString(),
    target: {
      id: target.id,
      label: target.label,
      framework: target.framework,
      url: target.url,
      rootDir: path.relative(ROOT_DIR, target.rootDir),
      binary: path.relative(ROOT_DIR, target.start.command),
    },
    host: {
      platform: process.platform,
      arch: process.arch,
      node: process.version,
      cpuModel: os.cpus()[0] ? os.cpus()[0].model : "unknown",
      cpuCount: os.cpus().length,
      totalMemoryGb: roundNumber(os.totalmem() / (1024 ** 3), 2),
    },
    config: {
      skipBuild: options.skipBuild,
      ssrConcurrency: options.ssrConcurrency,
      ssrDurationMs: options.ssrDurationMs,
      ssrWarmupRequests: options.ssrWarmupRequests,
      bootstrapSamples: options.bootstrapSamples,
      domPatchSamples: options.domPatchSamples,
      domPatchIterations: options.domPatchIterations,
    },
    metrics: {},
  };

  const server = await startServer(target);

  try {
    await warmupRequests(target, options.ssrWarmupRequests);
    report.metrics.ssr = await measureSsr({
      target,
      concurrency: options.ssrConcurrency,
      durationMs: options.ssrDurationMs,
    });
    report.metrics.bootstrap = await measureBootstrap({
      target,
      samples: options.bootstrapSamples,
    });
    report.metrics.domPatch = await measureDomPatch({
      target,
      samples: options.domPatchSamples,
      iterations: options.domPatchIterations,
    });
  } finally {
    await stopServer(server);
  }

  let comparison = null;
  let savedBaselinePath = null;

  if (options.saveBaseline) {
    savedBaselinePath = path.join(BASELINES_DIR, `${options.saveBaseline}.json`);
    fs.writeFileSync(savedBaselinePath, JSON.stringify(report, null, 2) + "\n");
  }

  if (options.compareTo) {
    const baselinePath = path.join(BASELINES_DIR, `${options.compareTo}.json`);
    if (!fs.existsSync(baselinePath)) {
      throw new Error(`Baseline not found: ${baselinePath}`);
    }

    const baseline = JSON.parse(fs.readFileSync(baselinePath, "utf8"));
    comparison = compareReports(report, baseline, options.compareTo);
  }

  if (comparison) {
    report.comparison = comparison;
  }

  if (savedBaselinePath || options.compareTo) {
    report.baseline = {
      savedAs: options.saveBaseline || null,
      savedPath: savedBaselinePath ? path.relative(ROOT_DIR, savedBaselinePath) : null,
      comparedTo: options.compareTo || null,
    };
  }

  const reportPaths = writeReport(report, target, {
    comparison,
    savedBaselinePath,
  });

  printSummary(report, reportPaths, comparison);
}

function parseArgs(argv) {
  const options = { ...DEFAULTS };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const value = argv[index + 1];

    switch (arg) {
      case "--target":
        if (!value) {
          throw new Error("--target requires a target id");
        }
        options.target = value;
        index += 1;
        break;
      case "--list-targets":
        options.listTargets = true;
        break;
      case "--skip-build":
        options.skipBuild = true;
        break;
      case "--save-baseline":
        if (!value) {
          throw new Error("--save-baseline requires a name");
        }
        options.saveBaseline = value;
        index += 1;
        break;
      case "--compare-to":
        if (!value) {
          throw new Error("--compare-to requires a baseline name");
        }
        options.compareTo = value;
        index += 1;
        break;
      case "--ssr-concurrency":
        options.ssrConcurrency = parsePositiveInteger(value, arg);
        index += 1;
        break;
      case "--ssr-duration-ms":
        options.ssrDurationMs = parsePositiveInteger(value, arg);
        index += 1;
        break;
      case "--bootstrap-samples":
        options.bootstrapSamples = parsePositiveInteger(value, arg);
        index += 1;
        break;
      case "--dom-patch-samples":
        options.domPatchSamples = parsePositiveInteger(value, arg);
        index += 1;
        break;
      case "--dom-patch-iterations":
        options.domPatchIterations = parsePositiveInteger(value, arg);
        index += 1;
        break;
      default:
        throw new Error(`Unknown argument: ${arg}`);
    }
  }

  return options;
}

function parsePositiveInteger(value, flagName) {
  const parsed = Number.parseInt(value || "", 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(`${flagName} requires a positive integer`);
  }
  return parsed;
}

function ensureDir(dirPath) {
  fs.mkdirSync(dirPath, { recursive: true });
}

async function assertServerNotRunning(target) {
  try {
    const response = await fetch(target.url, { redirect: "manual" });
    if (response.ok) {
      throw new Error(
        `A server is already responding at ${target.url}. Stop it before running the perf harness.`
      );
    }
  } catch (error) {
    if (String(error.message || error).includes("already responding")) {
      throw error;
    }
  }
}

async function buildTarget(target) {
  if (target.prepare && !fs.existsSync(target.prepare.marker)) {
    await runCommand(target.prepare.command, target.prepare.args, {
      cwd: target.prepare.cwd,
      env: target.prepare.env,
    });
  }

  await runCommand(target.build.command, target.build.args, {
    cwd: target.build.cwd,
    env: target.build.env,
  });
}

async function runCommand(command, args, options) {
  await new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: options.cwd,
      env: {
        ...process.env,
        ...(options.env || {}),
      },
      stdio: "inherit",
    });

    child.on("error", reject);
    child.on("exit", (code, signal) => {
      if (code === 0) {
        resolve();
        return;
      }

      reject(
        new Error(
          `${command} ${args.join(" ")} failed with ${signal || `exit code ${code}`}`
        )
      );
    });
  });
}

async function startServer(target) {
  if (!fs.existsSync(target.start.command)) {
    throw new Error(`Release binary not found at ${target.start.command}`);
  }

  const child = spawn(target.start.command, target.start.args, {
    cwd: target.start.cwd,
    env: {
      ...process.env,
      ...(target.start.env || {}),
    },
    stdio: ["ignore", "pipe", "pipe"],
  });

  child.stdout.on("data", (chunk) => {
    process.stdout.write(chunk);
  });

  child.stderr.on("data", (chunk) => {
    process.stderr.write(chunk);
  });

  await waitForServer(target, 30_000, child);
  return child;
}

async function waitForServer(target, timeoutMs, child) {
  const startedAt = Date.now();

  while (Date.now() - startedAt < timeoutMs) {
    if (child.exitCode !== null) {
      throw new Error(`Server exited before it became ready (exit code ${child.exitCode})`);
    }

    try {
      const response = await fetch(target.url, { redirect: "manual" });
      if (response.ok) {
        return;
      }
    } catch (_) {
      // Server not ready yet.
    }

    await sleep(250);
  }

  throw new Error(`Timed out waiting for ${target.url}`);
}

async function stopServer(child) {
  if (!child || child.exitCode !== null) {
    return;
  }

  child.kill("SIGTERM");

  await Promise.race([
    new Promise((resolve) => child.once("exit", resolve)),
    sleep(5_000).then(() => {
      if (child.exitCode === null) {
        child.kill("SIGKILL");
      }
    }),
  ]);
}

async function warmupRequests(target, count) {
  for (let index = 0; index < count; index += 1) {
    await fetchHtml(target.url);
  }
}

async function measureSsr({ target, concurrency, durationMs }) {
  const latenciesMs = [];
  let htmlBytes = 0;
  let requests = 0;
  const startedAt = process.hrtime.bigint();
  const deadline = Date.now() + durationMs;

  async function worker() {
    while (Date.now() < deadline) {
      const requestStartedAt = process.hrtime.bigint();
      const html = await fetchHtml(target.url);
      const requestEndedAt = process.hrtime.bigint();
      const duration = Number(requestEndedAt - requestStartedAt) / 1_000_000;

      if (htmlBytes === 0) {
        htmlBytes = Buffer.byteLength(html);
      }

      latenciesMs.push(duration);
      requests += 1;
    }
  }

  await Promise.all(Array.from({ length: concurrency }, worker));

  const elapsedMs = Number(process.hrtime.bigint() - startedAt) / 1_000_000;
  const stats = summarizeSamples(latenciesMs);

  return {
    concurrency,
    elapsedMs: roundNumber(elapsedMs, 2),
    htmlBytes,
    requests,
    requestsPerSecond: roundNumber(requests / (elapsedMs / 1000), 2),
    ...stats,
  };
}

async function measureBootstrap({ target, samples }) {
  const browser = await chromium.launch({ headless: true });
  const durationsMs = [];

  try {
    for (let index = 0; index < samples; index += 1) {
      const context = await browser.newContext();
      const page = await context.newPage();
      const url = new URL(target.url);
      url.searchParams.set("__framework_bench_bootstrap", String(index));

      await page.goto(url.toString(), { waitUntil: "domcontentloaded" });
      await page.waitForFunction(
        ({ benchGlobal, measureName }) => {
          const bench = window[benchGlobal];
          return (
            !!bench &&
            performance.getEntriesByName(measureName).length > 0
          );
        },
        {
          benchGlobal: BENCHMARK_GLOBAL,
          measureName: BENCHMARK_BOOTSTRAP_MEASURE,
        },
        { timeout: 30_000 }
      );

      const durationMs = await page.evaluate((measureName) => {
        const entries = performance.getEntriesByName(measureName);
        return entries[entries.length - 1].duration;
      }, BENCHMARK_BOOTSTRAP_MEASURE);

      durationsMs.push(durationMs);
      await context.close();
    }
  } finally {
    await browser.close();
  }

  return {
    samples,
    ...summarizeSamples(durationsMs),
  };
}

async function measureDomPatch({ target, samples, iterations }) {
  const browser = await chromium.launch({ headless: true });
  const totalDurationsMs = [];
  const perMutationUs = [];

  try {
    for (let index = 0; index < samples; index += 1) {
      const context = await browser.newContext();
      const page = await context.newPage();
      const url = new URL(target.url);
      url.searchParams.set("__framework_bench_dom_patch", String(index));

      await page.goto(url.toString(), { waitUntil: "domcontentloaded" });
      await page.waitForFunction(
        (benchGlobal) => {
          const bench = window[benchGlobal];
          return (
            !!bench &&
            typeof bench.writeCount === "function" &&
            typeof bench.readCount === "function"
          );
        },
        BENCHMARK_GLOBAL,
        { timeout: 30_000 }
      );

      const result = await page.evaluate(async ({ sampleIterations, benchGlobal }) => {
        const bench = window[benchGlobal];
        const startedAt = performance.now();

        for (let iteration = 1; iteration <= sampleIterations; iteration += 1) {
          await bench.writeCount(iteration);
        }

        const totalMs = performance.now() - startedAt;

        return {
          totalMs,
          renderedValue: await bench.readCount(),
        };
      }, { sampleIterations: iterations, benchGlobal: BENCHMARK_GLOBAL });

      if (result.renderedValue !== String(iterations)) {
        throw new Error(
          `DOM patch benchmark rendered ${result.renderedValue}, expected ${iterations}`
        );
      }

      totalDurationsMs.push(result.totalMs);
      perMutationUs.push((result.totalMs * 1000) / iterations);

      await context.close();
    }
  } finally {
    await browser.close();
  }

  return {
    samples,
    iterationsPerSample: iterations,
    totalMs: summarizeSamples(totalDurationsMs),
    perMutationUs: summarizeSamples(perMutationUs),
  };
}

async function fetchHtml(url) {
  const response = await fetch(url, {
    headers: {
      Accept: "text/html",
      "Cache-Control": "no-store",
    },
    redirect: "manual",
  });

  if (!response.ok) {
    throw new Error(`Request to ${url} failed with status ${response.status}`);
  }

  return response.text();
}

function summarizeSamples(samples) {
  const sorted = [...samples].sort((left, right) => left - right);
  const mean = sorted.reduce((sum, value) => sum + value, 0) / sorted.length;

  return {
    samples: samples.length,
    min: roundNumber(sorted[0], 3),
    max: roundNumber(sorted[sorted.length - 1], 3),
    mean: roundNumber(mean, 3),
    p50: roundNumber(percentile(sorted, 0.5), 3),
    p95: roundNumber(percentile(sorted, 0.95), 3),
  };
}

function percentile(sorted, fraction) {
  if (sorted.length === 1) {
    return sorted[0];
  }

  const position = (sorted.length - 1) * fraction;
  const lowerIndex = Math.floor(position);
  const upperIndex = Math.ceil(position);
  const weight = position - lowerIndex;

  if (lowerIndex === upperIndex) {
    return sorted[lowerIndex];
  }

  return sorted[lowerIndex] + (sorted[upperIndex] - sorted[lowerIndex]) * weight;
}

function roundNumber(value, digits) {
  const factor = 10 ** digits;
  return Math.round(value * factor) / factor;
}

function writeReport(report, target, options = {}) {
  const timestamp = report.generatedAt.replace(/[:.]/g, "-");
  const latestSummaryPath = path.join(PERF_DIR, "latest.json");
  const latestSummaryHtmlPath = path.join(PERF_DIR, "latest.html");
  const latestPath = path.join(PERF_DIR, `latest-${target.id}.json`);
  const latestHtmlPath = path.join(PERF_DIR, `latest-${target.id}.html`);
  const reportPath = path.join(REPORTS_DIR, `${target.id}-${timestamp}.json`);
  const reportHtmlPath = path.join(REPORTS_DIR, `${target.id}-${timestamp}.html`);
  const reportJson = JSON.stringify(report, null, 2) + "\n";
  const reportHtml = renderHtmlReport(report, {
    comparison: options.comparison || null,
    savedBaselinePath: options.savedBaselinePath || null,
  });

  fs.writeFileSync(latestSummaryPath, reportJson);
  fs.writeFileSync(latestPath, reportJson);
  fs.writeFileSync(reportPath, reportJson);

  fs.writeFileSync(latestSummaryHtmlPath, reportHtml);
  fs.writeFileSync(latestHtmlPath, reportHtml);
  fs.writeFileSync(reportHtmlPath, reportHtml);

  const aggregatePaths = writeAggregateReport(report.generatedAt);

  return {
    latestSummaryPath,
    latestSummaryHtmlPath,
    latestPath,
    latestHtmlPath,
    reportPath,
    reportHtmlPath,
    latestMatrixHtmlPath: aggregatePaths.latestMatrixHtmlPath,
    matrixReportHtmlPath: aggregatePaths.matrixReportHtmlPath,
    savedBaseline: options.savedBaselinePath || null,
  };
}

function writeAggregateReport(generatedAt) {
  const timestamp = generatedAt.replace(/[:.]/g, "-");
  const latestMatrixHtmlPath = path.join(PERF_DIR, "latest-matrix.html");
  const matrixReportHtmlPath = path.join(REPORTS_DIR, `matrix-${timestamp}.html`);
  const aggregateState = collectAggregateReports(generatedAt);
  const matrixHtml = renderAggregateHtmlReport(aggregateState);

  fs.writeFileSync(latestMatrixHtmlPath, matrixHtml);
  fs.writeFileSync(matrixReportHtmlPath, matrixHtml);

  return {
    latestMatrixHtmlPath,
    matrixReportHtmlPath,
  };
}

function collectAggregateReports(generatedAt) {
  const configuredTargets = listTargets(ROOT_DIR);
  const availableReports = configuredTargets
    .map((target) => {
      const latestReportPath = path.join(PERF_DIR, `latest-${target.id}.json`);
      if (!fs.existsSync(latestReportPath)) {
        return null;
      }

      return {
        target,
        latestReportPath,
        report: JSON.parse(fs.readFileSync(latestReportPath, "utf8")),
      };
    })
    .filter(Boolean)
    .sort((left, right) => right.report.metrics.ssr.requestsPerSecond - left.report.metrics.ssr.requestsPerSecond);

  const availableIds = new Set(availableReports.map((entry) => entry.target.id));
  const missingTargets = configuredTargets.filter((target) => !availableIds.has(target.id));

  return {
    generatedAt,
    configuredCount: configuredTargets.length,
    availableReports,
    missingTargets,
    leaders: availableReports.length > 0
      ? {
          ssr: Math.max(...availableReports.map((entry) => entry.report.metrics.ssr.requestsPerSecond)),
          bootstrap: Math.min(...availableReports.map((entry) => entry.report.metrics.bootstrap.p95)),
          domPatch: Math.min(...availableReports.map((entry) => entry.report.metrics.domPatch.perMutationUs.p95)),
        }
      : null,
  };
}

function renderHtmlReport(report, options) {
  const comparison = options.comparison;
  const savedBaselinePath = options.savedBaselinePath;
  const summaryCards = [
    {
      label: "SSR throughput",
      value: `${formatValue(report.metrics.ssr.requestsPerSecond)} req/s`,
      tone: "throughput",
    },
    {
      label: "Bootstrap p95",
      value: `${formatValue(report.metrics.bootstrap.p95)} ms`,
      tone: "bootstrap",
    },
    {
      label: "DOM patch p95",
      value: `${formatValue(report.metrics.domPatch.perMutationUs.p95)} us`,
      tone: "patch",
    },
  ];
  const comparisonMarkup = comparison
    ? `
      <section class="panel panel-wide">
        <div class="panel-header">
          <div>
            <p class="eyebrow">Comparison</p>
            <h2>Baseline delta vs ${escapeHtml(comparison.baselineName)}</h2>
          </div>
        </div>
        <table>
          <thead>
            <tr>
              <th>Metric</th>
              <th>Current</th>
              <th>Baseline</th>
              <th>Delta</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            ${comparison.checks
              .map((check) => {
                const direction = check.improved ? "improved" : "regressed";
                const deltaPrefix = check.delta > 0 ? "+" : "";
                const deltaPctPrefix = check.deltaPct > 0 ? "+" : "";

                return `
                  <tr>
                    <td>${escapeHtml(check.label)}</td>
                    <td>${formatValue(check.current)} ${escapeHtml(check.unit)}</td>
                    <td>${formatValue(check.baseline)} ${escapeHtml(check.unit)}</td>
                    <td>${deltaPrefix}${formatValue(check.delta)} ${escapeHtml(check.unit)} <span class="muted">(${deltaPctPrefix}${formatValue(check.deltaPct)}%)</span></td>
                    <td><span class="status ${direction}">${direction}</span></td>
                  </tr>
                `;
              })
              .join("")}
          </tbody>
        </table>
      </section>
    `
    : `
      <section class="panel panel-wide">
        <div class="panel-header">
          <div>
            <p class="eyebrow">Comparison</p>
            <h2>No baseline comparison attached</h2>
          </div>
        </div>
        <p class="empty-state">Run the harness with <code>--compare-to &lt;baseline&gt;</code> to embed comparison deltas in this HTML report.</p>
      </section>
    `;

  return `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>${escapeHtml(report.target.label)} perf report</title>
    <style>
      :root {
        color-scheme: light;
        --bg: #f2efe8;
        --panel: rgba(255, 253, 248, 0.88);
        --panel-strong: #fffdf8;
        --text: #1c1914;
        --muted: #6b6254;
        --border: rgba(42, 34, 22, 0.12);
        --shadow: 0 24px 60px rgba(48, 36, 16, 0.12);
        --throughput: #a24a24;
        --bootstrap: #0f6d67;
        --patch: #6e4ca0;
        --good: #146c2e;
        --bad: #a12d2d;
        --good-bg: rgba(20, 108, 46, 0.12);
        --bad-bg: rgba(161, 45, 45, 0.12);
      }

      * {
        box-sizing: border-box;
      }

      body {
        margin: 0;
        font-family: "IBM Plex Sans", "Avenir Next", "Segoe UI", sans-serif;
        color: var(--text);
        background:
          radial-gradient(circle at top left, rgba(162, 74, 36, 0.16), transparent 28%),
          radial-gradient(circle at top right, rgba(15, 109, 103, 0.14), transparent 26%),
          linear-gradient(180deg, #f7f3ec 0%, var(--bg) 100%);
      }

      main {
        width: min(1200px, calc(100vw - 32px));
        margin: 0 auto;
        padding: 40px 0 56px;
      }

      .hero {
        background: linear-gradient(145deg, rgba(255, 250, 241, 0.96), rgba(245, 239, 228, 0.92));
        border: 1px solid var(--border);
        border-radius: 28px;
        box-shadow: var(--shadow);
        padding: 28px 28px 24px;
        margin-bottom: 20px;
      }

      .eyebrow {
        margin: 0 0 8px;
        font-size: 0.72rem;
        letter-spacing: 0.14em;
        text-transform: uppercase;
        color: var(--muted);
      }

      h1, h2, h3, p {
        margin: 0;
      }

      h1 {
        font-family: "Iowan Old Style", "Palatino Linotype", serif;
        font-size: clamp(2.4rem, 5vw, 4rem);
        line-height: 0.96;
        letter-spacing: -0.04em;
      }

      .subtitle {
        margin-top: 12px;
        color: var(--muted);
        max-width: 58rem;
        line-height: 1.55;
      }

      .meta-grid,
      .summary-grid,
      .detail-grid {
        display: grid;
        gap: 16px;
      }

      .meta-grid {
        grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
        margin-top: 20px;
      }

      .summary-grid {
        grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
        margin-bottom: 20px;
      }

      .detail-grid {
        grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
      }

      .panel,
      .summary-card,
      .meta-card {
        background: var(--panel);
        border: 1px solid var(--border);
        border-radius: 22px;
        box-shadow: var(--shadow);
        backdrop-filter: blur(10px);
      }

      .meta-card,
      .summary-card {
        padding: 18px 18px 20px;
      }

      .meta-card dt,
      .summary-card .label {
        font-size: 0.76rem;
        text-transform: uppercase;
        letter-spacing: 0.08em;
        color: var(--muted);
      }

      .meta-card dd {
        margin: 8px 0 0;
        font-size: 1rem;
        line-height: 1.45;
      }

      .summary-card .value {
        display: block;
        margin-top: 8px;
        font-size: 2rem;
        font-weight: 700;
        letter-spacing: -0.04em;
      }

      .summary-card.throughput .value { color: var(--throughput); }
      .summary-card.bootstrap .value { color: var(--bootstrap); }
      .summary-card.patch .value { color: var(--patch); }

      .panel {
        padding: 20px;
      }

      .panel-wide {
        margin-top: 20px;
      }

      .panel-header {
        display: flex;
        align-items: start;
        justify-content: space-between;
        gap: 12px;
        margin-bottom: 14px;
      }

      h2 {
        font-size: 1.2rem;
        letter-spacing: -0.03em;
      }

      table {
        width: 100%;
        border-collapse: collapse;
      }

      th,
      td {
        padding: 12px 10px;
        text-align: left;
        border-top: 1px solid rgba(42, 34, 22, 0.08);
        font-size: 0.96rem;
      }

      th {
        font-size: 0.76rem;
        text-transform: uppercase;
        letter-spacing: 0.08em;
        color: var(--muted);
        border-top: none;
      }

      td:first-child,
      th:first-child {
        padding-left: 0;
      }

      td:last-child,
      th:last-child {
        padding-right: 0;
      }

      .status {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        min-width: 96px;
        padding: 6px 10px;
        border-radius: 999px;
        font-size: 0.78rem;
        font-weight: 700;
        text-transform: uppercase;
        letter-spacing: 0.06em;
      }

      .status.improved {
        color: var(--good);
        background: var(--good-bg);
      }

      .status.regressed {
        color: var(--bad);
        background: var(--bad-bg);
      }

      .muted {
        color: var(--muted);
      }

      .empty-state {
        color: var(--muted);
        line-height: 1.6;
      }

      .footer-note {
        margin-top: 12px;
        font-size: 0.92rem;
        color: var(--muted);
      }

      code {
        font-family: "SFMono-Regular", "Consolas", monospace;
        font-size: 0.9em;
      }

      @media (max-width: 700px) {
        main {
          width: min(100vw - 20px, 1200px);
          padding-top: 20px;
        }

        .hero,
        .panel,
        .meta-card,
        .summary-card {
          border-radius: 20px;
        }

        table,
        thead,
        tbody,
        tr,
        th,
        td {
          display: block;
        }

        thead {
          display: none;
        }

        tbody tr {
          padding: 12px 0;
          border-top: 1px solid rgba(42, 34, 22, 0.08);
        }

        tbody tr:first-child {
          border-top: none;
        }

        td {
          border-top: none;
          padding: 6px 0;
        }
      }
    </style>
  </head>
  <body>
    <main>
      <section class="hero">
        <p class="eyebrow">Thebe performance harness</p>
        <h1>${escapeHtml(report.target.label)}</h1>
        <p class="subtitle">Browser-friendly summary for the ${escapeHtml(report.target.framework)} target. This report mirrors the JSON artifact and includes comparison deltas when the run was executed with a saved baseline.</p>
        <dl class="meta-grid">
          <div class="meta-card">
            <dt>Target</dt>
            <dd>${escapeHtml(report.target.id)}</dd>
          </div>
          <div class="meta-card">
            <dt>Framework</dt>
            <dd>${escapeHtml(report.target.framework)}</dd>
          </div>
          <div class="meta-card">
            <dt>Generated</dt>
            <dd>${escapeHtml(report.generatedAt)}</dd>
          </div>
          <div class="meta-card">
            <dt>Host</dt>
            <dd>${escapeHtml(report.host.cpuModel)}<br /><span class="muted">${escapeHtml(report.host.platform)} ${escapeHtml(report.host.arch)} · Node ${escapeHtml(report.host.node)}</span></dd>
          </div>
        </dl>
        ${savedBaselinePath ? `<p class="footer-note">Saved baseline: <code>${escapeHtml(path.relative(ROOT_DIR, savedBaselinePath))}</code></p>` : ""}
      </section>

      <section class="summary-grid">
        ${summaryCards
          .map(
            (card) => `
              <article class="summary-card ${escapeHtml(card.tone)}">
                <span class="label">${escapeHtml(card.label)}</span>
                <strong class="value">${escapeHtml(card.value)}</strong>
              </article>
            `
          )
          .join("")}
      </section>

      <section class="detail-grid">
        ${renderMetricPanel("SSR", [
          ["Requests", report.metrics.ssr.requests, null],
          ["Requests / second", report.metrics.ssr.requestsPerSecond, "req/s"],
          ["p50 latency", report.metrics.ssr.p50, "ms"],
          ["p95 latency", report.metrics.ssr.p95, "ms"],
          ["HTML bytes", report.metrics.ssr.htmlBytes, "bytes"],
        ])}
        ${renderMetricPanel("Runtime bootstrap", [
          ["Samples", report.metrics.bootstrap.samples, null],
          ["Min", report.metrics.bootstrap.min, "ms"],
          ["Mean", report.metrics.bootstrap.mean, "ms"],
          ["p50", report.metrics.bootstrap.p50, "ms"],
          ["p95", report.metrics.bootstrap.p95, "ms"],
        ])}
        ${renderMetricPanel("DOM patch", [
          ["Samples", report.metrics.domPatch.samples, null],
          ["Iterations / sample", report.metrics.domPatch.iterationsPerSample, null],
          ["Total p50", report.metrics.domPatch.totalMs.p50, "ms"],
          ["Total p95", report.metrics.domPatch.totalMs.p95, "ms"],
          ["Per-mutation p50", report.metrics.domPatch.perMutationUs.p50, "us"],
          ["Per-mutation p95", report.metrics.domPatch.perMutationUs.p95, "us"],
        ])}
      </section>

      ${comparisonMarkup}

      <section class="detail-grid">
        ${renderMetricPanel("Run config", [
          ["Skip build", report.config.skipBuild ? "yes" : "no", null],
          ["SSR concurrency", report.config.ssrConcurrency, null],
          ["SSR duration", report.config.ssrDurationMs, "ms"],
          ["Bootstrap samples", report.config.bootstrapSamples, null],
          ["DOM patch samples", report.config.domPatchSamples, null],
          ["DOM patch iterations", report.config.domPatchIterations, null],
        ])}
        ${renderMetricPanel("Target metadata", [
          ["Label", report.target.label, null],
          ["URL", report.target.url, null],
          ["Root", report.target.rootDir, null],
          ["Binary / entry", report.target.binary, null],
          ["CPU count", report.host.cpuCount, null],
          ["Memory", report.host.totalMemoryGb, "GB"],
        ])}
      </section>
    </main>
  </body>
</html>`;
}

function renderAggregateHtmlReport(state) {
  const entries = state.availableReports;
  const leaders = state.leaders;
  const fastestSsr = entries[0] || null;
  const fastestBootstrap = entries.reduce((best, entry) => {
    if (!best || entry.report.metrics.bootstrap.p95 < best.report.metrics.bootstrap.p95) {
      return entry;
    }
    return best;
  }, null);
  const fastestDomPatch = entries.reduce((best, entry) => {
    if (!best || entry.report.metrics.domPatch.perMutationUs.p95 < best.report.metrics.domPatch.perMutationUs.p95) {
      return entry;
    }
    return best;
  }, null);
  const comparisonEntries = entries.filter(
    (entry) => entry.report.comparison && Array.isArray(entry.report.comparison.checks)
  );
  const missingTargetsMarkup = state.missingTargets.length > 0
    ? `<p class="footer-note">Missing latest runs: ${escapeHtml(
        state.missingTargets.map((target) => target.id).join(", ")
      )}. Run those targets to fill out the matrix.</p>`
    : `<p class="footer-note">All configured targets currently have a latest report in the matrix.</p>`;

  return `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Thebe perf matrix</title>
    <style>
      :root {
        color-scheme: light;
        --bg: #f3efe7;
        --panel: rgba(255, 252, 246, 0.9);
        --panel-strong: rgba(255, 255, 255, 0.94);
        --text: #1f1a14;
        --muted: #6b6255;
        --border: rgba(44, 36, 22, 0.12);
        --shadow: 0 28px 68px rgba(52, 41, 18, 0.12);
        --accent: #a24a24;
        --accent-soft: rgba(162, 74, 36, 0.12);
        --teal: #0f6d67;
        --teal-soft: rgba(15, 109, 103, 0.12);
        --blue: #2756b5;
        --blue-soft: rgba(39, 86, 181, 0.12);
        --gold: #9c6d1f;
        --gold-soft: rgba(156, 109, 31, 0.12);
        --good: #146c2e;
        --bad: #a12d2d;
        --good-bg: rgba(20, 108, 46, 0.12);
        --bad-bg: rgba(161, 45, 45, 0.12);
      }

      * {
        box-sizing: border-box;
      }

      body {
        margin: 0;
        font-family: "IBM Plex Sans", "Avenir Next", "Segoe UI", sans-serif;
        color: var(--text);
        background:
          radial-gradient(circle at top left, rgba(162, 74, 36, 0.16), transparent 28%),
          radial-gradient(circle at top right, rgba(15, 109, 103, 0.14), transparent 24%),
          linear-gradient(180deg, #f8f4ec 0%, var(--bg) 100%);
      }

      main {
        width: min(1320px, calc(100vw - 28px));
        margin: 0 auto;
        padding: 34px 0 56px;
      }

      .hero,
      .panel,
      .summary-card,
      .meta-card {
        border: 1px solid var(--border);
        border-radius: 26px;
        box-shadow: var(--shadow);
      }

      .hero {
        padding: 30px 30px 24px;
        margin-bottom: 18px;
        background:
          linear-gradient(140deg, rgba(255, 250, 242, 0.96), rgba(245, 239, 228, 0.92)),
          var(--panel-strong);
      }

      .eyebrow {
        margin: 0 0 8px;
        font-size: 0.72rem;
        letter-spacing: 0.14em;
        text-transform: uppercase;
        color: var(--muted);
      }

      h1,
      h2,
      h3,
      p {
        margin: 0;
      }

      h1 {
        font-family: "Iowan Old Style", "Palatino Linotype", serif;
        font-size: clamp(2.5rem, 5vw, 4.4rem);
        line-height: 0.96;
        letter-spacing: -0.045em;
      }

      .subtitle {
        margin-top: 12px;
        max-width: 64rem;
        color: var(--muted);
        line-height: 1.6;
      }

      .meta-grid,
      .summary-grid {
        display: grid;
        gap: 16px;
      }

      .meta-grid {
        grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
        margin-top: 20px;
      }

      .summary-grid {
        grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
        margin-bottom: 18px;
      }

      .meta-card,
      .summary-card,
      .panel {
        background: var(--panel);
        backdrop-filter: blur(10px);
      }

      .meta-card,
      .summary-card {
        padding: 18px 18px 20px;
      }

      .meta-card dt,
      .summary-card .label,
      .metric-note,
      .mini-label {
        font-size: 0.76rem;
        text-transform: uppercase;
        letter-spacing: 0.08em;
        color: var(--muted);
      }

      .meta-card dd {
        margin: 8px 0 0;
        font-size: 1rem;
        line-height: 1.45;
      }

      .summary-card .value {
        display: block;
        margin-top: 10px;
        font-size: 1.8rem;
        font-weight: 700;
        letter-spacing: -0.04em;
      }

      .summary-card.accent .value { color: var(--accent); }
      .summary-card.teal .value { color: var(--teal); }
      .summary-card.blue .value { color: var(--blue); }
      .summary-card.gold .value { color: var(--gold); }

      .summary-card .caption {
        margin-top: 8px;
        color: var(--muted);
        line-height: 1.5;
      }

      .panel {
        padding: 22px;
        margin-bottom: 18px;
      }

      .panel-header {
        display: flex;
        justify-content: space-between;
        align-items: start;
        gap: 14px;
        margin-bottom: 14px;
      }

      h2 {
        font-size: 1.2rem;
        letter-spacing: -0.03em;
      }

      table {
        width: 100%;
        border-collapse: collapse;
      }

      th,
      td {
        padding: 14px 10px;
        text-align: left;
        border-top: 1px solid rgba(42, 34, 22, 0.08);
        vertical-align: top;
      }

      th {
        padding-top: 0;
        font-size: 0.76rem;
        text-transform: uppercase;
        letter-spacing: 0.08em;
        color: var(--muted);
        border-top: none;
      }

      td:first-child,
      th:first-child {
        padding-left: 0;
      }

      td:last-child,
      th:last-child {
        padding-right: 0;
      }

      .rank {
        display: inline-flex;
        min-width: 38px;
        height: 38px;
        align-items: center;
        justify-content: center;
        border-radius: 999px;
        font-weight: 700;
        background: var(--accent-soft);
        color: var(--accent);
      }

      .framework-name {
        font-weight: 700;
        letter-spacing: -0.02em;
      }

      .framework-meta {
        margin-top: 4px;
        color: var(--muted);
        line-height: 1.4;
      }

      .metric-value {
        font-weight: 700;
        letter-spacing: -0.03em;
      }

      .metric-note {
        display: block;
        margin-top: 6px;
      }

      .pill,
      .status {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        border-radius: 999px;
        font-size: 0.76rem;
        font-weight: 700;
        letter-spacing: 0.06em;
        text-transform: uppercase;
      }

      .pill {
        margin-top: 6px;
        padding: 5px 10px;
        color: var(--gold);
        background: var(--gold-soft);
      }

      .status {
        min-width: 112px;
        padding: 6px 10px;
      }

      .status.improved {
        color: var(--good);
        background: var(--good-bg);
      }

      .status.regressed {
        color: var(--bad);
        background: var(--bad-bg);
      }

      .empty-state,
      .footer-note {
        color: var(--muted);
        line-height: 1.6;
      }

      code {
        font-family: "SFMono-Regular", "Consolas", monospace;
        font-size: 0.9em;
      }

      @media (max-width: 900px) {
        main {
          width: min(100vw - 18px, 1320px);
          padding-top: 20px;
        }

        .hero,
        .panel,
        .summary-card,
        .meta-card {
          border-radius: 22px;
        }

        table,
        thead,
        tbody,
        tr,
        th,
        td {
          display: block;
        }

        thead {
          display: none;
        }

        tbody tr {
          padding: 12px 0;
          border-top: 1px solid rgba(42, 34, 22, 0.08);
        }

        tbody tr:first-child {
          border-top: none;
        }

        td {
          border-top: none;
          padding: 6px 0;
        }

        td::before {
          content: attr(data-label);
          display: block;
          margin-bottom: 4px;
          font-size: 0.72rem;
          letter-spacing: 0.08em;
          text-transform: uppercase;
          color: var(--muted);
        }
      }
    </style>
  </head>
  <body>
    <main>
      <section class="hero">
        <p class="eyebrow">Thebe performance harness</p>
        <h1>Framework matrix</h1>
        <p class="subtitle">One HTML view for the latest benchmark result from every configured target. Each row comes from that target's latest local JSON report, and any row that was run with <code>--compare-to</code> also carries its saved-baseline delta.</p>
        <dl class="meta-grid">
          <div class="meta-card">
            <dt>Targets included</dt>
            <dd>${escapeHtml(String(entries.length))} / ${escapeHtml(String(state.configuredCount))}</dd>
          </div>
          <div class="meta-card">
            <dt>Generated</dt>
            <dd>${escapeHtml(state.generatedAt)}</dd>
          </div>
          <div class="meta-card">
            <dt>Missing targets</dt>
            <dd>${escapeHtml(state.missingTargets.length === 0 ? "none" : state.missingTargets.map((target) => target.id).join(", "))}</dd>
          </div>
          <div class="meta-card">
            <dt>Source</dt>
            <dd><code>benchmarks/results/latest-*.json</code></dd>
          </div>
        </dl>
        ${missingTargetsMarkup}
      </section>

      <section class="summary-grid">
        ${renderAggregateSummaryCard(
          "Targets included",
          `${entries.length} / ${state.configuredCount}`,
          entries.length === 0 ? "Run any perf target to start filling the matrix." : "The matrix refreshes every time a target report is written.",
          "accent"
        )}
        ${renderAggregateSummaryCard(
          "Fastest SSR",
          fastestSsr ? `${formatValue(fastestSsr.report.metrics.ssr.requestsPerSecond)} req/s` : "No runs yet",
          fastestSsr ? `${fastestSsr.target.label} leads current throughput.` : "",
          "teal"
        )}
        ${renderAggregateSummaryCard(
          "Lowest bootstrap p95",
          fastestBootstrap ? `${formatValue(fastestBootstrap.report.metrics.bootstrap.p95)} ms` : "No runs yet",
          fastestBootstrap ? `${fastestBootstrap.target.label} reaches interactivity fastest.` : "",
          "blue"
        )}
        ${renderAggregateSummaryCard(
          "Lowest DOM patch p95",
          fastestDomPatch ? `${formatValue(fastestDomPatch.report.metrics.domPatch.perMutationUs.p95)} us` : "No runs yet",
          fastestDomPatch ? `${fastestDomPatch.target.label} leads bound-value patch latency.` : "",
          "gold"
        )}
      </section>

      <section class="panel">
        <div class="panel-header">
          <div>
            <p class="eyebrow">Current metrics</p>
            <h2>Latest target runs</h2>
          </div>
        </div>
        ${entries.length === 0 ? `<p class="empty-state">No target reports were found under <code>benchmarks/results/latest-*.json</code> yet.</p>` : `
        <table>
          <thead>
            <tr>
              <th>Rank</th>
              <th>Framework</th>
              <th>SSR req/s</th>
              <th>SSR p95</th>
              <th>Bootstrap p95</th>
              <th>DOM patch p95</th>
              <th>Generated</th>
            </tr>
          </thead>
          <tbody>
            ${entries
              .map((entry, index) => {
                const report = entry.report;

                return `
                  <tr>
                    <td data-label="Rank"><span class="rank">${index + 1}</span></td>
                    <td data-label="Framework">
                      <div class="framework-name">${escapeHtml(entry.target.framework)}</div>
                      <div class="framework-meta">${escapeHtml(entry.target.label)}<br />${escapeHtml(entry.target.id)}</div>
                    </td>
                    <td data-label="SSR req/s">${renderAggregateMetricValue(report.metrics.ssr.requestsPerSecond, "req/s", leaders ? leaders.ssr : null, true)}</td>
                    <td data-label="SSR p95">${renderAggregateMetricValue(report.metrics.ssr.p95, "ms", null, false)}</td>
                    <td data-label="Bootstrap p95">${renderAggregateMetricValue(report.metrics.bootstrap.p95, "ms", leaders ? leaders.bootstrap : null, false)}</td>
                    <td data-label="DOM patch p95">${renderAggregateMetricValue(report.metrics.domPatch.perMutationUs.p95, "us", leaders ? leaders.domPatch : null, false)}</td>
                    <td data-label="Generated">
                      <div class="metric-value">${escapeHtml(report.generatedAt)}</div>
                      <span class="metric-note">HTML ${escapeHtml(path.basename(path.join(PERF_DIR, `latest-${entry.target.id}.html`)))}</span>
                    </td>
                  </tr>
                `;
              })
              .join("")}
          </tbody>
        </table>`}
      </section>

      ${comparisonEntries.length === 0 ? "" : `
      <section class="panel">
        <div class="panel-header">
          <div>
            <p class="eyebrow">Baseline deltas</p>
            <h2>Latest saved-baseline comparisons</h2>
          </div>
        </div>
        <table>
          <thead>
            <tr>
              <th>Framework</th>
              <th>Baseline</th>
              <th>SSR req/s</th>
              <th>SSR p95</th>
              <th>Bootstrap p95</th>
              <th>DOM patch p95</th>
            </tr>
          </thead>
          <tbody>
            ${comparisonEntries
              .map((entry) => `
                <tr>
                  <td data-label="Framework">
                    <div class="framework-name">${escapeHtml(entry.target.framework)}</div>
                    <div class="framework-meta">${escapeHtml(entry.target.id)}</div>
                  </td>
                  <td data-label="Baseline"><div class="metric-value">${escapeHtml(entry.report.comparison.baselineName)}</div></td>
                  <td data-label="SSR req/s">${renderComparisonCheckCell(getComparisonCheck(entry.report.comparison, "SSR req/s"))}</td>
                  <td data-label="SSR p95">${renderComparisonCheckCell(getComparisonCheck(entry.report.comparison, "SSR p95"))}</td>
                  <td data-label="Bootstrap p95">${renderComparisonCheckCell(getComparisonCheck(entry.report.comparison, "Bootstrap p95"))}</td>
                  <td data-label="DOM patch p95">${renderComparisonCheckCell(getComparisonCheck(entry.report.comparison, "DOM patch p95"))}</td>
                </tr>
              `)
              .join("")}
          </tbody>
        </table>
      </section>`}
    </main>
  </body>
</html>`;
}

function renderAggregateSummaryCard(label, value, caption, tone) {
  return `
    <article class="summary-card ${escapeHtml(tone)}">
      <span class="label">${escapeHtml(label)}</span>
      <strong class="value">${escapeHtml(value)}</strong>
      <p class="caption">${escapeHtml(caption)}</p>
    </article>
  `;
}

function renderAggregateMetricValue(value, unit, leaderValue, higherIsBetter) {
  const leaderNote = leaderValue === null || leaderValue === undefined
    ? ""
    : renderLeaderNote(value, leaderValue, higherIsBetter);

  return `
    <div class="metric-value">${formatDisplayValue(value, unit)}</div>
    ${leaderNote}
  `;
}

function renderLeaderNote(value, leaderValue, higherIsBetter) {
  if (!Number.isFinite(value) || !Number.isFinite(leaderValue)) {
    return "";
  }

  if (value === leaderValue) {
    return `<span class="pill">best</span>`;
  }

  if (higherIsBetter) {
    const shareOfLeader = leaderValue === 0 ? 0 : (value / leaderValue) * 100;
    return `<span class="metric-note">${escapeHtml(formatValue(roundNumber(shareOfLeader, 2)))}% of leader</span>`;
  }

  const ratio = leaderValue === 0 ? 0 : value / leaderValue;
  return `<span class="metric-note">${escapeHtml(formatValue(roundNumber(ratio, 2)))}x leader</span>`;
}

function getComparisonCheck(comparison, label) {
  return comparison.checks.find((check) => check.label === label) || null;
}

function renderComparisonCheckCell(check) {
  if (!check) {
    return '<span class="metric-note">n/a</span>';
  }

  const direction = check.improved ? "improved" : "regressed";
  const deltaPrefix = check.deltaPct > 0 ? "+" : "";

  return `
    <span class="status ${direction}">${deltaPrefix}${formatValue(check.deltaPct)}%</span>
    <span class="metric-note">${escapeHtml(formatValue(check.current))} vs ${escapeHtml(formatValue(check.baseline))} ${escapeHtml(check.unit)}</span>
  `;
}

function renderMetricPanel(title, rows) {
  return `
    <section class="panel">
      <div class="panel-header">
        <div>
          <p class="eyebrow">Details</p>
          <h2>${escapeHtml(title)}</h2>
        </div>
      </div>
      <table>
        <tbody>
          ${rows
            .map(
              ([label, value, unit]) => `
                <tr>
                  <th>${escapeHtml(label)}</th>
                  <td>${formatDisplayValue(value, unit)}</td>
                </tr>
              `
            )
            .join("")}
        </tbody>
      </table>
    </section>
  `;
}

function formatDisplayValue(value, unit) {
  const formattedValue = typeof value === "number" ? formatValue(value) : escapeHtml(String(value));
  return unit ? `${formattedValue} ${escapeHtml(unit)}` : formattedValue;
}

function formatValue(value) {
  const numericValue = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(numericValue)) {
    return escapeHtml(String(value));
  }

  return numericValue.toLocaleString("en-US", {
    maximumFractionDigits: 3,
  });
}

function escapeHtml(value) {
  return String(value)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/\"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function compareReports(current, baseline, baselineName) {
  return {
    baselineName,
    checks: [
      compareMetric(
        "SSR req/s",
        current.metrics.ssr.requestsPerSecond,
        baseline.metrics.ssr.requestsPerSecond,
        true,
        "req/s"
      ),
      compareMetric(
        "SSR p95",
        current.metrics.ssr.p95,
        baseline.metrics.ssr.p95,
        false,
        "ms"
      ),
      compareMetric(
        "Bootstrap p95",
        current.metrics.bootstrap.p95,
        baseline.metrics.bootstrap.p95,
        false,
        "ms"
      ),
      compareMetric(
        "DOM patch p95",
        current.metrics.domPatch.perMutationUs.p95,
        baseline.metrics.domPatch.perMutationUs.p95,
        false,
        "us"
      ),
    ],
  };
}

function compareMetric(label, current, baseline, higherIsBetter, unit) {
  const delta = current - baseline;
  const deltaPct = baseline === 0 ? 0 : (delta / baseline) * 100;
  const improved = higherIsBetter ? current >= baseline : current <= baseline;

  return {
    label,
    unit,
    current: roundNumber(current, 3),
    baseline: roundNumber(baseline, 3),
    delta: roundNumber(delta, 3),
    deltaPct: roundNumber(deltaPct, 2),
    improved,
  };
}

function printSummary(report, reportPaths, comparison) {
  console.log(`\nPerf report — ${report.target.framework} (${report.target.id})`);
  console.log(`  latest: ${path.relative(ROOT_DIR, reportPaths.latestPath)}`);
  console.log(`  report: ${path.relative(ROOT_DIR, reportPaths.reportPath)}`);
  console.log(`  latest html: ${path.relative(ROOT_DIR, reportPaths.latestHtmlPath)}`);
  console.log(`  html: ${path.relative(ROOT_DIR, reportPaths.reportHtmlPath)}`);
  console.log(`  latest matrix html: ${path.relative(ROOT_DIR, reportPaths.latestMatrixHtmlPath)}`);
  console.log(`  matrix html: ${path.relative(ROOT_DIR, reportPaths.matrixReportHtmlPath)}`);
  if (reportPaths.savedBaseline) {
    console.log(`  saved baseline: ${path.relative(ROOT_DIR, reportPaths.savedBaseline)}`);
  }

  console.log("\nMetrics");
  console.log(
    `  SSR        ${report.metrics.ssr.requestsPerSecond} req/s | p50 ${report.metrics.ssr.p50} ms | p95 ${report.metrics.ssr.p95} ms`
  );
  console.log(
    `  Bootstrap  p50 ${report.metrics.bootstrap.p50} ms | p95 ${report.metrics.bootstrap.p95} ms`
  );
  console.log(
    `  DOM patch  p50 ${report.metrics.domPatch.perMutationUs.p50} us | p95 ${report.metrics.domPatch.perMutationUs.p95} us`
  );

  if (!comparison) {
    return;
  }

  console.log(`\nComparison vs baseline '${comparison.baselineName}'`);
  for (const check of comparison.checks) {
    const direction = check.improved ? "improved" : "regressed";
    const sign = check.deltaPct > 0 ? "+" : "";
    console.log(
      `  ${check.label}: ${check.current} ${check.unit} vs ${check.baseline} ${check.unit} (${sign}${check.deltaPct}% ${direction})`
    );
  }
}

function printTargets() {
  console.log("Available perf targets:\n");
  for (const target of listTargets(ROOT_DIR)) {
    console.log(`  ${target.id}  ${target.framework}  ${target.label}`);
  }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
