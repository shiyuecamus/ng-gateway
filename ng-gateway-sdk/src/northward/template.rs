use dashmap::DashMap;
use handlebars::{
    Context as HbContext, Handlebars, Helper, HelperDef, Output, RenderContext, RenderError,
    RenderErrorReason,
};
use once_cell::sync::Lazy;
use serde_json::Value;
use std::sync::RwLock;

/// Render a topic/key template using **Handlebars** syntax.
///
/// Implementation notes:
/// - We cache compiled templates by their original string to avoid recompilation on hot paths.
/// - We keep handlebars **non-strict** to match prior behavior (missing keys -> empty string).
pub fn render_template(template: &str, data: &Value) -> String {
    static HB: Lazy<RwLock<Handlebars<'static>>> = Lazy::new(|| {
        let mut hb = Handlebars::new();
        // Keep missing keys as empty string, consistent with previous behavior.
        hb.set_strict_mode(false);
        // Avoid HTML escaping (topics/keys are plain text)
        hb.register_escape_fn(handlebars::no_escape);
        hb.register_helper("default", Box::new(DefaultHelper));
        RwLock::new(hb)
    });
    static REG: Lazy<DashMap<String, String>> = Lazy::new(DashMap::new); // template -> name

    let name = if let Some(v) = REG.get(template) {
        v.clone()
    } else {
        let processed = template.to_string();
        let key = format!("t_{:x}", hash64(template.as_bytes()));

        // Register only once (best-effort). Never panic on a poisoned lock.
        if let Ok(mut hb) = HB.write() {
            if hb.get_template(&key).is_none() {
                // Best-effort: if registration fails, we fall back to direct render_template below.
                let _ = hb.register_template_string(&key, processed);
            }
        }

        REG.insert(template.to_string(), key.clone());
        key
    };

    // Render with cached template when possible. Never panic on a poisoned lock.
    if let Ok(hb) = HB.read() {
        return hb
            .render(&name, data)
            .or_else(|_| hb.render_template(template, data))
            .unwrap_or_default();
    }

    // Lock poisoned: degrade gracefully by building a local renderer (no cache).
    let mut hb = Handlebars::new();
    hb.set_strict_mode(false);
    hb.register_escape_fn(handlebars::no_escape);
    hb.register_helper("default", Box::new(DefaultHelper));
    hb.render_template(template, data).unwrap_or_default()
}

/// Stable, non-cryptographic hash for cache keys.
fn hash64(bytes: &[u8]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hasher;
    let mut h = DefaultHasher::new();
    h.write(bytes);
    h.finish()
}

/// `{{default value "fallback"}}` helper.
#[derive(Clone, Copy)]
struct DefaultHelper;

impl HelperDef for DefaultHelper {
    fn call<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc HbContext,
        _rc: &mut RenderContext<'reg, 'rc>,
        out: &mut dyn Output,
    ) -> Result<(), RenderError> {
        let v0 = h.param(0).map(|p| p.value());
        let v1 = h.param(1).map(|p| p.value());

        let selected = match v0 {
            None | Some(Value::Null) => v1,
            Some(Value::String(s)) if s.is_empty() => v1,
            _ => v0,
        };

        match selected {
            Some(v) => {
                write!(out, "{}", handlebars::JsonRender::render(v)).map_err(RenderError::from)
            }
            None => Err(RenderErrorReason::Other("default helper requires 2 params".into()).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_basic() {
        let data = json!({"a": "1"});
        assert_eq!(render_template("x.{{a}}.y", &data), "x.1.y");
    }

    #[test]
    fn render_default() {
        let data = json!({});
        assert_eq!(
            render_template("x.{{default missing \"d\"}}.y", &data),
            "x.d.y"
        );
    }
}
