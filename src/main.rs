use directories::ProjectDirs;
use foreseer_desktop::config::{AppConfig, AppMode, MIN_CACHE_LIMIT_BYTES, validate_foreseer_url};
use foreseer_desktop::extension::ForeseerExtension;
use foreseer_desktop::setup::setup_document_url;
use foreseer_desktop::supervisor::StandaloneSupervisor;
use jfn_rust::{HostExtensionDescriptor, HostOptions};
use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

const SETUP_RELAUNCH_ENV: &str = "FORESEER_SETUP_RELAUNCHED";

fn main() {
    // CEF helper processes share this binary. They must never create data,
    // backups, or a managed Node child.
    if std::env::args().any(|arg| arg.starts_with("--type=")) {
        std::process::exit(jfn_rust::app::jfn_app_main_with(HostOptions::default()));
    }
    relaunch_after_consuming_setup_flag();
    configure_product_profile();
    let cli_requested_setup = handle_cli_args();

    let config = AppConfig::load();
    let needs_setup =
        cli_requested_setup || (config.mode == AppMode::Remote && !config.is_configured());

    let frontend_script = include_str!("assets/foreseer-native.js")
        .replace("__HOST_VERSION__", env!("CARGO_PKG_VERSION"));
    let primary_web_script = include_str!("assets/jellyfin-session.js").to_string();

    let mut standalone: Option<Arc<Mutex<StandaloneSupervisor>>> = None;
    let (descriptor, frontend_url, allow_insecure_http) = if needs_setup {
        let url = setup_document_url("");
        let descriptor = HostExtensionDescriptor::from_setup_document(
            &url,
            vec![frontend_script],
            vec![primary_web_script],
            false,
        )
        .expect("generated setup document");
        (descriptor, url, true)
    } else if config.mode == AppMode::Standalone {
        let child = match StandaloneSupervisor::start(&config) {
            Ok(child) => child,
            Err(error) => {
                let url = setup_document_url(&error.to_string());
                let descriptor = HostExtensionDescriptor::from_setup_document(
                    &url,
                    vec![frontend_script],
                    vec![primary_web_script],
                    false,
                )
                .expect("generated recovery document");
                let extension: Arc<dyn jfn_rust::HostExtension> =
                    ForeseerExtension::new(descriptor, url, true, true);
                let options = HostOptions::with_extension(extension);
                std::process::exit(jfn_rust::app::jfn_app_main_with(options));
            }
        };
        let url = child.origin.clone();
        let descriptor = HostExtensionDescriptor::from_url(
            &url,
            vec![frontend_script],
            vec![primary_web_script],
            false,
        )
        .expect("validated managed loopback URL");
        standalone = Some(Arc::new(Mutex::new(child)));
        (descriptor, url, true)
    } else {
        let url =
            validate_foreseer_url(&config.remote.server_url)
                .expect("validated configured Foreseer URL");
        let descriptor = HostExtensionDescriptor::from_url(
            &url,
            vec![frontend_script],
            vec![primary_web_script],
            false,
        )
        .expect("validated configured Foreseer URL");
        (descriptor, url, config.remote.allow_insecure_http)
    };

    let extension: Arc<dyn jfn_rust::HostExtension> = ForeseerExtension::new_with_supervisor(
        descriptor,
        frontend_url,
        allow_insecure_http,
        needs_setup,
        standalone.clone(),
    );
    let options = HostOptions::with_extension(extension);
    let options = if config.mode == AppMode::Standalone {
        options.with_cef_disk_cache_limit(config.standalone.cache_limit_bytes * 3 / 8)
    } else {
        options
    };
    let code = jfn_rust::app::jfn_app_main_with(options);
    if let Some(supervisor) = &standalone
        && let Ok(mut supervisor) = supervisor.lock()
    {
        supervisor.shutdown();
    }
    drop(standalone);
    std::process::exit(code);
}

/// `--setup` belongs to Foreseer, while the embedded Jellium runtime parses
/// the remaining process arguments. Relaunch without the Foreseer-only flag
/// so Jellium never rejects it as an unknown option.
fn relaunch_after_consuming_setup_flag() {
    if std::env::var_os(SETUP_RELAUNCH_ENV).is_some()
        || std::env::args_os().nth(1).as_deref() != Some(OsStr::new("--setup"))
    {
        return;
    }

    let executable = std::env::current_exe().expect("current Foreseer executable path");
    let status = Command::new(executable)
        .args(std::env::args_os().skip(2))
        .env(SETUP_RELAUNCH_ENV, "1")
        .status()
        .expect("relaunch Foreseer without --setup");
    std::process::exit(status.code().unwrap_or(1));
}

fn handle_cli_args() -> bool {
    if std::env::var_os(SETUP_RELAUNCH_ENV).is_some() {
        return true;
    }
    let args: Vec<String> = std::env::args().collect();
    if args.len() <= 1 {
        return false;
    }

    match args[1].as_str() {
        "--setup" => return true,
        "--help" | "-h" => {
            println!("Foreseer Desktop Client");
            println!();
            println!("USAGE:");
            println!("  foreseer-desktop [OPTIONS]");
            println!();
            println!("OPTIONS:");
            println!("  --setup            Open the graphical server setup page");
            println!("  --remote <URL>     Set remote Foreseerr URL and switch to remote mode");
            println!("  --set-url <URL>    Compatibility alias for --remote");
            println!("  --standalone       Switch to bundled standalone mode");
            println!("  --cache-limit <B>  Set standalone transient cache budget in bytes");
            println!("  --show-config      Display current config file path and settings");
            println!("  --help, -h         Show this help message");
            std::process::exit(0);
        }
        "--show-config" => {
            let path = AppConfig::config_file_path();
            let config = AppConfig::load();
            println!(
                "Config Path: {}",
                path.map(|p| p.display().to_string())
                    .unwrap_or_else(|| "Unknown".into())
            );
            println!("Schema version: {}", config.schema_version);
            println!("Mode:           {:?}", config.mode);
            println!("Remote URL:     {}", config.remote.server_url);
            println!("Allow HTTP:     {}", config.remote.allow_insecure_http);
            println!(
                "Standalone data: {}",
                AppConfig::standalone_data_directory()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "Unknown".into())
            );
            println!("Cache budget:   {}", config.standalone.cache_limit_bytes);
            println!(
                "Bundled Foreseerr version: {}",
                include_str!("../foreseerr.version").trim()
            );
            std::process::exit(0);
        }
        "--set-url" | "--remote" => {
            if args.len() < 3 {
                eprintln!(
                    "Error: --set-url requires a URL argument (e.g. --set-url https://my-server.com)"
                );
                std::process::exit(1);
            }
            let url = args[2].clone();
            let url = match validate_foreseer_url(&url) {
                Ok(url) => url,
                Err(error) => {
                    eprintln!("Error: {}", error.message());
                    std::process::exit(1);
                }
            };

            let mut config = AppConfig::load();
            config.mode = AppMode::Remote;
            config.remote.server_url = url.clone();
            config.remote.allow_insecure_http = url.starts_with("http://");
            if let Err(e) = config.save() {
                eprintln!("Error saving config: {}", e);
                std::process::exit(1);
            }
            println!("Successfully saved server URL to config.");
            std::process::exit(0);
        }
        "--standalone" => {
            let mut config = AppConfig::load();
            config.mode = AppMode::Standalone;
            if let Err(e) = config.save() {
                eprintln!("Error saving config: {e}");
                std::process::exit(1);
            }
            println!("Successfully enabled standalone mode.");
            std::process::exit(0);
        }
        "--cache-limit" => {
            let Some(value) = args.get(2) else {
                eprintln!("Error: --cache-limit requires a byte value");
                std::process::exit(1);
            };
            let limit = match value.parse::<u64>() {
                Ok(limit) if limit >= MIN_CACHE_LIMIT_BYTES => limit,
                _ => {
                    eprintln!(
                        "Error: cache limit must be an integer of at least {MIN_CACHE_LIMIT_BYTES} bytes"
                    );
                    std::process::exit(1);
                }
            };
            let mut config = AppConfig::load();
            config.standalone.cache_limit_bytes = limit;
            if let Err(e) = config.save() {
                eprintln!("Error saving config: {e}");
                std::process::exit(1);
            }
            println!("Successfully set standalone cache budget to {limit} bytes.");
            std::process::exit(0);
        }
        _ => {}
    }
    false
}

fn configure_product_profile() {
    set_default_env("JELLIUM_DESKTOP_TITLE", "Foreseer".as_ref());
    set_default_env(
        "JELLIUM_DESKTOP_APP_ID",
        "com.selmantrabzon.Foreseer".as_ref(),
    );
    mirror_env("FORESEER_LOG_LEVEL", "JELLIUM_DESKTOP_LOG_LEVEL");
    mirror_env("FORESEER_LOG_FILE", "JELLIUM_DESKTOP_LOG_FILE");
    mirror_env("FORESEER_CONFIG_DIR", "JELLIUM_DESKTOP_CONFIG_DIR");
    mirror_env("FORESEER_PLATFORM_PAINT", "JELLIUM_DESKTOP_PLATFORM_PAINT");
    mirror_env("FORESEER_MPV_HOME", "MPV_HOME");
    set_default_env(
        "JELLIUM_DESKTOP_HOST_VERSION",
        env!("CARGO_PKG_VERSION").as_ref(),
    );
    set_default_env(
        "JELLIUM_DESKTOP_HOST_JELLIUM_REVISION",
        include_str!("../jellium.rev").trim().as_ref(),
    );
    if let Some(project_dirs) = ProjectDirs::from("com", "selmantrabzon", "Foreseer") {
        set_default_env(
            "JELLIUM_DESKTOP_CONFIG_DIR",
            project_dirs.config_dir().as_os_str(),
        );
        let cache_root = std::env::var_os("FORESEER_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| project_dirs.cache_dir().to_path_buf());
        set_default_env("FORESEER_CACHE_DIR", cache_root.as_os_str());
        set_default_env(
            "JELLIUM_DESKTOP_CACHE_DIR",
            cache_root.join("cef").as_os_str(),
        );
    }
}

fn set_default_env(target: &str, value: &std::ffi::OsStr) {
    if std::env::var_os(target).is_none() {
        unsafe { std::env::set_var(target, value) };
    }
}

fn mirror_env(source: &str, target: &str) {
    if std::env::var_os(target).is_none()
        && let Some(value) = std::env::var_os(source)
    {
        unsafe { std::env::set_var(target, value) };
    }
}
