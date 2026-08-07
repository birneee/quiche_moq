use wasm_bindgen::JsValue;
use web_sys::js_sys::Reflect;

pub(crate) fn js_bool(obj: &JsValue, key: &str) -> bool {
    Reflect::get(obj, &key.into())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}
