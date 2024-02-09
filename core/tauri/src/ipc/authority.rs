// Copyright 2019-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use std::fmt::{Debug, Display};
use std::{collections::BTreeMap, ops::Deref};

use serde::de::DeserializeOwned;
use state::TypeMap;

use tauri_utils::acl::Value;
use tauri_utils::acl::{
  resolved::{CommandKey, Resolved, ResolvedCommand, ResolvedScope, ScopeKey},
  ExecutionContext,
};

use crate::{ipc::InvokeError, sealed::ManagerBase, Runtime};
use crate::{AppHandle, Manager};

use super::{CommandArg, CommandItem};

/// The runtime authority used to authorize IPC execution based on the Access Control List.
pub struct RuntimeAuthority {
  #[cfg(debug_assertions)]
  acl: BTreeMap<String, crate::utils::acl::plugin::Manifest>,
  allowed_commands: BTreeMap<CommandKey, ResolvedCommand>,
  denied_commands: BTreeMap<CommandKey, ResolvedCommand>,
  pub(crate) scope_manager: ScopeManager,
}

/// The origin trying to access the IPC.
pub enum Origin {
  /// Local app origin.
  Local,
  /// Remote origin.
  Remote {
    /// Remote origin domain.
    domain: String,
  },
}

impl Display for Origin {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Local => write!(f, "local"),
      Self::Remote { domain } => write!(f, "remote: {domain}"),
    }
  }
}

impl Origin {
  fn matches(&self, context: &ExecutionContext) -> bool {
    match (self, context) {
      (Self::Local, ExecutionContext::Local) => true,
      (
        Self::Remote { domain },
        ExecutionContext::Remote {
          domain: domain_pattern,
        },
      ) => domain_pattern.matches(domain),
      _ => false,
    }
  }
}

impl RuntimeAuthority {
  pub(crate) fn new(resolved_acl: Resolved) -> Self {
    let command_cache = resolved_acl
      .command_scope
      .keys()
      .map(|key| (*key, <TypeMap![Send + Sync]>::new()))
      .collect();
    Self {
      #[cfg(debug_assertions)]
      acl: resolved_acl.acl,
      allowed_commands: resolved_acl.allowed_commands,
      denied_commands: resolved_acl.denied_commands,
      scope_manager: ScopeManager {
        command_scope: resolved_acl.command_scope,
        global_scope: resolved_acl.global_scope,
        command_cache,
        global_scope_cache: Default::default(),
      },
    }
  }

  #[cfg(debug_assertions)]
  pub(crate) fn resolve_access_message(
    &self,
    plugin: &str,
    command_name: &str,
    window: &str,
    origin: &Origin,
  ) -> String {
    fn print_references(resolved: &ResolvedCommand) -> String {
      resolved
        .referenced_by
        .iter()
        .map(|r| format!("capability: {}, permission: {}", r.capability, r.permission))
        .collect::<Vec<_>>()
        .join(" || ")
    }

    fn has_permissions_allowing_command(
      manifest: &crate::utils::acl::plugin::Manifest,
      set: &crate::utils::acl::PermissionSet,
      command: &str,
    ) -> bool {
      for permission_id in &set.permissions {
        if permission_id == "default" {
          if let Some(default) = &manifest.default_permission {
            if has_permissions_allowing_command(manifest, default, command) {
              return true;
            }
          }
        } else if let Some(ref_set) = manifest.permission_sets.get(permission_id) {
          if has_permissions_allowing_command(manifest, ref_set, command) {
            return true;
          }
        } else if let Some(permission) = manifest.permissions.get(permission_id) {
          if permission.commands.allow.contains(&command.into()) {
            return true;
          }
        }
      }
      false
    }

    let command = format!("plugin:{plugin}|{command_name}");
    if let Some((_cmd, resolved)) = self
      .denied_commands
      .iter()
      .find(|(cmd, _)| cmd.name == command && origin.matches(&cmd.context))
    {
      format!(
        "{plugin}.{command_name} denied on origin {origin}, referenced by: {}",
        print_references(resolved)
      )
    } else {
      let command_matches = self
        .allowed_commands
        .iter()
        .filter(|(cmd, _)| cmd.name == command)
        .collect::<BTreeMap<_, _>>();

      if let Some((_cmd, resolved)) = command_matches
        .iter()
        .find(|(cmd, _)| origin.matches(&cmd.context))
      {
        if resolved.windows.iter().any(|w| w.matches(window)) {
          "allowed".to_string()
        } else {
          format!("{plugin}.{command_name} not allowed on window {window}, expected one of {}, referenced by {}", resolved.windows.iter().map(|w| w.as_str()).collect::<Vec<_>>().join(", "), print_references(resolved))
        }
      } else {
        let permission_error_detail = if let Some(manifest) = self.acl.get(plugin) {
          let mut permissions_referencing_command = Vec::new();

          if let Some(default) = &manifest.default_permission {
            if has_permissions_allowing_command(manifest, default, command_name) {
              permissions_referencing_command.push("default".into());
            }
          }
          for set in manifest.permission_sets.values() {
            if has_permissions_allowing_command(manifest, set, command_name) {
              permissions_referencing_command.push(set.identifier.clone());
            }
          }
          for permission in manifest.permissions.values() {
            if permission.commands.allow.contains(&command_name.into()) {
              permissions_referencing_command.push(permission.identifier.clone());
            }
          }

          permissions_referencing_command.sort();

          format!(
            "Permissions associated with this command: {}",
            permissions_referencing_command
              .iter()
              .map(|p| format!("{plugin}:{p}"))
              .collect::<Vec<_>>()
              .join(", ")
          )
        } else {
          "Plugin did not define its manifest".to_string()
        };

        if command_matches.is_empty() {
          format!("{plugin}.{command_name} not allowed. {permission_error_detail}")
        } else {
          format!(
            "{plugin}.{command_name} not allowed on origin [{}]. Please create a capability that has this origin on the context field.\n\nFound matches for: {}\n\n{permission_error_detail}",
            origin,
            command_matches
              .iter()
              .map(|(cmd, resolved)| {
                let context = match &cmd.context {
                  ExecutionContext::Local => "[local]".to_string(),
                  ExecutionContext::Remote { domain } => format!("[remote: {}]", domain.as_str()),
                };
                format!(
                  "- context: {context}, referenced by: {}",
                  print_references(resolved)
                )
              })
              .collect::<Vec<_>>()
              .join("\n")
          )
        }
      }
    }
  }

  /// Checks if the given IPC execution is allowed and returns the [`ResolvedCommand`] if it is.
  pub fn resolve_access(
    &self,
    command: &str,
    window: &str,
    origin: &Origin,
  ) -> Option<&ResolvedCommand> {
    if self
      .denied_commands
      .keys()
      .any(|cmd| cmd.name == command && origin.matches(&cmd.context))
    {
      None
    } else {
      self
        .allowed_commands
        .iter()
        .find(|(cmd, _)| cmd.name == command && origin.matches(&cmd.context))
        .map(|(_cmd, resolved)| resolved)
        .filter(|resolved| resolved.windows.iter().any(|w| w.matches(window)))
    }
  }
}

/// List of allowed and denied objects that match either the command-specific or plugin global scope criterias.
#[derive(Debug)]
pub struct ScopeValue<T: ScopeObject> {
  allow: Vec<T>,
  deny: Vec<T>,
}

impl<T: ScopeObject> ScopeValue<T> {
  /// What this access scope allows.
  pub fn allows(&self) -> &Vec<T> {
    &self.allow
  }

  /// What this access scope denies.
  pub fn denies(&self) -> &Vec<T> {
    &self.deny
  }
}

#[derive(Debug)]
enum OwnedOrRef<'a, T: Debug> {
  Owned(T),
  Ref(&'a T),
}

impl<'a, T: Debug> Deref for OwnedOrRef<'a, T> {
  type Target = T;
  fn deref(&self) -> &Self::Target {
    match self {
      Self::Owned(t) => t,
      Self::Ref(r) => r,
    }
  }
}

/// Access scope for a command that can be retrieved directly in the command function.
#[derive(Debug)]
pub struct CommandScope<'a, T: ScopeObject>(OwnedOrRef<'a, ScopeValue<T>>);

impl<'a, T: ScopeObject> CommandScope<'a, T> {
  /// What this access scope allows.
  pub fn allows(&self) -> &Vec<T> {
    &self.0.allow
  }

  /// What this access scope denies.
  pub fn denies(&self) -> &Vec<T> {
    &self.0.deny
  }
}

impl<'a, R: Runtime, T: ScopeObject> CommandArg<'a, R> for CommandScope<'a, T> {
  /// Grabs the [`ResolvedScope`] from the [`CommandItem`] and returns the associated [`CommandScope`].
  fn from_command(command: CommandItem<'a, R>) -> Result<Self, InvokeError> {
    if let Some(scope_id) = command.acl.as_ref().and_then(|resolved| resolved.scope) {
      Ok(CommandScope(OwnedOrRef::Ref(
        command
          .message
          .webview
          .manager()
          .runtime_authority
          .scope_manager
          .get_command_scope_typed(command.message.webview.app_handle(), &scope_id)?,
      )))
    } else {
      Ok(CommandScope(OwnedOrRef::Owned(ScopeValue {
        allow: Vec::new(),
        deny: Vec::new(),
      })))
    }
  }
}

/// Global access scope that can be retrieved directly in the command function.
#[derive(Debug)]
pub struct GlobalScope<'a, T: ScopeObject>(&'a ScopeValue<T>);

impl<'a, T: ScopeObject> GlobalScope<'a, T> {
  /// What this access scope allows.
  pub fn allows(&self) -> &Vec<T> {
    &self.0.allow
  }

  /// What this access scope denies.
  pub fn denies(&self) -> &Vec<T> {
    &self.0.deny
  }
}

impl<'a, R: Runtime, T: ScopeObject> CommandArg<'a, R> for GlobalScope<'a, T> {
  /// Grabs the [`ResolvedScope`] from the [`CommandItem`] and returns the associated [`GlobalScope`].
  fn from_command(command: CommandItem<'a, R>) -> Result<Self, InvokeError> {
    command
      .plugin
      .ok_or_else(|| {
        InvokeError::from_anyhow(anyhow::anyhow!(
          "global scope not available for app commands"
        ))
      })
      .and_then(|plugin| {
        command
          .message
          .webview
          .manager()
          .runtime_authority
          .scope_manager
          .get_global_scope_typed(command.message.webview.app_handle(), plugin)
          .map_err(InvokeError::from_error)
      })
      .map(GlobalScope)
  }
}

#[derive(Debug)]
pub struct ScopeManager {
  command_scope: BTreeMap<ScopeKey, ResolvedScope>,
  global_scope: BTreeMap<String, ResolvedScope>,
  command_cache: BTreeMap<ScopeKey, TypeMap![Send + Sync]>,
  global_scope_cache: TypeMap![Send + Sync],
}

/// Marks a type as a scope object.
///
/// Usually you will just rely on [`serde::de::DeserializeOwned`] instead of implementing it manually,
/// though this is useful if you need to do some initialization logic on the type itself.
pub trait ScopeObject: Sized + Send + Sync + Debug + 'static {
  /// The error type.
  type Error: std::error::Error + Send + Sync;
  /// Deserialize the raw scope value.
  fn deserialize<R: Runtime>(app: &AppHandle<R>, raw: Value) -> Result<Self, Self::Error>;
}

impl<T: Send + Sync + Debug + DeserializeOwned + 'static> ScopeObject for T {
  type Error = serde_json::Error;
  fn deserialize<R: Runtime>(_app: &AppHandle<R>, raw: Value) -> Result<Self, Self::Error> {
    serde_json::from_value(raw.into())
  }
}

impl ScopeManager {
  pub(crate) fn get_global_scope_typed<R: Runtime, T: ScopeObject>(
    &self,
    app: &AppHandle<R>,
    plugin: &str,
  ) -> crate::Result<&ScopeValue<T>> {
    match self.global_scope_cache.try_get() {
      Some(cached) => Ok(cached),
      None => {
        let mut allow: Vec<T> = Vec::new();
        let mut deny: Vec<T> = Vec::new();

        if let Some(global_scope) = self.global_scope.get(plugin) {
          for allowed in &global_scope.allow {
            allow.push(
              T::deserialize(app, allowed.clone())
                .map_err(|e| crate::Error::CannotDeserializeScope(Box::new(e)))?,
            );
          }
          for denied in &global_scope.deny {
            deny.push(
              T::deserialize(app, denied.clone())
                .map_err(|e| crate::Error::CannotDeserializeScope(Box::new(e)))?,
            );
          }
        }

        let scope = ScopeValue { allow, deny };
        let _ = self.global_scope_cache.set(scope);
        Ok(self.global_scope_cache.get())
      }
    }
  }

  fn get_command_scope_typed<R: Runtime, T: ScopeObject>(
    &self,
    app: &AppHandle<R>,
    key: &ScopeKey,
  ) -> crate::Result<&ScopeValue<T>> {
    let cache = self.command_cache.get(key).unwrap();
    match cache.try_get() {
      Some(cached) => Ok(cached),
      None => {
        let resolved_scope = self
          .command_scope
          .get(key)
          .unwrap_or_else(|| panic!("missing command scope for key {key}"));

        let mut allow: Vec<T> = Vec::new();
        let mut deny: Vec<T> = Vec::new();

        for allowed in &resolved_scope.allow {
          allow.push(
            T::deserialize(app, allowed.clone())
              .map_err(|e| crate::Error::CannotDeserializeScope(Box::new(e)))?,
          );
        }
        for denied in &resolved_scope.deny {
          deny.push(
            T::deserialize(app, denied.clone())
              .map_err(|e| crate::Error::CannotDeserializeScope(Box::new(e)))?,
          );
        }

        let value = ScopeValue { allow, deny };

        let _ = cache.set(value);
        Ok(cache.get())
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use glob::Pattern;
  use tauri_utils::acl::{
    resolved::{CommandKey, Resolved, ResolvedCommand},
    ExecutionContext,
  };

  use crate::ipc::Origin;

  use super::RuntimeAuthority;

  #[test]
  fn window_glob_pattern_matches() {
    let command = CommandKey {
      name: "my-command".into(),
      context: ExecutionContext::Local,
    };
    let window = "main-*";

    let resolved_cmd = ResolvedCommand {
      windows: vec![Pattern::new(window).unwrap()],
      ..Default::default()
    };
    let allowed_commands = [(command.clone(), resolved_cmd.clone())]
      .into_iter()
      .collect();

    let authority = RuntimeAuthority::new(Resolved {
      allowed_commands,
      ..Default::default()
    });

    assert_eq!(
      authority.resolve_access(
        &command.name,
        &window.replace('*', "something"),
        &Origin::Local
      ),
      Some(&resolved_cmd)
    );
  }

  #[test]
  fn remote_domain_matches() {
    let domain = "tauri.app";
    let command = CommandKey {
      name: "my-command".into(),
      context: ExecutionContext::Remote {
        domain: Pattern::new(domain).unwrap(),
      },
    };
    let window = "main";

    let resolved_cmd = ResolvedCommand {
      windows: vec![Pattern::new(window).unwrap()],
      scope: None,
      ..Default::default()
    };
    let allowed_commands = [(command.clone(), resolved_cmd.clone())]
      .into_iter()
      .collect();

    let authority = RuntimeAuthority::new(Resolved {
      allowed_commands,
      ..Default::default()
    });

    assert_eq!(
      authority.resolve_access(
        &command.name,
        window,
        &Origin::Remote {
          domain: domain.into()
        }
      ),
      Some(&resolved_cmd)
    );
  }

  #[test]
  fn remote_domain_glob_pattern_matches() {
    let domain = "tauri.*";
    let command = CommandKey {
      name: "my-command".into(),
      context: ExecutionContext::Remote {
        domain: Pattern::new(domain).unwrap(),
      },
    };
    let window = "main";

    let resolved_cmd = ResolvedCommand {
      windows: vec![Pattern::new(window).unwrap()],
      scope: None,
      ..Default::default()
    };
    let allowed_commands = [(command.clone(), resolved_cmd.clone())]
      .into_iter()
      .collect();

    let authority = RuntimeAuthority::new(Resolved {
      allowed_commands,
      ..Default::default()
    });

    assert_eq!(
      authority.resolve_access(
        &command.name,
        window,
        &Origin::Remote {
          domain: domain.replace('*', "studio")
        }
      ),
      Some(&resolved_cmd)
    );
  }

  #[test]
  fn remote_context_denied() {
    let command = CommandKey {
      name: "my-command".into(),
      context: ExecutionContext::Local,
    };
    let window = "main";

    let resolved_cmd = ResolvedCommand {
      windows: vec![Pattern::new(window).unwrap()],
      scope: None,
      ..Default::default()
    };
    let allowed_commands = [(command.clone(), resolved_cmd.clone())]
      .into_iter()
      .collect();

    let authority = RuntimeAuthority::new(Resolved {
      allowed_commands,
      ..Default::default()
    });

    assert!(authority
      .resolve_access(
        &command.name,
        window,
        &Origin::Remote {
          domain: "tauri.app".into()
        }
      )
      .is_none());
  }

  #[test]
  fn denied_command_takes_precendence() {
    let command = CommandKey {
      name: "my-command".into(),
      context: ExecutionContext::Local,
    };
    let window = "main";
    let windows = vec![Pattern::new(window).unwrap()];
    let allowed_commands = [(
      command.clone(),
      ResolvedCommand {
        windows: windows.clone(),
        ..Default::default()
      },
    )]
    .into_iter()
    .collect();
    let denied_commands = [(
      command.clone(),
      ResolvedCommand {
        windows: windows.clone(),
        ..Default::default()
      },
    )]
    .into_iter()
    .collect();

    let authority = RuntimeAuthority::new(Resolved {
      allowed_commands,
      denied_commands,
      ..Default::default()
    });

    assert!(authority
      .resolve_access(&command.name, window, &Origin::Local)
      .is_none());
  }
}
