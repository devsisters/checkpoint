use kube::core::{admission::AdmissionRequest, DynamicObject};
use mlua::{Lua, LuaSerdeExt, Value};

use super::Error;

pub(super) fn prepare_lua_ctx(
    admission_req: &AdmissionRequest<DynamicObject>,
) -> Result<Lua, Error> {
    let lua = Lua::new();

    lua.sandbox(true).map_err(Error::SetLuaSandbox)?;

    {
        let deepcopy = lua
            .create_function(lua_deepcopy)
            .map_err(Error::CreateLuaFunction)?;
        let jsonpatch_diff = lua
            .create_function(lua_jsonpatch_diff)
            .map_err(Error::CreateLuaFunction)?;

        let globals = lua.globals();

        globals
            .set("jsonpatch_diff", jsonpatch_diff)
            .map_err(Error::SetLuaValue)?;
        globals
            .set("deepcopy", deepcopy)
            .map_err(Error::SetLuaValue)?;

        globals
            .set(
                "request",
                lua.to_value(admission_req)
                    .map_err(Error::ConvertAdmissionRequestToLuaValue)?,
            )
            .map_err(Error::SetGlobalAdmissionRequestValue)?;
    }

    Ok(lua)
}

fn lua_deepcopy<'lua>(lua: &'lua Lua, v: Value<'lua>) -> mlua::Result<Value<'lua>> {
    let v_json: serde_json::Value = lua.from_value(v)?;
    lua.to_value(&v_json)
}

fn lua_jsonpatch_diff<'lua>(
    lua: &'lua Lua,
    (v1, v2): (Value<'lua>, Value<'lua>),
) -> mlua::Result<Value<'lua>> {
    let v1_json: serde_json::Value = lua.from_value(v1)?;
    let v2_json: serde_json::Value = lua.from_value(v2)?;
    let patch = json_patch::diff(&v1_json, &v2_json);
    lua.to_value(&patch)
}
