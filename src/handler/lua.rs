use kube::core::{admission::AdmissionRequest, DynamicObject};
use mlua::{Lua, LuaSerdeExt};

use super::Error;

pub(super) fn prepare_lua_ctx(
    admission_req: &AdmissionRequest<DynamicObject>,
) -> Result<Lua, Error> {
    let lua = Lua::new();

    lua.sandbox(true).map_err(Error::SetLuaSandbox)?;

    {
        let globals = lua.globals();

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
