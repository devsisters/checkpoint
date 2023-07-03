globalThis.__checkpoint_context = {};
function __checkpoint_get_context(key) {
  return globalThis.__checkpoint_context[key];
}
function __checkpoint_set_context(key, value) {
  globalThis.__checkpoint_context[key] = value;
}
function print(value) {
  Deno.core.ops.ops_print(value);
}
globalThis.console = {
  log: (value) => {
    print(value);
  },
};
function jsonPatchDiff(v1, v2) {
  return Deno.core.ops.ops_jsonpatch_diff(v1, v2);
}
function jsonClone(value) {
  return Deno.core.ops.ops_json_clone(value);
}
