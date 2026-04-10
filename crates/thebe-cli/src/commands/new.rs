use anyhow::Context;
use std::path::Path;

/// Scaffold a new minimal Thebe project.
pub fn run(name: &str) -> anyhow::Result<()> {
    let project_dir = Path::new(name);
    anyhow::ensure!(
        !project_dir.exists(),
        "directory `{name}` already exists"
    );

    std::fs::create_dir_all(project_dir.join("src").join("routes"))
        .context("failed to create project directories")?;

    std::fs::write(project_dir.join("Cargo.toml"), cargo_toml(name))
        .context("failed to write Cargo.toml")?;

    std::fs::write(
        project_dir.join(".gitignore"),
        "/target\n# thebe-generated files\nsrc/main.rs\nsrc/**/*.rs\n",
    )
    .context("failed to write .gitignore")?;

    std::fs::write(
        project_dir.join("src").join("routes").join("index.trs"),
        EXAMPLE_TRS,
    )
    .context("failed to write src/routes/index.trs")?;

    println!("created `{name}`");
    println!("  run:  cd {name} && thebe dev");
    Ok(())
}

fn cargo_toml(name: &str) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
axum = "0.8"
tokio = {{ version = "1", features = ["full"] }}
minijinja = "2"
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
"#
    )
}

const EXAMPLE_TRS: &str = r#"<script setup>
struct Props {
    title: String,
    subtitle: String,
}

#[thebe::get]
pub fn handler() -> Props {
    Props {
        title: "Hello from Thebe!".to_string(),
        subtitle: "A compiler-driven Rust web framework.".to_string(),
    }
}
</script>

<h1>{{ title }}</h1>
<p>{{ subtitle }}</p>
"#;
