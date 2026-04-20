use anyhow::Context;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

/// Run `thebe dev`: refresh generated `.thebe` artifacts and hand off to `cargo run`.
pub fn run(watch: bool) -> anyhow::Result<()> {
  let project_root = find_project_root()?;
  println!("thebe: project root at {}", project_root.display());

  let config = thebe_project::ThebeConfig::load(&project_root)
    .context("failed to load thebe.toml")?;

  if let Some(pre_build) = config.get_hook("pre_build") {
    println!("thebe: running pre_build hook: {}", pre_build);
    let status = Command::new(if cfg!(target_os = "windows") { "cmd" } else { "sh" })
      .arg(if cfg!(target_os = "windows") { "/C" } else { "-c" })
      .arg(pre_build)
      .current_dir(&project_root)
      .status()
      .context("failed to execute pre_build hook")?;

    if !status.success() {
      anyhow::bail!("pre_build hook failed with status {:?}", status.code());
    }
  }

  if let Some(tailwind_config) = &config.tailwind {
    crate::tailwind::ensure_and_run(&project_root, tailwind_config)?;
  }

  run_codegen(&project_root)?;

  if watch {
    run_watch(&project_root)
  } else {
    println!("thebe: running `cargo run`…");
    let status = Command::new("cargo")
      .arg("run")
      .current_dir(&project_root)
      .status()
      .context("failed to invoke `cargo run`")?;
    std::process::exit(status.code().unwrap_or(1));
  }
}

/// Run `thebe check`: validate project files and emit `.thebe/diagnostics.json`.
pub fn check() -> anyhow::Result<()> {
  let project_root = find_project_root()?;
  println!("thebe: project root at {}", project_root.display());

  let diagnostics = thebe_project::check_project(&project_root)?;
  if diagnostics.is_empty() {
    println!("thebe: no diagnostics");
    return Ok(());
  }

  anyhow::bail!("found {} diagnostic(s)", diagnostics.diagnostics.len())
}

fn run_codegen(project_root: &Path) -> anyhow::Result<()> {
  thebe_project::generate_project(project_root)?;
  println!(
    "thebe: refreshed {}",
    project_root.join(thebe_project::THEBE_DIR).display()
  );
  Ok(())
}

fn spawn_server(project_root: &Path) -> anyhow::Result<Child> {
  println!("thebe: running `cargo run`…");
  Command::new("cargo")
    .arg("run")
    .current_dir(project_root)
    .spawn()
    .context("failed to spawn `cargo run`")
}

fn kill_server(child: &mut Child) {
  #[cfg(unix)]
  {
    let _ = Command::new("pkill")
      .args(["-TERM", "-P", &child.id().to_string()])
      .status();
    std::thread::sleep(Duration::from_millis(200));
  }
  let _ = child.kill();
  let _ = child.wait();
}

fn run_watch(project_root: &Path) -> anyhow::Result<()> {
  use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
  use std::sync::mpsc;

  let routes_dir = project_root.join("src").join("routes");
  println!(
    "thebe: watch mode — watching {} for changes…",
    routes_dir.display()
  );

  let mut child = spawn_server(project_root)?;

  let (tx, rx) = mpsc::channel();
  let mut watcher = RecommendedWatcher::new(
    move |res| {
      let _ = tx.send(res);
    },
    Config::default(),
  )
  .context("failed to create file watcher")?;

  watcher
    .watch(&routes_dir, RecursiveMode::Recursive)
    .context("failed to watch routes directory")?;
  watcher
    .watch(project_root, RecursiveMode::NonRecursive)
    .context("failed to watch project root")?;

  while let Ok(first) = rx.recv() {
    let codegen_changed = is_codegen_event(&first, project_root);

    let mut any_codegen_change = codegen_changed;
    loop {
      match rx.recv_timeout(Duration::from_millis(150)) {
        Ok(res) => {
          if is_codegen_event(&res, project_root) {
            any_codegen_change = true;
          }
        }
        Err(mpsc::RecvTimeoutError::Timeout) => break,
        Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
      }
    }

    if !any_codegen_change {
      continue;
    }

    println!("thebe: change detected — rebuilding…");
    kill_server(&mut child);

    match thebe_project::ThebeConfig::load(project_root) {
      Ok(config) => {
        if let Some(on_change) = config.get_hook("on_change") {
          println!("thebe: running on_change hook: {}", on_change);
          let status = Command::new(if cfg!(target_os = "windows") { "cmd" } else { "sh" })
            .arg(if cfg!(target_os = "windows") { "/C" } else { "-c" })
            .arg(on_change)
            .current_dir(project_root)
            .status();

          if let Err(e) = status {
            println!("thebe: \u{1b}[31mon_change hook failed\u{1b}[0m: {}", e);
          } else if let Ok(status) = status {
            if !status.success() {
              println!("thebe: \u{1b}[31mon_change hook failed with status {:?}\u{1b}[0m", status.code());
            }
          }
        }

        if let Some(tailwind_config) = config.tailwind {
          if let Err(e) = crate::tailwind::ensure_and_run(project_root, &tailwind_config) {
            println!("thebe: \u{1b}[31mtailwind error\u{1b}[0m: {:?}", e);
          }
        }
      }
      Err(err) => {
        println!("thebe: \u{1b}[31mfailed to load thebe.toml configuration\u{1b}[0m: {}", err);
      }
    }

    match run_codegen(project_root) {
      Err(err) => eprintln!("thebe: codegen error: {err:#}"),
      Ok(()) => match spawn_server(project_root) {
        Ok(new_child) => child = new_child,
        Err(err) => eprintln!("thebe: failed to restart server: {err:#}"),
      },
    }
  }

  Ok(())
}

fn is_codegen_event(res: &notify::Result<notify::Event>, project_root: &Path) -> bool {
  use notify::event::EventKind;

  match res {
    Ok(event) => {
      matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
      ) && event.paths.iter().any(|path| {
        path.extension().is_some_and(|ext| ext == "trs")
          || is_app_html_path(path, project_root)
          || is_cargo_toml_path(path, project_root)
          || is_thebe_toml_path(path, project_root)
      })
    }
    Err(_) => false,
  }
}

fn is_app_html_path(path: &Path, project_root: &Path) -> bool {
  path
    .file_name()
    .is_some_and(|file_name| file_name == "app.html")
    && path.parent().is_some_and(|parent| parent == project_root)
}

fn is_cargo_toml_path(path: &Path, project_root: &Path) -> bool {
  path
    .file_name()
    .is_some_and(|file_name| file_name == "Cargo.toml")
    && path.parent().is_some_and(|parent| parent == project_root)
}

fn is_thebe_toml_path(path: &Path, project_root: &Path) -> bool {
  path
    .file_name()
    .is_some_and(|file_name| file_name == "thebe.toml")
    && path.parent().is_some_and(|parent| parent == project_root)
}

fn find_project_root() -> anyhow::Result<PathBuf> {
  let current_dir = std::env::current_dir().context("failed to get current directory")?;
  thebe_project::find_project_root_from(&current_dir)
}
