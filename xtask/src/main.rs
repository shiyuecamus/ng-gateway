use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "Task runner for ng-gateway project", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build drivers and plugins, then deploy them
    Build {
        /// Build profile (debug/release)
        #[arg(short, long, default_value = "debug")]
        profile: String,

        /// Skip building Web UI assets.
        ///
        /// Best practice for this repository:
        /// - `cargo xtask build` should be enough to run the gateway from source.
        /// - Therefore UI build is ON by default, and you can opt-out with `--without-ui`.
        ///
        /// Use cases for `--without-ui`:
        /// - Minimal backend-only development
        /// - Environments without Node/pnpm
        #[arg(long, default_value_t = false)]
        without_ui: bool,

        /// UI app name under `ng-gateway-ui/apps/`, e.g. `web-antd`.
        #[arg(long, default_value = "web-antd")]
        ui_app: String,

        /// Frontend monorepo root directory (contains pnpm-workspace.yaml).
        #[arg(long, default_value = "ng-gateway-ui")]
        ui_root: PathBuf,

        /// Existing dist directory to pack, e.g. `ng-gateway-ui/apps/web-antd/dist`.
        ///
        /// If not set, xtask will run pnpm build to generate it.
        #[arg(long)]
        ui_dist_dir: Option<PathBuf>,

        /// Output zip path for embedding.
        #[arg(long, default_value = "ng-gateway-web/ui-dist.zip")]
        ui_out_zip: PathBuf,

        /// Run `pnpm install --frozen-lockfile` before UI build (when `--ui-dist-dir` is not set).
        #[arg(long, default_value_t = true)]
        ui_install: bool,

        /// Only build the gateway binary (skip drivers/plugins discovery and deployment).
        ///
        /// This is intended for packaging scenarios (e.g. Homebrew).
        #[arg(long, default_value_t = false)]
        bin_only: bool,

        /// Skip deploying built drivers/plugins into `drivers/builtin` and `plugins/builtin`.
        #[arg(long, default_value_t = false)]
        no_deploy: bool,

        /// Additional cargo build arguments
        #[arg(last = true)]
        cargo_args: Vec<String>,
    },

    /// Deploy built drivers and plugins
    Deploy {
        /// Build profile to deploy from (debug/release)
        #[arg(short, long, default_value = "debug")]
        profile: String,
    },

    /// Clean build artifacts and deployed libraries
    Clean {
        /// Also remove deployed drivers and plugins
        #[arg(long)]
        all: bool,
    },
}

/// Arguments for the `xtask build` command.
///
/// Clippy note:
/// This groups build-related parameters to avoid a wide function signature and keep
/// the call site readable.
struct BuildCommandArgs<'a> {
    /// Build profile name (e.g. `debug`, `release`).
    profile: &'a str,
    /// Whether to build and package the Web UI (`dist` + `ui-dist.zip`).
    build_ui: bool,
    /// UI app name under `ng-gateway-ui/apps/`, e.g. `web-antd`.
    ui_app: &'a str,
    /// Frontend monorepo root directory (contains pnpm-workspace.yaml).
    ui_root: &'a Path,
    /// Existing dist directory to pack. If not set, xtask will build it with pnpm.
    ui_dist_dir: Option<&'a PathBuf>,
    /// Output zip path for embedding.
    ui_out_zip: &'a Path,
    /// Whether to run `pnpm install --frozen-lockfile` before building UI.
    ui_install: bool,
    /// Only build the gateway binary.
    bin_only: bool,
    /// Skip deploying built drivers/plugins into builtin directories.
    no_deploy: bool,
    /// Extra cargo args forwarded to `cargo build`.
    cargo_args: &'a [String],
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Build {
            profile,
            without_ui,
            ui_app,
            ui_root,
            ui_dist_dir,
            ui_out_zip,
            ui_install,
            bin_only,
            no_deploy,
            cargo_args,
        } => build_command(BuildCommandArgs {
            profile: &profile,
            build_ui: !without_ui,
            ui_app: &ui_app,
            ui_root: &ui_root,
            ui_dist_dir: ui_dist_dir.as_ref(),
            ui_out_zip: &ui_out_zip,
            ui_install,
            bin_only,
            no_deploy,
            cargo_args: &cargo_args,
        }),
        Commands::Deploy { profile } => deploy_command(&profile, None),
        Commands::Clean { all } => clean_command(all),
    };

    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("❌ Error: {:#}", e);
            ExitCode::FAILURE
        }
    }
}

/// Return the cargo-like command to use for building workspace crates.
///
/// # Why this exists
/// For multi-arch packaging, CI often uses `cross` as a drop-in replacement for `cargo`.
/// This helper allows overriding the build command without changing xtask logic.
fn cargo_build_program() -> String {
    if let Ok(v) = env::var("XTASK_CARGO") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    match env::var("CARGO") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => "cargo".to_string(),
    }
}

/// Extract the `--target <triple>` (or `--target=<triple>`) value from cargo args.
///
/// This is needed because when `--target` is specified, Cargo places artifacts under:
/// `target/<triple>/<profile>/...` instead of `target/<profile>/...`.
fn extract_target_triple(cargo_args: &[String]) -> Option<String> {
    let mut iter = cargo_args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--target" {
            if let Some(val) = iter.next() {
                return Some(val.to_string());
            }
        } else if let Some(rest) = arg.strip_prefix("--target=") {
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// Build UI (optional) and pack dist as a zip for embedding.
fn ui_build_and_pack(
    app: &str,
    ui_root: &Path,
    dist_dir: Option<&PathBuf>,
    out_zip: &Path,
    install: bool,
) -> Result<()> {
    let workspace_root = workspace_root()?;

    let ui_root = if ui_root.is_absolute() {
        ui_root.to_path_buf()
    } else {
        workspace_root.join(ui_root)
    };

    let (dist_dir, dist_dir_is_explicit) = match dist_dir {
        Some(p) => (
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                workspace_root.join(p)
            },
            true,
        ),
        None => (ui_root.join("apps").join(app).join("dist"), false),
    };

    // Policy:
    // - If the caller explicitly provides `dist_dir`, we treat it as a prebuilt artifact and do NOT run pnpm.
    // - Otherwise, we always build UI. Skipping UI should be done via `--without-ui` at a higher level.
    if dist_dir_is_explicit {
        if !(dist_dir.exists() && dist_dir.is_dir()) {
            anyhow::bail!("Explicit UI dist dir not found: {}", dist_dir.display());
        }
        println!("📦 Using explicit dist dir: {}", dist_dir.display());
    } else {
        println!("🔨 Building UI app: {app}");
        if install {
            println!("📥 pnpm install (frozen lockfile)...");
            let status = Command::new("pnpm")
                .current_dir(&ui_root)
                .arg("install")
                .arg("--frozen-lockfile")
                .status()
                .context("Failed to execute pnpm install")?;
            if !status.success() {
                anyhow::bail!("pnpm install failed");
            }
        }

        let filter = format!("@vben/{app}");
        println!("🏗️  pnpm build --filter={filter}");
        let status = Command::new("pnpm")
            .current_dir(&ui_root)
            .arg("run")
            .arg("build")
            .arg("--filter")
            .arg(filter)
            .status()
            .context("Failed to execute pnpm build")?;
        if !status.success() {
            anyhow::bail!("pnpm build failed");
        }

        if !(dist_dir.exists() && dist_dir.is_dir()) {
            anyhow::bail!(
                "UI build finished but dist directory not found: {}",
                dist_dir.display()
            );
        }
    }

    // Ensure output directory exists
    if let Some(parent) = out_zip.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create output dir: {}", parent.display()))?;
    }

    println!("🗜️  Packing dist -> {}", out_zip.display());
    pack_dir_to_zip(&dist_dir, out_zip)?;
    println!("✅ UI packed successfully");

    Ok(())
}

/// Pack a directory into a zip file (deterministic ordering).
fn pack_dir_to_zip(dir: &Path, out_zip: &Path) -> Result<()> {
    if !dir.exists() || !dir.is_dir() {
        anyhow::bail!("dist dir not found: {}", dir.display());
    }

    let mut entries = Vec::<PathBuf>::new();
    collect_files_recursively(dir, &mut entries)?;
    entries.sort();

    let file = fs::File::create(out_zip)
        .with_context(|| format!("Failed to create zip file: {}", out_zip.display()))?;

    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);

    for path in entries {
        let rel = path
            .strip_prefix(dir)
            .with_context(|| format!("Failed to compute relative path: {}", path.display()))?;
        let name = rel.to_string_lossy().replace('\\', "/");
        let bytes = fs::read(&path)
            .with_context(|| format!("Failed to read file for zip: {}", path.display()))?;

        zip.start_file(name, options)?;
        zip.write_all(&bytes)?;
    }

    zip.finish()?;
    Ok(())
}

fn collect_files_recursively(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries =
        fs::read_dir(dir).with_context(|| format!("Failed to read dir: {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let ty = entry.file_type()?;
        if ty.is_dir() {
            collect_files_recursively(&path, out)?;
        } else if ty.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

/// Build drivers and plugins, then deploy them
fn build_command(args: BuildCommandArgs<'_>) -> Result<()> {
    println!("🔍 Discovering drivers and plugins...");
    println!();

    let workspace_root = workspace_root()?;

    if args.build_ui {
        println!("🎨 Building UI assets...");
        ui_build_and_pack(
            args.ui_app,
            args.ui_root,
            args.ui_dist_dir,
            args.ui_out_zip,
            args.ui_install,
        )?;
        println!();
    }

    // Discover drivers and plugins
    let drivers = if args.bin_only {
        Vec::new()
    } else {
        discover_packages(&workspace_root.join("ng-gateway-southward"))?
    };
    let plugins = if args.bin_only {
        Vec::new()
    } else {
        discover_packages(&workspace_root.join("ng-gateway-northward"))?
    };

    if drivers.is_empty() && plugins.is_empty() && !args.bin_only {
        println!("⚠️  No drivers or plugins found to build");
        return Ok(());
    }

    // Print discovered packages
    if !drivers.is_empty() {
        println!("📦 Drivers:");
        for driver in &drivers {
            println!("   • {}", driver);
        }
        println!();
    }

    if !plugins.is_empty() {
        println!("🔌 Plugins:");
        for plugin in &plugins {
            println!("   • {}", plugin);
        }
        println!();
    }

    // Build drivers and plugins
    println!("🔨 Building (profile: {})...", args.profile);
    println!();

    let cargo_program = cargo_build_program();
    let mut cmd = Command::new(&cargo_program);
    cmd.current_dir(&workspace_root);
    cmd.arg("build");

    if args.profile == "release" {
        cmd.arg("--release");
    }

    // Add all discovered packages
    for package in drivers.iter().chain(plugins.iter()) {
        cmd.arg("-p").arg(package);
    }

    // Add main binary
    cmd.arg("-p").arg("ng-gateway-bin");

    // Add user-provided cargo arguments
    for arg in args.cargo_args {
        cmd.arg(arg);
    }

    let status = cmd.status().context("Failed to execute cargo build")?;

    if !status.success() {
        anyhow::bail!("Build failed");
    }

    println!();
    println!("✅ Build completed successfully");
    println!();

    // Deploy drivers and plugins
    // Deploy only makes sense when we built drivers/plugins.
    if !args.bin_only && !args.no_deploy {
        let target = extract_target_triple(args.cargo_args);
        deploy_command(args.profile, target.as_deref())?;
    }

    Ok(())
}

/// Deploy built drivers and plugins
fn deploy_command(profile: &str, target: Option<&str>) -> Result<()> {
    println!("📦 Deploying drivers and plugins...");
    println!();

    let workspace_root = workspace_root()?;
    let target_dir = if let Ok(target_dir) = env::var("CARGO_TARGET_DIR") {
        PathBuf::from(target_dir)
    } else {
        workspace_root.join("target")
    };
    let source_dir = match target {
        Some(triple) => target_dir.join(triple).join(profile),
        None => target_dir.join(profile),
    };

    println!("   Source: {}", source_dir.display());
    println!();

    // Determine library extension based on platform
    let lib_extension = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };

    // Deploy drivers
    let drivers_dest = workspace_root.join("drivers").join("builtin");
    let drivers_count = deploy_libraries(
        &source_dir,
        &drivers_dest,
        &format!("libng_driver_*.{}", lib_extension),
        "drivers",
    )?;

    // Deploy plugins
    let plugins_dest = workspace_root.join("plugins").join("builtin");
    let plugins_count = deploy_libraries(
        &source_dir,
        &plugins_dest,
        &format!("libng_plugin_*.{}", lib_extension),
        "plugins",
    )?;

    println!();

    if drivers_count == 0 && plugins_count == 0 {
        anyhow::bail!(
            "No driver or plugin libraries found in {}\n\
             Make sure crates have [lib] crate-type = [\"cdylib\"]",
            source_dir.display()
        );
    }

    println!(
        "🚀 Ready to run: {}",
        source_dir.join("ng-gateway-bin").display()
    );

    Ok(())
}

/// Clean build artifacts
fn clean_command(clean_all: bool) -> Result<()> {
    let workspace_root = workspace_root()?;

    println!("🧹 Cleaning build artifacts...");

    let status = Command::new("cargo")
        .current_dir(&workspace_root)
        .arg("clean")
        .status()
        .context("Failed to execute cargo clean")?;

    if !status.success() {
        anyhow::bail!("cargo clean failed");
    }

    println!("✅ Build artifacts cleaned");

    if clean_all {
        // Clean deployed drivers
        let drivers_dir = workspace_root.join("drivers").join("builtin");
        if drivers_dir.exists() {
            println!("🧹 Cleaning deployed drivers...");
            fs::remove_dir_all(&drivers_dir)
                .context("Failed to remove drivers/builtin directory")?;
            println!("✅ Deployed drivers cleaned");
        }

        // Clean deployed plugins
        let plugins_dir = workspace_root.join("plugins").join("builtin");
        if plugins_dir.exists() {
            println!("🧹 Cleaning deployed plugins...");
            fs::remove_dir_all(&plugins_dir)
                .context("Failed to remove plugins/builtin directory")?;
            println!("✅ Deployed plugins cleaned");
        }
    }

    Ok(())
}

/// Discover packages (drivers or plugins) in a directory
fn discover_packages(base_dir: &Path) -> Result<Vec<String>> {
    let mut packages = Vec::new();

    if !base_dir.exists() {
        return Ok(packages);
    }

    let entries = fs::read_dir(base_dir)
        .with_context(|| format!("Failed to read directory: {}", base_dir.display()))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let cargo_toml = path.join("Cargo.toml");
            if cargo_toml.exists() {
                // Read Cargo.toml to get package name
                if let Ok(content) = fs::read_to_string(&cargo_toml) {
                    if let Some(name) = extract_package_name(&content) {
                        packages.push(name);
                    }
                }
            }
        }
    }

    packages.sort();
    Ok(packages)
}

/// Extract package name from Cargo.toml content
fn extract_package_name(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("name") && line.contains('=') {
            // Parse: name = "package-name"
            let parts: Vec<&str> = line.split('=').collect();
            if parts.len() == 2 {
                let name = parts[1].trim().trim_matches('"').trim_matches('\'');
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Deploy libraries matching a pattern to a destination directory
fn deploy_libraries(
    source_dir: &Path,
    dest_dir: &Path,
    pattern: &str,
    category: &str,
) -> Result<usize> {
    // Create destination directory
    fs::create_dir_all(dest_dir)
        .with_context(|| format!("Failed to create directory: {}", dest_dir.display()))?;

    let mut copied_count = 0;

    if let Ok(entries) = fs::read_dir(source_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(filename) = path.file_name().and_then(|s| s.to_str()) {
                if matches_pattern(filename, pattern) {
                    let dest_path = dest_dir.join(filename);
                    fs::copy(&path, &dest_path)
                        .with_context(|| format!("Failed to copy {}", filename))?;
                    println!("   ✓ {} → {}", filename, category);
                    copied_count += 1;
                }
            }
        }
    }

    Ok(copied_count)
}

/// Get workspace root directory
fn workspace_root() -> Result<PathBuf> {
    let output = Command::new("cargo")
        .arg("locate-project")
        .arg("--workspace")
        .arg("--message-format=plain")
        .output()
        .context("Failed to locate workspace root")?;

    if !output.status.success() {
        anyhow::bail!("Failed to locate workspace");
    }

    let cargo_toml = String::from_utf8(output.stdout).context("Invalid UTF-8 in cargo output")?;
    let cargo_toml = cargo_toml.trim();

    let workspace_root = Path::new(cargo_toml)
        .parent()
        .context("Failed to get workspace root")?
        .to_path_buf();

    Ok(workspace_root)
}

/// Check if filename matches a simple glob pattern
fn matches_pattern(filename: &str, pattern: &str) -> bool {
    let pattern_parts: Vec<&str> = pattern.split('*').collect();

    if pattern_parts.len() != 2 {
        return filename == pattern;
    }

    let prefix = pattern_parts[0];
    let suffix = pattern_parts[1];

    filename.starts_with(prefix) && filename.ends_with(suffix)
}
