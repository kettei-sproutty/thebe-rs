use anyhow::Context;
use reqwest::blocking::Client;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Download and optionally run tailwind CLI if configured
pub fn ensure_and_run(
    project_root: &Path,
    tailwind_config: &thebe_project::config::TailwindConfig,
) -> anyhow::Result<()> {
    let binary_path = ensure_tailwind_binary()?;

    let input_path = project_root.join(&tailwind_config.input);
    let output_path = project_root.join(&tailwind_config.output);

    println!("thebe: running tailwindcss on {}", input_path.display());

    let status = Command::new(binary_path)
        .arg("-i")
        .arg(&input_path)
        .arg("-o")
        .arg(&output_path)
        .current_dir(project_root)
        .status()
        .context("failed to execute tailwindcss format")?;

    if !status.success() {
        anyhow::bail!("tailwindcss failed with status {:?}", status.code());
    }

    Ok(())
}

fn ensure_tailwind_binary() -> anyhow::Result<PathBuf> {
    let proj_dirs = directories::ProjectDirs::from("com", "thebe", "thebe-cli")
        .context("failed to determine thebe cache directory")?;
    let cache_dir = proj_dirs.cache_dir();
    let tools_dir = cache_dir.join("tools");

    if !tools_dir.exists() {
        fs::create_dir_all(&tools_dir).context("failed to create tools cache dir")?;
    }

    let binary_name = get_binary_name()?;
    let binary_path = tools_dir.join(binary_name);

    if binary_path.exists() {
        return Ok(binary_path);
    }

    println!("thebe: downloading tailwindcss binary ({})…", binary_name);

    let url = format!(
        "https://github.com/tailwindlabs/tailwindcss/releases/latest/download/{}",
        binary_name
    );

    let client = Client::builder()
        .user_agent("thebe-cli")
        .build()?;
    let mut response = client.get(&url).send()?.error_for_status()?;

    let mut file = fs::File::create(&binary_path).context("failed to create tailwind binary file")?;
    response.copy_to(&mut file)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&binary_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&binary_path, perms)?;
    }

    Ok(binary_path)
}

fn get_binary_name() -> anyhow::Result<&'static str> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    match (os, arch) {
        ("macos", "aarch64") => Ok("tailwindcss-macos-arm64"),
        ("macos", "x86_64") => Ok("tailwindcss-macos-x64"),
        ("linux", "aarch64") => Ok("tailwindcss-linux-arm64"),
        ("linux", "arm") => Ok("tailwindcss-linux-armv7"),
        ("linux", "x86_64") => Ok("tailwindcss-linux-x64"),
        ("windows", "x86_64") => Ok("tailwindcss-windows-x64.exe"),
        ("windows", "aarch64") => Ok("tailwindcss-windows-arm64.exe"),
        _ => anyhow::bail!("unsupported OS or architecture for tailwindcss standalone: {}-{}", os, arch),
    }
}
