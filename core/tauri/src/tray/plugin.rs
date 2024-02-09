// Copyright 2019-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use std::path::PathBuf;

use serde::Deserialize;

use crate::{
  command,
  ipc::Channel,
  menu::{plugin::ItemKind, Menu, Submenu},
  plugin::{Builder, TauriPlugin},
  resources::ResourceId,
  tray::TrayIconBuilder,
  AppHandle, IconDto, Manager, Runtime,
};

use super::TrayIcon;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrayIconOptions {
  id: Option<String>,
  menu: Option<(ResourceId, ItemKind)>,
  icon: Option<IconDto>,
  tooltip: Option<String>,
  title: Option<String>,
  temp_dir_path: Option<PathBuf>,
  icon_as_template: Option<bool>,
  menu_on_left_click: Option<bool>,
}

#[command(root = "crate")]
fn new<R: Runtime>(
  app: AppHandle<R>,
  options: TrayIconOptions,
  handler: Channel,
) -> crate::Result<(ResourceId, String)> {
  let mut builder = if let Some(id) = options.id {
    TrayIconBuilder::<R>::with_id(id)
  } else {
    TrayIconBuilder::<R>::new()
  };

  builder = builder.on_tray_icon_event(move |_tray, e| {
    let _ = handler.send(e);
  });

  let mut resources_table = app.resources_table();

  if let Some((rid, kind)) = options.menu {
    match kind {
      ItemKind::Menu => {
        let menu = resources_table.get::<Menu<R>>(rid)?;
        builder = builder.menu(&*menu);
      }
      ItemKind::Submenu => {
        let submenu = resources_table.get::<Submenu<R>>(rid)?;
        builder = builder.menu(&*submenu);
      }
      _ => return Err(anyhow::anyhow!("unexpected menu item kind").into()),
    };
  }
  if let Some(icon) = options.icon {
    builder = builder.icon(icon.into());
  }
  if let Some(tooltip) = options.tooltip {
    builder = builder.tooltip(tooltip);
  }
  if let Some(title) = options.title {
    builder = builder.title(title);
  }
  if let Some(temp_dir_path) = options.temp_dir_path {
    builder = builder.temp_dir_path(temp_dir_path);
  }
  if let Some(icon_as_template) = options.icon_as_template {
    builder = builder.icon_as_template(icon_as_template);
  }
  if let Some(menu_on_left_click) = options.menu_on_left_click {
    builder = builder.menu_on_left_click(menu_on_left_click);
  }

  let tray = builder.build(&app)?;
  let id = tray.id().as_ref().to_string();
  let rid = resources_table.add(tray);

  Ok((rid, id))
}

#[command(root = "crate")]
fn set_icon<R: Runtime>(
  app: AppHandle<R>,
  rid: ResourceId,
  icon: Option<IconDto>,
) -> crate::Result<()> {
  let resources_table = app.resources_table();
  let tray = resources_table.get::<TrayIcon<R>>(rid)?;
  tray.set_icon(icon.map(Into::into))
}

#[command(root = "crate")]
fn set_menu<R: Runtime>(
  app: AppHandle<R>,
  rid: ResourceId,
  menu: Option<(ResourceId, ItemKind)>,
) -> crate::Result<()> {
  let resources_table = app.resources_table();
  let tray = resources_table.get::<TrayIcon<R>>(rid)?;
  if let Some((rid, kind)) = menu {
    match kind {
      ItemKind::Menu => {
        let menu = resources_table.get::<Menu<R>>(rid)?;
        tray.set_menu(Some((*menu).clone()))?;
      }
      ItemKind::Submenu => {
        let submenu = resources_table.get::<Submenu<R>>(rid)?;
        tray.set_menu(Some((*submenu).clone()))?;
      }
      _ => return Err(anyhow::anyhow!("unexpected menu item kind").into()),
    };
  } else {
    tray.set_menu(None::<Menu<R>>)?;
  }
  Ok(())
}

#[command(root = "crate")]
fn set_tooltip<R: Runtime>(
  app: AppHandle<R>,
  rid: ResourceId,
  tooltip: Option<String>,
) -> crate::Result<()> {
  let resources_table = app.resources_table();
  let tray = resources_table.get::<TrayIcon<R>>(rid)?;
  tray.set_tooltip(tooltip)
}

#[command(root = "crate")]
fn set_title<R: Runtime>(
  app: AppHandle<R>,
  rid: ResourceId,
  title: Option<String>,
) -> crate::Result<()> {
  let resources_table = app.resources_table();
  let tray = resources_table.get::<TrayIcon<R>>(rid)?;
  tray.set_title(title)
}

#[command(root = "crate")]
fn set_visible<R: Runtime>(app: AppHandle<R>, rid: ResourceId, visible: bool) -> crate::Result<()> {
  let resources_table = app.resources_table();
  let tray = resources_table.get::<TrayIcon<R>>(rid)?;
  tray.set_visible(visible)
}

#[command(root = "crate")]
fn set_temp_dir_path<R: Runtime>(
  app: AppHandle<R>,
  rid: ResourceId,
  path: Option<PathBuf>,
) -> crate::Result<()> {
  let resources_table = app.resources_table();
  let tray = resources_table.get::<TrayIcon<R>>(rid)?;
  tray.set_temp_dir_path(path)
}

#[command(root = "crate")]
fn set_icon_as_template<R: Runtime>(
  app: AppHandle<R>,
  rid: ResourceId,
  as_template: bool,
) -> crate::Result<()> {
  let resources_table = app.resources_table();
  let tray = resources_table.get::<TrayIcon<R>>(rid)?;
  tray.set_icon_as_template(as_template)
}

#[command(root = "crate")]
fn set_show_menu_on_left_click<R: Runtime>(
  app: AppHandle<R>,
  rid: ResourceId,
  on_left: bool,
) -> crate::Result<()> {
  let resources_table = app.resources_table();
  let tray = resources_table.get::<TrayIcon<R>>(rid)?;
  tray.set_show_menu_on_left_click(on_left)
}

pub(crate) fn init<R: Runtime>() -> TauriPlugin<R> {
  Builder::new("tray")
    .invoke_handler(crate::generate_handler![
      new,
      set_icon,
      set_menu,
      set_tooltip,
      set_title,
      set_visible,
      set_temp_dir_path,
      set_icon_as_template,
      set_show_menu_on_left_click,
    ])
    .build()
}
