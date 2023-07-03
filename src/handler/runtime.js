function kubeGet(args) {
  const serviceAccountInfo = __checkpoint_get_context("serviceAccountInfo");
  const timeoutSeconds = __checkpoint_get_context("timeoutSeconds");
  return Deno.core.ops.ops_kube_get(serviceAccountInfo, timeoutSeconds, args);
}
function kubeList(args) {
  const serviceAccountInfo = __checkpoint_get_context("serviceAccountInfo");
  const timeoutSeconds = __checkpoint_get_context("timeoutSeconds");
  return Deno.core.ops.ops_kube_list(serviceAccountInfo, timeoutSeconds, args);
}
function getRequest() {
  return __checkpoint_get_context("admissionRequest");
}
function allow() {
  const output = __checkpoint_get_context("output");
  __checkpoint_set_context("output", { ...output, denyReason: undefined });
}
function deny(denyReason) {
  const output = __checkpoint_get_context("output");
  __checkpoint_set_context("output", { ...output, denyReason });
}
function mutate(patch) {
  const output = __checkpoint_get_context("output");
  __checkpoint_set_context("output", { ...output, patch });
}
function allowAndMutate(patch) {
  __checkpoint_set_context("output", { denyReason: undefined, patch });
}
__checkpoint_set_context("output", {});
