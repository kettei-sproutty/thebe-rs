const fs = require("node:fs");
const path = require("node:path");

async function main() {
  const typescriptLibraryPath = process.argv[2];
  if (typeof typescriptLibraryPath !== "string" || typescriptLibraryPath.length === 0) {
    throw new Error("Missing TypeScript library path");
  }

  const ts = require(typescriptLibraryPath);
  const request = await readJsonFromStdin();
  const languageService = createLanguageService(ts, request);

  if (request.command === "completions") {
    const completions = languageService.getCompletionsAtPosition(
      request.filePath,
      request.offset,
      {
        includeCompletionsWithInsertText: true,
        triggerCharacter: typeof request.triggerCharacter === "string"
          ? request.triggerCharacter
          : undefined,
      },
    );
    const optionalReplacementSpan = completions?.optionalReplacementSpan;
    writeJsonToStdout((completions?.entries ?? []).map((entry) => ({
      name: entry.name,
      kind: entry.kind,
      sortText: entry.sortText,
      insertText: entry.insertText ?? null,
      replacementSpan: entry.replacementSpan ?? optionalReplacementSpan ?? null,
    })));
    return;
  }

  if (request.command === "diagnostics") {
    const diagnostics = [
      ...languageService.getSyntacticDiagnostics(request.filePath),
      ...languageService.getSemanticDiagnostics(request.filePath),
      ...languageService.getSuggestionDiagnostics(request.filePath),
    ];
    writeJsonToStdout(diagnostics.map((diagnostic) => ({
      start: diagnostic.start ?? 0,
      length: diagnostic.length ?? 0,
      code: diagnostic.code,
      category: String(ts.DiagnosticCategory[diagnostic.category] ?? "message").toLowerCase(),
      messageText: diagnostic.messageText,
      reportsUnnecessary: diagnostic.reportsUnnecessary === true,
      reportsDeprecated: diagnostic.reportsDeprecated === true,
      source: "ts",
    })));
    return;
  }

  throw new Error(`Unsupported bridge command: ${request.command}`);
}

function createLanguageService(ts, request) {
  const filePath = request.filePath;
  const content = typeof request.content === "string" ? request.content : "";
  const currentDirectory = path.dirname(filePath);
  const compilerOptions = {
    allowJs: true,
    module: ts.ModuleKind.ESNext,
    strict: true,
    target: ts.ScriptTarget.ES2022,
  };
  const snapshots = new Map([
    [filePath, ts.ScriptSnapshot.fromString(content)],
  ]);
  const versions = new Map([[filePath, "1"]]);

  const host = {
    directoryExists: fs.existsSync,
    fileExists: fs.existsSync,
    getCompilationSettings: () => compilerOptions,
    getCurrentDirectory: () => currentDirectory,
    getDefaultLibFileName: (options) => ts.getDefaultLibFilePath(options),
    getDirectories: (dirPath) => {
      try {
        return fs.readdirSync(dirPath, { withFileTypes: true })
          .filter((entry) => entry.isDirectory())
          .map((entry) => path.join(dirPath, entry.name));
      } catch {
        return [];
      }
    },
    getNewLine: () => "\n",
    getScriptFileNames: () => [filePath],
    getScriptKind: () => ts.ScriptKind.TS,
    getScriptSnapshot: (name) => {
      if (snapshots.has(name)) {
        return snapshots.get(name);
      }

      try {
        if (!fs.existsSync(name)) {
          return undefined;
        }
        return ts.ScriptSnapshot.fromString(fs.readFileSync(name, "utf8"));
      } catch {
        return undefined;
      }
    },
    getScriptVersion: (name) => versions.get(name) ?? "0",
    readDirectory: ts.sys.readDirectory,
    readFile: (name) => {
      try {
        return fs.readFileSync(name, "utf8");
      } catch {
        return undefined;
      }
    },
    realpath: (name) => {
      try {
        return fs.realpathSync(name);
      } catch {
        return name;
      }
    },
    useCaseSensitiveFileNames: () => ts.sys.useCaseSensitiveFileNames,
  };

  return ts.createLanguageService(host, ts.createDocumentRegistry());
}

async function readJsonFromStdin() {
  const chunks = [];
  for await (const chunk of process.stdin) {
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function writeJsonToStdout(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

main().catch((error) => {
  process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
  process.exitCode = 1;
});
