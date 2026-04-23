const path = require("path");

function createTargets(rootDir) {
  const exampleDir = path.join(rootDir, "examples", "counter-app");
  const nextDir = path.join(rootDir, "benchmarks", "fixtures", "next-counter");
  const nuxtDir = path.join(rootDir, "benchmarks", "fixtures", "nuxt-counter");
  const svelteKitDir = path.join(rootDir, "benchmarks", "fixtures", "sveltekit-counter");
  const solidStartDir = path.join(rootDir, "benchmarks", "fixtures", "solidstart-counter");
  const binaryName = process.platform === "win32" ? "counter-app.exe" : "counter-app";

  return {
    "thebe-counter": {
      id: "thebe-counter",
      framework: "thebe",
      label: "Thebe counter app",
      url: "http://127.0.0.1:3000/",
      rootDir: exampleDir,
      build: {
        command: "cargo",
        args: [
          "run",
          "--manifest-path",
          path.join(rootDir, "Cargo.toml"),
          "-p",
          "thebe-cli",
          "--",
          "build",
        ],
        cwd: exampleDir,
      },
      start: {
        command: path.join(exampleDir, "target", "release", binaryName),
        args: [],
        cwd: exampleDir,
      },
    },
    "next-counter": {
      id: "next-counter",
      framework: "nextjs",
      label: "Next.js App Router counter",
      url: "http://127.0.0.1:3001/",
      rootDir: nextDir,
      prepare: {
        command: "npm",
        args: ["install"],
        cwd: nextDir,
        marker: path.join(nextDir, "node_modules"),
      },
      build: {
        command: process.execPath,
        args: [
          path.join(nextDir, "node_modules", "next", "dist", "bin", "next"),
          "build",
        ],
        cwd: nextDir,
      },
      start: {
        command: process.execPath,
        args: [
          path.join(nextDir, "node_modules", "next", "dist", "bin", "next"),
          "start",
          "-p",
          "3001",
        ],
        cwd: nextDir,
      },
    },
    "nuxt-counter": {
      id: "nuxt-counter",
      framework: "nuxt",
      label: "Nuxt counter",
      url: "http://127.0.0.1:3002/",
      rootDir: nuxtDir,
      prepare: {
        command: "npm",
        args: ["install"],
        cwd: nuxtDir,
        marker: path.join(nuxtDir, "node_modules"),
      },
      build: {
        command: process.execPath,
        args: [
          path.join(nuxtDir, "node_modules", "nuxt", "bin", "nuxt.mjs"),
          "build",
        ],
        cwd: nuxtDir,
        env: {
          NITRO_PRESET: "node-server",
          NUXT_TELEMETRY_DISABLED: "1",
        },
      },
      start: {
        command: process.execPath,
        args: [path.join(nuxtDir, ".output", "server", "index.mjs")],
        cwd: nuxtDir,
        env: {
          HOST: "127.0.0.1",
          NUXT_TELEMETRY_DISABLED: "1",
          PORT: "3002",
        },
      },
    },
    "sveltekit-counter": {
      id: "sveltekit-counter",
      framework: "sveltekit",
      label: "SvelteKit counter",
      url: "http://127.0.0.1:3003/",
      rootDir: svelteKitDir,
      prepare: {
        command: "npm",
        args: ["install"],
        cwd: svelteKitDir,
        marker: path.join(svelteKitDir, "node_modules"),
      },
      build: {
        command: process.execPath,
        args: [
          path.join(svelteKitDir, "node_modules", "vite", "bin", "vite.js"),
          "build",
        ],
        cwd: svelteKitDir,
      },
      start: {
        command: process.execPath,
        args: ["build"],
        cwd: svelteKitDir,
        env: {
          HOST: "127.0.0.1",
          PORT: "3003",
          ORIGIN: "http://127.0.0.1:3003",
        },
      },
    },
    "solidstart-counter": {
      id: "solidstart-counter",
      framework: "solidstart",
      label: "SolidStart counter",
      url: "http://127.0.0.1:3004/",
      rootDir: solidStartDir,
      prepare: {
        command: "npm",
        args: ["install"],
        cwd: solidStartDir,
        marker: path.join(solidStartDir, "node_modules"),
      },
      build: {
        command: process.execPath,
        args: [
          path.join(solidStartDir, "node_modules", "vinxi", "bin", "cli.mjs"),
          "build",
          "--config",
          "app.config.ts",
        ],
        cwd: solidStartDir,
      },
      start: {
        command: process.execPath,
        args: [
          path.join(solidStartDir, "node_modules", "vinxi", "bin", "cli.mjs"),
          "start",
          "--config",
          "app.config.ts",
          "--host",
          "127.0.0.1",
          "--port",
          "3004",
        ],
        cwd: solidStartDir,
      },
    },
  };
}

function listTargets(rootDir) {
  return Object.values(createTargets(rootDir));
}

function getTarget(rootDir, targetId) {
  const targets = createTargets(rootDir);
  const target = targets[targetId];

  if (!target) {
    const available = Object.keys(targets).sort().join(", ");
    throw new Error(`Unknown target '${targetId}'. Available targets: ${available}`);
  }

  return target;
}

module.exports = {
  BENCHMARK_BOOTSTRAP_MEASURE: "framework-bench:bootstrap",
  BENCHMARK_GLOBAL: "__frameworkBench",
  createTargets,
  getTarget,
  listTargets,
};
