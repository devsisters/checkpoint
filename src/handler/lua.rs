use kube::{
    api::{ApiResource, ListParams},
    core::{admission::AdmissionRequest, DynamicObject, GroupVersionKind, Object},
    Api, Client,
};
use mlua::{Lua, LuaSerdeExt, Value};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use super::Error;

pub(super) async fn eval_lua_code<T>(
    code: String,
    admission_req: AdmissionRequest<DynamicObject>,
) -> Result<T, Error>
where
    for<'a> T: mlua::FromLuaMulti<'a> + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(Error::CreateRuntime)
            .and_then(|runtime| {
                runtime.block_on(async move {
                    let lua = prepare_lua_ctx()?;

                    // Serialize AdmissionRequest to Lua value
                    let admission_req_lua_value = lua
                        .to_value_with(
                            &admission_req,
                            mlua::SerializeOptions::new()
                                .serialize_none_to_null(false)
                                .serialize_unit_to_null(false),
                        )
                        .map_err(Error::ConvertAdmissionRequestToLuaValue)?;

                    // Load Lua code chunk
                    let lua_chunk = lua
                        .load(&code)
                        .set_name("rule code")
                        .map_err(Error::SetLuaCodeName)?;

                    // Evaluate Lua code chunk as a function
                    let output = lua_chunk
                        .call_async(admission_req_lua_value)
                        .await
                        .map_err(Error::LuaEval)?;
                    Ok(output)
                })
            });
        let _ = tx.send(result);
    });
    rx.await.map_err(Error::RecvLuaThread)?
}

fn prepare_lua_ctx() -> Result<Lua, Error> {
    let lua = Lua::new();

    lua.sandbox(true).map_err(Error::SetLuaSandbox)?;

    {
        let globals = lua.globals();

        macro_rules! register_lua_function {
            ($name:literal, $func:ident) => {
                let f = lua
                    .create_function($func)
                    .map_err(Error::CreateLuaFunction)?;
                globals.set($name, f).map_err(Error::SetLuaValue)?;
            };
            ($name:literal, $func:ident, async) => {
                let f = lua
                    .create_async_function($func)
                    .map_err(Error::CreateLuaFunction)?;
                globals.set($name, f).map_err(Error::SetLuaValue)?;
            };
        }

        register_lua_function!("debug_print", lua_debug_print);
        register_lua_function!("deepcopy", lua_deepcopy);
        register_lua_function!("jsonpatch_diff", lua_jsonpatch_diff);
        register_lua_function!("kube_get", lua_kube_get, async);
        register_lua_function!("kube_list", lua_kube_list, async);
    }

    Ok(lua)
}

fn lua_debug_print<'lua>(lua: &'lua Lua, v: Value<'lua>) -> mlua::Result<()> {
    let v_json: serde_json::Value = lua.from_value(v)?;
    tracing::info!(
        "debug print fron Lua code: {}",
        serde_json::to_string(&v_json).map_err(mlua::Error::external)?
    );
    Ok(())
}

fn lua_deepcopy<'lua>(lua: &'lua Lua, v: Value<'lua>) -> mlua::Result<Value<'lua>> {
    let v_json: serde_json::Value = lua.from_value(v)?;
    lua.to_value_with(
        &v_json,
        mlua::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}

fn lua_jsonpatch_diff<'lua>(
    lua: &'lua Lua,
    (v1, v2): (Value<'lua>, Value<'lua>),
) -> mlua::Result<Value<'lua>> {
    let v1_json: serde_json::Value = lua.from_value(v1)?;
    let v2_json: serde_json::Value = lua.from_value(v2)?;
    let patch = json_patch::diff(&v1_json, &v2_json);
    lua.to_value_with(
        &patch,
        mlua::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}

#[derive(Deserialize, Debug, Clone)]
struct KubeGetArgument {
    group: String,
    version: String,
    kind: String,
    plural: Option<String>,
    namespace: Option<String>,
    name: String,
}

async fn lua_kube_get<'lua>(lua: &'lua Lua, argument: Value<'lua>) -> mlua::Result<Value<'lua>> {
    let KubeGetArgument {
        group,
        version,
        kind,
        plural,
        namespace,
        name,
    } = lua.from_value(argument)?;

    let gvk = GroupVersionKind::gvk(&group, &version, &kind);
    let ar = if let Some(plural) = plural {
        ApiResource::from_gvk_with_plural(&gvk, &plural)
    } else {
        ApiResource::from_gvk(&gvk)
    };

    let client = Client::try_default().await.map_err(mlua::Error::external)?;
    let api = if let Some(namespace) = namespace {
        Api::<Object<JsonValue, JsonValue>>::namespaced_with(client, &namespace, &ar)
    } else {
        Api::<Object<JsonValue, JsonValue>>::all_with(client, &ar)
    };
    let object = api.get_opt(&name).await.map_err(mlua::Error::external)?;

    lua.to_value_with(
        &object,
        mlua::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}

#[derive(Deserialize, Debug, Clone)]
struct KubeListArgument {
    group: String,
    version: String,
    kind: String,
    plural: Option<String>,
    namespace: Option<String>,
    list_params: Option<KubeListArgumentListParams>,
}

fn default_kube_list_argument_list_params_bookmarks() -> bool {
    true
}

#[derive(Deserialize, Debug, Clone)]
struct KubeListArgumentListParams {
    label_selector: Option<String>,
    field_selector: Option<String>,
    timeout: Option<u32>,
    #[serde(default = "default_kube_list_argument_list_params_bookmarks")]
    bookmarks: bool,
    limit: Option<u32>,
    continue_token: Option<String>,
}

async fn lua_kube_list<'lua>(lua: &'lua Lua, argument: Value<'lua>) -> mlua::Result<Value<'lua>> {
    let KubeListArgument {
        group,
        version,
        kind,
        plural,
        namespace,
        list_params,
    } = lua.from_value(argument)?;
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

    let gvk = GroupVersionKind::gvk(&group, &version, &kind);
    let ar = if let Some(plural) = plural {
        ApiResource::from_gvk_with_plural(&gvk, &plural)
    } else {
        ApiResource::from_gvk(&gvk)
    };

    let client = Client::try_default().await.map_err(mlua::Error::external)?;
    let api = if let Some(namespace) = namespace {
        Api::<Object<JsonValue, JsonValue>>::namespaced_with(client, &namespace, &ar)
    } else {
        Api::<Object<JsonValue, JsonValue>>::all_with(client, &ar)
    };
    let object = api
        .list(&list_params)
        .await
        .map_err(mlua::Error::external)?;

    lua.to_value_with(
        &object,
        mlua::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}
