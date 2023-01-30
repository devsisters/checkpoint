pub mod helper;

use mlua::{Lua, LuaSerdeExt, Value};
use serde::{Deserialize, Serialize};

pub fn lua_to_value<'lua, T>(lua: &'lua Lua, value: &T) -> mlua::Result<Value<'lua>>
where
    T: Serialize + ?Sized,
{
    lua.to_value_with(
        value,
        mlua::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}

pub fn lua_from_value<'lua, T>(lua: &'lua Lua, value: Value<'lua>) -> mlua::Result<T>
where
    T: Deserialize<'lua>,
{
    lua.from_value_with(
        value,
        mlua::DeserializeOptions::new()
            .deny_unsupported_types(false)
            .deny_recursive_tables(false),
    )
}

pub fn prepare_lua_ctx() -> mlua::Result<Lua> {
    let lua = Lua::new();

    // Enable sandbox mode
    lua.sandbox(true)?;

    helper::register_lua_helper_functions(&lua)?;

    Ok(lua)
}
