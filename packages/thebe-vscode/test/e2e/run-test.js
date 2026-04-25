const path = require("node:path");
const { runTests } = require("@vscode/test-electron");

async function main() {
  try {
    await runTests({
      extensionDevelopmentPath: path.resolve(__dirname, "..", ".."),
      extensionTestsPath: path.resolve(__dirname, "suite", "index.js"),
      launchArgs: [path.resolve(__dirname, "fixture-workspace")],
    });
  } catch (error) {
    console.error(error);
    console.error("Failed to run Thebe VS Code e2e tests");
    process.exit(1);
  }
}

main();
