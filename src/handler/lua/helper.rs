//! Lua helper functions for rules

use kube::{
    api::ListParams,
    core::{DynamicObject, GroupVersionKind},
    discovery::ApiResource,
    Api,
};
use mlua::{Lua, Value};
use serde::Deserialize;

use crate::lua::{lua_from_value, lua_to_value};

use super::extract_kube_client_from_lua_ctx;

pub fn register_lua_helper_functions(lua: &Lua) -> Result<(), mlua::Error> {
    let globals = lua.globals();

    macro_rules! register_lua_function {
        ($name:literal, $func:ident) => {
            let f = lua.create_function($func)?;
            globals.set($name, f)?;
        };
        ($name:literal, $func:ident, async) => {
            let f = lua.create_async_function($func)?;
            globals.set($name, f)?;
        };
    }

    // Register all Lua helper functions
    register_lua_function!("kubeGet", kube_get, async);
    register_lua_function!("kubeList", kube_list, async);

    Ok(())
}

#[derive(Deserialize, Debug, Clone, Hash, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KubeGetArgument {
    pub group: String,
    pub version: String,
    pub kind: String,
    pub plural: Option<String>,
    pub namespace: Option<String>,
    pub name: String,
}

/// Lua helper function to get a Kubernetes resource
async fn kube_get<'lua>(lua: &'lua Lua, argument: Value<'lua>) -> mlua::Result<Value<'lua>> {
    // Unpack argument
    let KubeGetArgument {
        group,
        version,
        kind,
        plural,
        namespace,
        name,
    } = lua_from_value(lua, argument)?;

    // Prepare GroupVersionKind and ApiResource from argument
    let gvk = GroupVersionKind::gvk(&group, &version, &kind);
    let ar = if let Some(plural) = plural {
        ApiResource::from_gvk_with_plural(&gvk, &plural)
    } else {
        ApiResource::from_gvk(&gvk)
    };

    let client = extract_kube_client_from_lua_ctx(lua)?;

    // Prepare Kubernetes API with or without namespace
    let api = if let Some(namespace) = namespace {
        Api::<DynamicObject>::namespaced_with(client, &namespace, &ar)
    } else {
        Api::<DynamicObject>::all_with(client, &ar)
    };

    // Get object
    let object = api.get_opt(&name).await.map_err(mlua::Error::external)?;

    // Serialize object into Lua value
    lua_to_value(lua, &object)
}

#[derive(Deserialize, Debug, Clone, Hash, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KubeListArgument {
    pub group: String,
    pub version: String,
    pub kind: String,
    pub plural: Option<String>,
    pub namespace: Option<String>,
    pub list_params: Option<KubeListArgumentListParams>,
}

fn default_kube_list_argument_list_params_bookmarks() -> bool {
    true
}

#[derive(Deserialize, Debug, Clone, Hash, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KubeListArgumentListParams {
    pub label_selector: Option<String>,
    pub field_selector: Option<String>,
    pub timeout: Option<u32>,
    #[serde(default = "default_kube_list_argument_list_params_bookmarks")]
    pub bookmarks: bool,
    pub limit: Option<u32>,
    pub continue_token: Option<String>,
}

/// Lua helper function to list Kubernetes resources
async fn kube_list<'lua>(lua: &'lua Lua, argument: Value<'lua>) -> mlua::Result<Value<'lua>> {
    // Unpack argument
    let KubeListArgument {
        group,
        version,
        kind,
        plural,
        namespace,
        list_params,
    } = lua_from_value(lua, argument)?;
    let list_params = list_params
        .map(
            |KubeListArgumentListParams {
                 label_selector,
                 field_selector,
                 timeout,
                 bookmarks,
                 limit,
                 continue_token,
             }| ListParams {
                label_selector,
                field_selector,
                timeout,
                bookmarks,
                limit,
                continue_token,
            },
        )
        .unwrap_or_default();

    // Prepare GroupVersionKind and ApiResource from argument
    let gvk = GroupVersionKind::gvk(&group, &version, &kind);
    let ar = if let Some(plural) = plural {
        ApiResource::from_gvk_with_plural(&gvk, &plural)
    } else {
        ApiResource::from_gvk(&gvk)
    };

    let client = extract_kube_client_from_lua_ctx(lua)?;

    // Prepare Kubernetes API with or without namespace
    let api = if let Some(namespace) = namespace {
        Api::<DynamicObject>::namespaced_with(client, &namespace, &ar)
    } else {
        Api::<DynamicObject>::all_with(client, &ar)
    };

    // List objects
    let object_list = api
        .list(&list_params)
        .await
        .map_err(mlua::Error::external)?;

    // Serialize object list into Lua value
    lua_to_value(lua, &object_list)
}
