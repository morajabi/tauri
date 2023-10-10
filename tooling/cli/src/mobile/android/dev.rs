// Copyright 2019-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use super::{
  configure_cargo, delete_codegen_vars, device_prompt, ensure_init, env, get_app, get_config,
  inject_assets, open_and_wait, setup_dev_config, MobileTarget,
};
use crate::{
  dev::Options as DevOptions,
  helpers::{
    app_paths::tauri_dir,
    config::{get as get_tauri_config, ConfigHandle},
    flock, resolve_merge_config,
  },
  interface::{AppSettings, Interface, MobileOptions, Options as InterfaceOptions},
  mobile::{write_options, CliOptions, DevChild, DevProcess},
  Result,
};
use clap::{ArgAction, Parser};

use anyhow::Context;
use cargo_mobile2::{
  android::{
    config::{Config as AndroidConfig, Metadata as AndroidMetadata},
    device::Device,
    env::Env,
    target::Target,
  },
  config::app::App,
  opts::{FilterLevel, NoiseLevel, Profile},
  target::TargetTrait,
};

use std::env::{set_current_dir, set_var};

const WEBVIEW_CLIENT_CLASS_EXTENSION: &str = "
    @android.annotation.SuppressLint(\"WebViewClientOnReceivedSslError\")
    override fun onReceivedSslError(view: WebView?, handler: SslErrorHandler, error: android.net.http.SslError) {
        handler.proceed()
    }
";
const WEBVIEW_CLASS_INIT: &str =
  "this.settings.mixedContentMode = android.webkit.WebSettings.MIXED_CONTENT_ALWAYS_ALLOW";

#[derive(Debug, Clone, Parser)]
#[clap(about = "Android dev")]
pub struct Options {
  /// List of cargo features to activate
  #[clap(short, long, action = ArgAction::Append, num_args(0..))]
  pub features: Option<Vec<String>>,
  /// Exit on panic
  #[clap(short, long)]
  exit_on_panic: bool,
  /// JSON string or path to JSON file to merge with tauri.conf.json
  #[clap(short, long)]
  pub config: Option<String>,
  /// Disable the file watcher
  #[clap(long)]
  pub no_watch: bool,
  /// Disable the dev server for static files.
  #[clap(long)]
  pub no_dev_server: bool,
  /// Open Android Studio instead of trying to run on a connected device
  #[clap(short, long)]
  pub open: bool,
  /// Runs on the given device name
  pub device: Option<String>,
  /// Specify port for the dev server for static files. Defaults to 1430
  /// Can also be set using `TAURI_DEV_SERVER_PORT` env var.
  #[clap(long)]
  pub port: Option<u16>,
  /// Force prompting for an IP to use to connect to the dev server on mobile.
  #[clap(long)]
  pub force_ip_prompt: bool,
  /// Run the code in release mode
  #[clap(long = "release")]
  pub release_mode: bool,
}

impl From<Options> for DevOptions {
  fn from(options: Options) -> Self {
    Self {
      runner: None,
      target: None,
      features: options.features,
      exit_on_panic: options.exit_on_panic,
      config: options.config,
      args: Vec::new(),
      no_watch: options.no_watch,
      no_dev_server: options.no_dev_server,
      port: options.port,
      force_ip_prompt: options.force_ip_prompt,
      release_mode: options.release_mode,
    }
  }
}

pub fn command(options: Options, noise_level: NoiseLevel) -> Result<()> {
  let result = run_command(options, noise_level);
  if result.is_err() {
    crate::dev::kill_before_dev_process();
  }
  result
}

fn run_command(mut options: Options, noise_level: NoiseLevel) -> Result<()> {
  delete_codegen_vars();

  let (merge_config, _merge_config_path) = resolve_merge_config(&options.config)?;
  options.config = merge_config;

  let tauri_config = get_tauri_config(
    tauri_utils::platform::Target::Android,
    options.config.as_deref(),
  )?;

  let (app, config, metadata) = {
    let tauri_config_guard = tauri_config.lock().unwrap();
    let tauri_config_ = tauri_config_guard.as_ref().unwrap();
    let app = get_app(tauri_config_);
    let (config, metadata) = get_config(&app, tauri_config_, &Default::default());
    (app, config, metadata)
  };

  set_var(
    "WRY_RUSTWEBVIEWCLIENT_CLASS_EXTENSION",
    WEBVIEW_CLIENT_CLASS_EXTENSION,
  );
  set_var("WRY_RUSTWEBVIEW_CLASS_INIT", WEBVIEW_CLASS_INIT);

  let tauri_path = tauri_dir();
  set_current_dir(tauri_path).with_context(|| "failed to change current working directory")?;

  ensure_init(config.project_dir(), MobileTarget::Android)?;
  run_dev(options, tauri_config, &app, &config, &metadata, noise_level)
}

fn run_dev(
  mut options: Options,
  tauri_config: ConfigHandle,
  app: &App,
  config: &AndroidConfig,
  metadata: &AndroidMetadata,
  noise_level: NoiseLevel,
) -> Result<()> {
  setup_dev_config(
    MobileTarget::Android,
    &mut options.config,
    options.force_ip_prompt,
  )?;
  let mut env = env()?;
  let device = if options.open {
    None
  } else {
    match device_prompt(&env, options.device.as_deref()) {
      Ok(d) => Some(d),
      Err(e) => {
        log::error!("{e}");
        None
      }
    }
  };

  let mut dev_options: DevOptions = options.clone().into();
  let target_triple = device
    .as_ref()
    .map(|d| d.target().triple.to_string())
    .unwrap_or_else(|| Target::all().values().next().unwrap().triple.into());
  dev_options.target = Some(target_triple.clone());
  let mut interface = crate::dev::setup(
    tauri_utils::platform::Target::Android,
    &mut dev_options,
    true,
  )?;

  let interface_options = InterfaceOptions {
    debug: !dev_options.release_mode,
    target: dev_options.target.clone(),
    ..Default::default()
  };

  let app_settings = interface.app_settings();
  let bin_path = app_settings.app_binary_path(&interface_options)?;
  let out_dir = bin_path.parent().unwrap();
  let _lock = flock::open_rw(out_dir.join("lock").with_extension("android"), "Android")?;

  configure_cargo(app, Some((&mut env, config)))?;

  // run an initial build to initialize plugins
  let target = Target::all()
    .values()
    .find(|t| t.triple == target_triple)
    .unwrap_or_else(|| Target::all().values().next().unwrap());
  target.build(
    config,
    metadata,
    &env,
    noise_level,
    true,
    if options.release_mode {
      Profile::Release
    } else {
      Profile::Debug
    },
  )?;

  let open = options.open;
  let exit_on_panic = options.exit_on_panic;
  let no_watch = options.no_watch;
  interface.mobile_dev(
    MobileOptions {
      debug: !options.release_mode,
      features: options.features,
      args: Vec::new(),
      config: options.config,
      no_watch: options.no_watch,
    },
    |options| {
      let cli_options = CliOptions {
        features: options.features.clone(),
        args: options.args.clone(),
        noise_level,
        vars: Default::default(),
      };

      let _handle = write_options(
        &tauri_config
          .lock()
          .unwrap()
          .as_ref()
          .unwrap()
          .tauri
          .bundle
          .identifier,
        cli_options,
      )?;

      inject_assets(config, tauri_config.lock().unwrap().as_ref().unwrap())?;

      if open {
        open_and_wait(config, &env)
      } else if let Some(device) = &device {
        match run(device, options, config, &env, metadata, noise_level) {
          Ok(c) => {
            crate::dev::wait_dev_process(c.clone(), move |status, reason| {
              crate::dev::on_app_exit(status, reason, exit_on_panic, no_watch)
            });
            Ok(Box::new(c) as Box<dyn DevProcess + Send>)
          }
          Err(e) => {
            crate::dev::kill_before_dev_process();
            Err(e.into())
          }
        }
      } else {
        open_and_wait(config, &env)
      }
    },
  )
}

#[derive(Debug, thiserror::Error)]
enum RunError {
  #[error("{0}")]
  RunFailed(String),
}

fn run(
  device: &Device<'_>,
  options: MobileOptions,
  config: &AndroidConfig,
  env: &Env,
  metadata: &AndroidMetadata,
  noise_level: NoiseLevel,
) -> Result<DevChild, RunError> {
  let profile = if options.debug {
    Profile::Debug
  } else {
    Profile::Release
  };

  let build_app_bundle = metadata.asset_packs().is_some();

  device
    .run(
      config,
      env,
      noise_level,
      profile,
      Some(match noise_level {
        NoiseLevel::Polite => FilterLevel::Info,
        NoiseLevel::LoudAndProud => FilterLevel::Debug,
        NoiseLevel::FranklyQuitePedantic => FilterLevel::Verbose,
      }),
      build_app_bundle,
      false,
      ".MainActivity".into(),
    )
    .map(DevChild::new)
    .map_err(|e| RunError::RunFailed(e.to_string()))
}
