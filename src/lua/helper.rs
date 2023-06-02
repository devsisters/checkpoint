//! Lua common helper functions

use mlua::{Lua, MultiValue, Value};

use super::{lua_from_value, lua_to_value};

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
    register_lua_function!("toJsonString", to_json_string);
    register_lua_function!("debugPrint", debug_print);
    register_lua_function!("deepCopy", deepcopy);
    register_lua_function!("jsonPatchDiff", jsonpatch_diff);
    register_lua_function!("startsWith", starts_with);
    register_lua_function!("endsWith", ends_with);
    register_lua_function!("lookup", lookup);

    Ok(())
}

/// Lua helper function to convert table to JSON string
fn to_json_string<'lua>(lua: &'lua Lua, v: Value<'lua>) -> mlua::Result<String> {
    let v_json: serde_json::Value = lua_from_value(lua, v)?;
    let v_json_string = serde_json::to_string(&v_json).map_err(mlua::Error::external)?;
    Ok(v_json_string)
}

/// Lua helper function to debug-print Lua value with JSON format
fn debug_print<'lua>(lua: &'lua Lua, v: Value<'lua>) -> mlua::Result<()> {
    let v_json: serde_json::Value = lua_from_value(lua, v)?;
    tracing::info!(
        "debug print fron Lua code: {}",
        serde_json::to_string(&v_json).map_err(mlua::Error::external)?
    );
    Ok(())
}

// Lua helper function to deep-copy a Lua value
fn deepcopy<'lua>(lua: &'lua Lua, v: Value<'lua>) -> mlua::Result<Value<'lua>> {
    // Convert Lua value to JSON value and convert back to deep-copy
    let v_json: serde_json::Value = lua_from_value(lua, v)?;
    lua_to_value(lua, &v_json)
}

// Lua helper function to generate jsonpatch with diff of two table
fn jsonpatch_diff<'lua>(
    lua: &'lua Lua,
    (v1, v2): (Value<'lua>, Value<'lua>),
) -> mlua::Result<Value<'lua>> {
    let v1_json: serde_json::Value = lua_from_value(lua, v1)?;
    let v2_json: serde_json::Value = lua_from_value(lua, v2)?;
    let patch = json_patch::diff(&v1_json, &v2_json);
    lua_to_value(lua, &patch)
}

// Lua helper function to check first string starts with second string
fn starts_with(_lua: &Lua, (s1, s2): (String, String)) -> mlua::Result<bool> {
    Ok(s1.starts_with(&s2))
}

// Lua helper function to check first string ends with second string
fn ends_with(_lua: &Lua, (s1, s2): (String, String)) -> mlua::Result<bool> {
    Ok(s1.ends_with(&s2))
}

fn lookup<'lua>(
    _lua: &'lua Lua,
    (mut v, keys): (Option<Value<'lua>>, MultiValue<'lua>),
) -> mlua::Result<Option<Value<'lua>>> {
    for key in keys {
        if let Some(Value::Table(t)) = v {
            v = t.get(key)?;
        } else {
            return Ok(None);
        }
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_lua() -> Lua {
        let lua = Lua::new();
        lua.sandbox(true).unwrap();
        register_lua_helper_functions(&lua).unwrap();
        lua
    }

    #[tokio::test]
    async fn test_lookup() {
        let lua = setup_lua();
        let s: String = lua
            .load(
                r#"
object = {foo={bar={{baz="some string"}}}}
return lookup(object, "foo", "bar", 1, "baz")
"#,
            )
            .eval_async()
            .await
            .unwrap();
        assert_eq!(s, "some string");

        let s: Option<String> = lua
            .load(
                r#"
object = {bar="baz"}
return lookup(object, "foo", "bar", 1, "baz")
"#,
            )
            .eval_async()
            .await
            .unwrap();
        assert_eq!(s, None);

        let s: Option<String> = lua
            .load(
                r#"
object = {foo={bar="baz"}}
return lookup(object, "foo", "bar", 1, "baz")
"#,
            )
            .eval_async()
            .await
            .unwrap();
        assert_eq!(s, None);

        let s: Option<String> = lua
            .load(
                r#"
object = "foobar"
return lookup(object, "foo", "bar", 1, "baz")
"#,
            )
            .eval_async()
            .await
            .unwrap();
        assert_eq!(s, None);
    }
}
