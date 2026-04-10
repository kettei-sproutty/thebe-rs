use anyhow::Context;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Run `thebe dev`: parse all `.trs` route files, emit generated Rust sources,
/// and hand off to `cargo run`.
pub fn run() -> anyhow::Result<()> {
    let project_root = find_project_root()?;
    println!("thebe: project root at {}", project_root.display());

    let routes_dir = project_root.join("src").join("routes");
    anyhow::ensure!(
        routes_dir.exists(),
        "no `src/routes/` directory found — create your route `.trs` files there"
    );

    let trs_files = collect_trs_files(&routes_dir)?;
    anyhow::ensure!(
        !trs_files.is_empty(),
        "no `.trs` files found in `src/routes/`"
    );

    let mut route_entries: Vec<thebe_codegen::RouteEntry> = Vec::new();

    for trs_path in &trs_files {
        let source = std::fs::read_to_string(trs_path)
            .with_context(|| format!("failed to read {}", trs_path.display()))?;

        let blocks = thebe_parser::parse_sfc(&source)
            .with_context(|| format!("parse error in {}", trs_path.display()))?;

        let route_path = file_to_route_path(trs_path, &routes_dir);
        let mod_name = file_to_mod_name(trs_path);

        let generated = thebe_codegen::generate_route(&blocks, &route_path)
            .with_context(|| format!("codegen error for {}", trs_path.display()))?;

        let rs_path = trs_path.with_extension("rs");
        std::fs::write(&rs_path, &generated)
            .with_context(|| format!("failed to write {}", rs_path.display()))?;

        println!(
            "thebe: {} \u{2192} {}",
            trs_path.display(),
            rs_path.display()
        );

        route_entries.push(thebe_codegen::RouteEntry {
            mod_name,
            route_path,
        });
    }

    let main_rs = thebe_codegen::generate_main(&route_entries);
    let main_path = project_root.join("src").join("main.rs");
    std::fs::write(&main_path, &main_rs)
        .context("failed to write src/main.rs")?;
    println!("thebe: generated src/main.rs");

    println!("thebe: running `cargo run`\u{2026}");
    let status = std::process::Command::new("cargo")
        .arg("run")
        .current_dir(&project_root)
        .status()
        .context("failed to invoke `cargo run`")?;

    std::process::exit(status.code().unwrap_or(1));
}

/// Walk up from the current directory to find the nearest `Cargo.toml`.
fn find_project_root() -> anyhow::Result<PathBuf> {
    let mut dir = std::env::current_dir().context("failed to get current directory")?;
    loop {
        if dir.join("Cargo.toml").exists() {
            return Ok(dir);
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => anyhow::bail!(
                "could not find a `Cargo.toml` in the current directory or any parent"
            ),
        }
    }
}

/// Recursively collect `.trs` files from `dir`.
fn collect_trs_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(dir).min_depth(1) {
        let entry = entry.context("failed to read directory entry")?;
        if entry.file_type().is_file()
            && entry.path().extension().is_some_and(|e| e == "trs")
        {
            files.push(entry.into_path());
        }
    }
    Ok(files)
}

/// Derive the Axum route path from the file's position under `routes_dir`.
///
/// * `src/routes/index.trs` → `/`
/// * `src/routes/about.trs` → `/about`
/// * `src/routes/blog/[slug].trs` → `/blog/:slug`
fn file_to_route_path(trs_path: &Path, routes_dir: &Path) -> String {
    let rel = trs_path
        .strip_prefix(routes_dir)
        .unwrap_or(trs_path);
    let stem = rel
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let parent = rel.parent().unwrap_or(Path::new(""));

    let mut segments: Vec<String> = parent
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();

    if stem != "index" {
        segments.push(stem.into_owned());
    }

    if segments.is_empty() {
        "/".to_owned()
    } else {
        let path = segments.join("/");
        // Convert file-system dynamic segments `[param]` → `:param`.
        let path = path.replace('[', ":").replace(']', "");
        format!("/{path}")
    }
}

/// Derive the Rust module name from the `.trs` filename stem.
fn file_to_mod_name(trs_path: &Path) -> String {
    trs_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .replace('-', "_")
}
