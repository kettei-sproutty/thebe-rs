const path = require("node:path");

const GENERATED_CLIENT_COMMAND_ID = "thebe.openGeneratedClientMirror";
const GENERATED_TYPES_COMMAND_ID = "thebe.openGeneratedTypesMirror";

function resolveGeneratedClientMirrorPath({ documentPath, workspaceFolders }) {
  return resolveGeneratedRouteArtifactPath({
    documentPath,
    workspaceFolders,
    artifactSegments: [".thebe", "client", "routes"],
  });
}

function resolveGeneratedTypesMirrorPath({ documentPath, workspaceFolders }) {
  return resolveGeneratedRouteArtifactPath({
    documentPath,
    workspaceFolders,
    artifactSegments: [".thebe", "types", "routes"],
  });
}

function resolveGeneratedRouteArtifactPath({ documentPath, workspaceFolders, artifactSegments }) {
  if (typeof documentPath !== "string" || documentPath.length === 0) {
    return null;
  }

  for (const workspaceFolder of workspaceFolders ?? []) {
    if (typeof workspaceFolder !== "string" || workspaceFolder.length === 0) {
      continue;
    }

    const routesDir = path.join(workspaceFolder, "src", "routes");
    const relativePath = path.relative(routesDir, documentPath);
    if (
      relativePath.length === 0
      || relativePath.startsWith("..")
      || path.isAbsolute(relativePath)
      || path.extname(relativePath) !== ".trs"
      || path.basename(relativePath).startsWith("_")
    ) {
      continue;
    }

    return path.join(workspaceFolder, ...artifactSegments, relativePath.slice(0, -4) + ".ts");
  }

  return null;
}

function selectGeneratedTypesLocation({ locations, mirrorPath }) {
  return selectGeneratedArtifactLocation({ locations, targetPath: mirrorPath });
}

function selectGeneratedClientLocation({ locations, mirrorPath }) {
  return selectGeneratedArtifactLocation({ locations, targetPath: mirrorPath });
}

function selectGeneratedArtifactLocation({ locations, targetPath }) {
  if (!Array.isArray(locations) || typeof targetPath !== "string" || targetPath.length === 0) {
    return null;
  }

  const normalizedTargetPath = path.normalize(targetPath);
  for (const location of locations) {
    const resolved = resolveDefinitionLocation(location);
    if (!resolved || path.normalize(resolved.uri.fsPath) !== normalizedTargetPath) {
      continue;
    }

    return resolved;
  }

  return null;
}

function resolveDefinitionLocation(location) {
  if (!location || typeof location !== "object") {
    return null;
  }

  if (location.targetUri && typeof location.targetUri.fsPath === "string") {
    return {
      uri: location.targetUri,
      range: location.targetSelectionRange ?? location.targetRange ?? null,
    };
  }

  if (location.uri && typeof location.uri.fsPath === "string") {
    return {
      uri: location.uri,
      range: location.range ?? null,
    };
  }

  return null;
}

module.exports = {
  GENERATED_CLIENT_COMMAND_ID,
  GENERATED_TYPES_COMMAND_ID,
  resolveGeneratedClientMirrorPath,
  resolveGeneratedTypesMirrorPath,
  selectGeneratedClientLocation,
  selectGeneratedTypesLocation,
};
