// Copyright 2024 RISC Zero, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use cargo_metadata::MetadataCommand;
use docker_generate::DockerFile;
use risc0_binfmt::{MemoryImage, Program};
use risc0_zkvm_platform::{
    memory::{GUEST_MAX_MEM, TEXT_START},
    PAGE_SIZE,
};
use tempfile::tempdir;
use which::which;

use crate::get_env_var;

const DOCKER_MSG: &str = r#"Docker is not running.

Reproducible builds rely on Docker to build the ELF binaries.
Please install Docker and ensure it is running before running this command.
"#;

const DOCKER_IGNORE: &str = r#"
**/Dockerfile
**/.git
**/node_modules
**/target
**/tmp
"#;

const TARGET_DIR: &str = "target/riscv-guest/riscv32im-risc0-zkvm-elf/docker";

/// Indicates weather the build was successful or skipped.
pub enum BuildStatus {
    /// The build was successful.
    Success,
    /// The build was skipped.
    Skipped,
}

/// Build the package in the manifest path using a docker environment.
pub fn docker_build(
    manifest_path: &Path,
    src_dir: &Path,
    features: &[String],
) -> Result<BuildStatus> {
    ensure_docker_is_running()?;

    if !get_env_var("RISC0_SKIP_BUILD").is_empty() {
        eprintln!("Skipping build because RISC0_SKIP_BUILD is set");
        return Ok(BuildStatus::Skipped);
    }

    let manifest_path = canonicalize_path(manifest_path)?;
    let src_dir = canonicalize_path(src_dir)?;
    let root_pkg = get_root_pkg(&manifest_path, &src_dir)?;
    let pkg_name = &root_pkg.name;

    eprintln!("Building ELF binaries in {pkg_name} for riscv32im-risc0-zkvm-elf target...");

    if !Command::new("docker")
        .arg("--version")
        .status()
        .context("Could not find or execute docker")?
        .success()
    {
        bail!("`docker --version` failed");
    }

    if let Err(err) = check_cargo_lock(manifest_path.as_path()) {
        eprintln!("{err}");
    }

    let pkg_name = pkg_name.replace('-', "_");
    {
        let temp_dir = tempdir()?;
        let temp_path = temp_dir.path();
        let rel_manifest_path = manifest_path.strip_prefix(&src_dir)?;
        create_dockerfile(rel_manifest_path, temp_path, pkg_name.as_str(), features)?;
        build(src_dir.as_path(), temp_path)?;
    }
    println!("ELFs ready at:");

    for target in get_targets(&root_pkg) {
        if target.is_bin() {
            let elf_path = get_elf_path(&src_dir, &pkg_name, &target.name);
            let image_id = compute_image_id(&elf_path)?;
            let rel_elf_path = Path::new(TARGET_DIR).join(&pkg_name).join(&target.name);
            println!("ImageID: {} - {:?}", image_id, rel_elf_path);
        }
    }

    Ok(BuildStatus::Success)
}

fn canonicalize_path(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .context(format!("Failed to canonicalize path: {path:?}"))
}

/// Get the path to the ELF binary.
pub fn get_elf_path(
    src_dir: impl AsRef<Path>,
    pkg_name: impl ToString,
    target_name: impl AsRef<Path>,
) -> PathBuf {
    src_dir
        .as_ref()
        .join(TARGET_DIR)
        .join(pkg_name.to_string().replace('-', "_"))
        .join(target_name)
}

/// Get the root package from the manifest path.
pub fn get_root_pkg(manifest_path: &PathBuf, src_dir: &PathBuf) -> Result<cargo_metadata::Package> {
    eprintln!("Docker context: {src_dir:?}");
    let meta = MetadataCommand::new()
        .manifest_path(manifest_path)
        .exec()
        .context("Manifest not found")?;

    Ok(meta
        .root_package()
        .context("failed to parse Cargo.toml")?
        .clone())
}

/// Get the targets from the root package.
pub fn get_targets(root_pkg: &cargo_metadata::Package) -> Vec<cargo_metadata::Target> {
    root_pkg
        .targets
        .iter()
        .filter(|target| target.is_bin())
        .cloned()
        .collect()
}

/// Create the dockerfile.
///
/// Overwrites if a dockerfile already exists.
fn create_dockerfile(
    manifest_path: &Path,
    temp_dir: &Path,
    pkg_name: &str,
    features: &[String],
) -> Result<()> {
    let manifest_env = &[("CARGO_MANIFEST_PATH", manifest_path.to_str().unwrap())];
    let rustflags = format!(
        "-C passes=loweratomic -C link-arg=-Ttext=0x{TEXT_START:08X} -C link-arg=--fatal-warnings",
    );
    let rustflags_env = &[("RUSTFLAGS", rustflags.as_str())];

    let common_args = vec![
        "--locked",
        "--target",
        "riscv32im-risc0-zkvm-elf",
        "--manifest-path",
        "$CARGO_MANIFEST_PATH",
    ];

    let mut build_args = common_args.clone();
    let features_str = features.join(",");
    if !features.is_empty() {
        build_args.push("--features");
        build_args.push(&features_str);
    }

    let fetch_cmd = [&["cargo", "+risc0", "fetch"], common_args.as_slice()]
        .concat()
        .join(" ");
    let build_cmd = [
        &["cargo", "+risc0", "build", "--release"],
        build_args.as_slice(),
    ]
    .concat()
    .join(" ");

    let build = DockerFile::new()
        .from_alias("build", "risczero/risc0-guest-builder:v2024-02-08.1")
        .workdir("/src")
        .copy(".", ".")
        .env(manifest_env)
        .env(rustflags_env)
        .env(&[("CARGO_TARGET_DIR", "target")])
        // Fetching separately allows docker to cache the downloads, assuming the Cargo.lock
        // doesn't change.
        .run(&fetch_cmd)
        .run(&build_cmd);

    let out_dir = format!("/{pkg_name}");
    let binary = DockerFile::new()
        .comment("export stage")
        .from_alias("export", "scratch")
        .copy_from(
            "build",
            "/src/target/riscv32im-risc0-zkvm-elf/release",
            out_dir.as_str(),
        );

    let file = DockerFile::new().dockerfile(build).dockerfile(binary);
    fs::write(temp_dir.join("Dockerfile"), file.to_string())?;
    fs::write(temp_dir.join("Dockerfile.dockerignore"), DOCKER_IGNORE)?;

    Ok(())
}

/// Build the dockerfile and outputs the ELF.
///
/// Overwrites if an ELF with the same name already exists.
fn build(src_dir: &Path, temp_dir: &Path) -> Result<()> {
    let target_dir = src_dir.join(TARGET_DIR);
    let target_dir = target_dir.to_str().unwrap();
    if Command::new("docker")
        .arg("build")
        .arg(format!("--output={target_dir}"))
        .arg("-f")
        .arg(temp_dir.join("Dockerfile"))
        .arg(src_dir)
        .status()
        .context("docker failed to execute")?
        .success()
    {
        Ok(())
    } else {
        Err(anyhow::anyhow!("docker build failed"))
    }
}

fn check_cargo_lock(manifest_path: &Path) -> Result<()> {
    let lock_file = manifest_path
        .parent()
        .context("invalid manifest path")?
        .join("Cargo.lock");
    fs::metadata(lock_file.clone()).context(format!(
        "Cargo.lock not found in path {}",
        lock_file.display()
    ))?;
    Ok(())
}

/// Compute the image ID for a given ELF.
fn compute_image_id(elf_path: &Path) -> Result<String> {
    let elf = fs::read(elf_path)?;
    let program = Program::load_elf(&elf, GUEST_MAX_MEM as u32).context("unable to load elf")?;
    let image =
        MemoryImage::new(&program, PAGE_SIZE as u32).context("unable to create memory image")?;
    Ok(image.compute_id().to_string())
}

/// Check if docker is running.
fn check_docker(docker: impl AsRef<std::ffi::OsStr>, max_elapsed_time: Duration) -> Result<()> {
    eprintln!("Checking if Docker is running...");
    let backoff = backoff::ExponentialBackoffBuilder::new()
        .with_max_elapsed_time(Some(max_elapsed_time))
        .build();
    let f = || -> Result<bool, backoff::Error<anyhow::Error>> {
        Command::new(&docker)
            .arg("version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .context("Failed to run `docker version`")?
            .success()
            .then_some(true)
            .ok_or(anyhow!("Docker engine is not running").into())
    };

    let result = backoff::retry(backoff, f);

    match result {
        Ok(true) => Ok(()),
        _ => Err(anyhow!("Docker engine is not running")),
    }
}

/// Ensures that docker is running.
fn ensure_docker_is_running() -> Result<()> {
    let docker = which("docker").context(DOCKER_MSG)?;
    check_docker(docker, Duration::from_secs(5)).context(DOCKER_MSG)?;
    Ok(())
}

// requires Docker to be installed
#[cfg(feature = "docker")]
#[cfg(test)]
mod test {
    use std::path::Path;

    use super::{docker_build, TARGET_DIR};

    const SRC_DIR: &str = "../..";

    fn build(manifest_path: &str) {
        let src_dir = Path::new(SRC_DIR);
        let manifest_path = Path::new(manifest_path);
        self::docker_build(manifest_path, &src_dir, &[]).unwrap();
    }

    fn compare_image_id(bin_path: &str, expected: &str) {
        let src_dir = Path::new(SRC_DIR);
        let target_dir = src_dir.join(TARGET_DIR);
        let elf_path = target_dir.join(bin_path);
        let actual = super::compute_image_id(&elf_path).unwrap();
        assert_eq!(expected, actual);
    }

    // Test build reproducibility for risc0_zkvm_methods_guest.
    // If the code of the package or any of its dependencies change,
    // it may be required to recompute the expected image_ids.
    // For that, run:
    // `cargo risczero build --manifest-path risc0/zkvm/methods/guest/Cargo.toml`
    #[test]
    fn test_reproducible_methods_guest() {
        build("../../risc0/zkvm/methods/guest/Cargo.toml");
        compare_image_id(
            "risc0_zkvm_methods_guest/multi_test",
            "5419071089b1fa21be2547dd552dbd85ce16d3614d877529e984b03bf20f63c4",
        );
    }
}
