use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

const BASE_URL_TOKEN: &str = "__KOMGA_BASE_URL__";
const PREBUILT_ENV: &str = "KOMGA_WEBUI_DIST_DIR";

fn main() {
    println!("cargo::rerun-if-env-changed={PREBUILT_ENV}");
    if env::var_os("CARGO_FEATURE_WEBUI").is_none() {
        return;
    }

    if let Err(error) = package_webui() {
        panic!("failed to package WebUI: {error}");
    }
}

fn package_webui() -> Result<(), String> {
    let manifest_dir = PathBuf::from(required_env("CARGO_MANIFEST_DIR")?);
    let workspace_dir = manifest_dir.join("../..");
    let output_dir = PathBuf::from(required_env("OUT_DIR")?).join("webui");

    if let Some(prebuilt) = env::var_os(PREBUILT_ENV) {
        let prebuilt = PathBuf::from(prebuilt);
        let prebuilt = if prebuilt.is_absolute() {
            prebuilt
        } else {
            workspace_dir.join(prebuilt)
        };
        println!("cargo::rerun-if-changed={}", prebuilt.display());
        replace_directory(&prebuilt, &output_dir)
            .map_err(|error| format!("could not copy prebuilt WebUI from {}: {error}", prebuilt.display()))?;
    } else {
        let webui_dir = workspace_dir.join("webui");
        watch_webui_inputs(&webui_dir);
        validate_node(&webui_dir)?;
        install_dependencies(&webui_dir)?;
        remove_directory(&output_dir).map_err(|error| format!("could not clear {}: {error}", output_dir.display()))?;
        run(
            Command::new(npm_command())
                .current_dir(&webui_dir)
                .arg("run")
                .arg("build")
                .arg("--")
                .arg("--dest")
                .arg(&output_dir),
            "npm run build",
        )?;
    }

    validate_output(&output_dir)
}

fn watch_webui_inputs(webui_dir: &Path) {
    for path in [
        ".browserslistrc",
        ".env",
        "babel.config.js",
        "package-lock.json",
        "package.json",
        "postcss.config.js",
        "public",
        "src",
        "tsconfig.json",
        "vue.config.js",
    ] {
        println!("cargo::rerun-if-changed={}", webui_dir.join(path).display());
    }
}

fn validate_node(webui_dir: &Path) -> Result<(), String> {
    let output = Command::new("node")
        .arg("--version")
        .current_dir(webui_dir)
        .output()
        .map_err(|error| format!("Node.js 22.11 through 24 is required: {error}"))?;
    if !output.status.success() {
        return Err(format!("Node.js version check failed with {}", output.status));
    }
    let version =
        String::from_utf8(output.stdout).map_err(|error| format!("Node.js returned a non-UTF-8 version: {error}"))?;
    let version = version.trim().trim_start_matches('v');
    let mut components = version.split('.');
    let major = components
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| format!("could not parse Node.js version {version:?}"))?;
    let minor = components
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| format!("could not parse Node.js version {version:?}"))?;
    if !(major == 22 && minor >= 11 || (23..25).contains(&major)) {
        return Err(format!(
            "Node.js {version} is unsupported; install a version in the range >=22.11.0 <25"
        ));
    }
    Ok(())
}

fn install_dependencies(webui_dir: &Path) -> Result<(), String> {
    let lock = fs::read(webui_dir.join("package-lock.json"))
        .map_err(|error| format!("could not read package-lock.json: {error}"))?;
    let stamp = webui_dir.join("node_modules/.komga-package-lock");
    if fs::read(&stamp).ok().as_deref() == Some(lock.as_slice()) {
        return Ok(());
    }

    run(Command::new(npm_command()).current_dir(webui_dir).arg("ci"), "npm ci")?;
    fs::write(&stamp, lock).map_err(|error| format!("could not write dependency stamp {}: {error}", stamp.display()))
}

fn npm_command() -> &'static str {
    if cfg!(windows) { "npm.cmd" } else { "npm" }
}

fn run(command: &mut Command, description: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("could not start {description}: {error}"))?;
    successful(status, description)
}

fn successful(status: ExitStatus, description: &str) -> Result<(), String> {
    if status.success() {
        Ok(())
    } else {
        Err(format!("{description} exited with {status}"))
    }
}

fn validate_output(output_dir: &Path) -> Result<(), String> {
    let index_path = output_dir.join("index.html");
    let index = fs::read_to_string(&index_path)
        .map_err(|error| format!("could not read generated {}: {error}", index_path.display()))?;
    if index.matches(BASE_URL_TOKEN).count() < 2 {
        return Err(format!(
            "generated {} must contain the {BASE_URL_TOKEN} placeholder for both the document base and resource URL",
            index_path.display()
        ));
    }
    Ok(())
}

fn replace_directory(source: &Path, target: &Path) -> io::Result<()> {
    if !source.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not a directory", source.display()),
        ));
    }
    remove_directory(target)?;
    copy_directory(source, target)
}

fn remove_directory(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn copy_directory(source: &Path, target: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("symlinks are not allowed in WebUI assets: {}", source.display()),
        ));
    }
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("symlinks are not allowed in WebUI assets: {}", source_path.display()),
            ));
        }
        if file_type.is_dir() {
            copy_directory(&source_path, &target_path)?;
        } else if file_type.is_file() {
            fs::copy(source_path, target_path)?;
        }
    }
    Ok(())
}

fn required_env(name: &str) -> Result<String, String> {
    env::var(name).map_err(|error| format!("Cargo did not set {name}: {error}"))
}
