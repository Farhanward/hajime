//! `code`, the JavaScript node. Fifteen instances across the imported
//! workflows, and every active one depends on it.
//!
//! Runs on QuickJS through `rquickjs`. QuickJS was chosen over a pure-Rust
//! engine because this code was written against Node inside n8n and leans on
//! real language coverage: tagged regex with unicode classes, `Math.imul`,
//! spread, optional chaining and template literals all appear in the export.
//!
//! The sandbox has no filesystem, no network and no timers. `require()` is
//! defined only so it throws a clear error: the two nodes that call it pull in
//! `child_process` to run shell commands, both live in disabled workflows, and
//! reproducing that capability inside a workflow engine is not something to do
//! by accident.

use super::{Effect, ExecContext, NodeError, NodeExecutor, NodeOutput};
use crate::model::{Item, Node};
use async_trait::async_trait;
use rquickjs::{Context, Runtime};
use std::time::{Duration, Instant};

/// Ceiling on a single node's run. n8n has no default limit; an unbounded loop
/// there stalls a worker, and here it would stall the executor.
const TIME_LIMIT: Duration = Duration::from_secs(30);
/// 64 MB is far above what the imported nodes need and well below the budget
/// the whole service is expected to live in.
const MEMORY_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub struct Code;

impl Code {
    fn source(node: &Node) -> Result<&str, NodeError> {
        node.param_str("jsCode")
            .or_else(|| node.param_str("functionCode"))
            .ok_or(NodeError::MissingParameter("jsCode"))
    }

    /// n8n runs the script once for the whole list, or once per item.
    fn per_item(node: &Node) -> bool {
        matches!(
            node.param_str("mode").unwrap_or("runOnceForAllItems"),
            "runOnceForEachItem"
        )
    }

    /// Build the wrapper that supplies n8n's globals.
    ///
    /// Data crosses the boundary as JSON text rather than through a native
    /// binding: it is one conversion either way, and it keeps a malformed value
    /// from reaching the engine as a half-built object.
    fn wrap(user_code: &str, input_json: &str, siblings_json: &str) -> String {
        format!(
            r#"(function () {{
  const __items = JSON.parse({input});
  const __nodes = JSON.parse({siblings});

  globalThis.items = __items;
  globalThis.$input = {{
    all:   function () {{ return __items; }},
    first: function () {{ return __items[0]; }},
    last:  function () {{ return __items[__items.length - 1]; }},
  }};
  globalThis.$json = __items.length ? __items[0].json : {{}};
  globalThis.$items = function (name) {{
    if (!(name in __nodes)) {{
      throw new Error("$items('" + name + "'): that node has not produced output");
    }}
    return __nodes[name];
  }};
  globalThis.$node = __nodes;
  globalThis.require = function (m) {{
    throw new Error("require('" + m + "') is not available in this sandbox");
  }};

{code}
}})()"#,
            input = js_string(input_json),
            siblings = js_string(siblings_json),
            code = user_code,
        )
    }

    /// Convert node output back into items, accepting every shape n8n does:
    /// a list of `{json: ...}`, a list of bare objects, or a single object.
    fn to_items(value: serde_json::Value) -> Vec<Item> {
        fn one(v: serde_json::Value) -> Item {
            match v.get("json") {
                Some(inner) => Item::from_json(inner.clone()),
                None => Item::from_json(v),
            }
        }
        match value {
            serde_json::Value::Array(list) => list.into_iter().map(one).collect(),
            serde_json::Value::Null => Vec::new(),
            other => vec![one(other)],
        }
    }

    fn eval(source: &str) -> Result<serde_json::Value, NodeError> {
        let runtime =
            Runtime::new().map_err(|e| NodeError::Other(format!("js runtime: {e}")))?;
        runtime.set_memory_limit(MEMORY_LIMIT);

        let deadline = Instant::now() + TIME_LIMIT;
        runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));

        let context = Context::full(&runtime)
            .map_err(|e| NodeError::Other(format!("js context: {e}")))?;

        context.with(|ctx| {
            let value: rquickjs::Value = ctx.eval(source).map_err(|e| {
                // A thrown Error carries the useful text; the raw error does not.
                let detail = ctx
                    .catch()
                    .as_exception()
                    .and_then(|ex| ex.message())
                    .unwrap_or_else(|| e.to_string());
                NodeError::Other(format!("code node failed: {detail}"))
            })?;

            if value.is_undefined() || value.is_null() {
                return Ok(serde_json::Value::Null);
            }

            let json: rquickjs::Object = ctx
                .globals()
                .get("JSON")
                .map_err(|e| NodeError::Other(format!("JSON unavailable: {e}")))?;
            let stringify: rquickjs::Function = json
                .get("stringify")
                .map_err(|e| NodeError::Other(format!("JSON.stringify: {e}")))?;
            let text: String = stringify
                .call((value,))
                .map_err(|_| NodeError::Other("code node returned a value that is not JSON serialisable".into()))?;

            serde_json::from_str(&text)
                .map_err(|e| NodeError::Other(format!("could not read node result: {e}")))
        })
    }
}

/// Quote a Rust string as a JavaScript string literal.
fn js_string(raw: &str) -> String {
    serde_json::Value::String(raw.to_string()).to_string()
}

#[async_trait]
impl NodeExecutor for Code {
    /// Sandboxed JavaScript with no `require` and no host bindings, so it can
    /// compute but cannot reach anything.
    fn effect(&self, _node: &Node) -> Effect {
        Effect::Read
    }

    async fn execute(
        &self,
        node: &Node,
        input: Vec<Item>,
        ctx: &ExecContext<'_>,
    ) -> Result<NodeOutput, NodeError> {
        let user_code = Self::source(node)?;

        // `$items('Name')` reaches nodes that already ran.
        let siblings = ctx.sibling_map();
        let siblings_json = serde_json::to_string(&siblings)
            .map_err(|e| NodeError::Other(e.to_string()))?;

        if Self::per_item(node) {
            let mut out = Vec::with_capacity(input.len());
            for item in input {
                let payload = serde_json::to_string(&vec![item])
                    .map_err(|e| NodeError::Other(e.to_string()))?;
                let source = Self::wrap(user_code, &payload, &siblings_json);
                out.extend(Self::to_items(Self::eval(&source)?));
            }
            return Ok(NodeOutput::items(out));
        }

        let payload =
            serde_json::to_string(&input).map_err(|e| NodeError::Other(e.to_string()))?;
        let source = Self::wrap(user_code, &payload, &siblings_json);
        Ok(NodeOutput::items(Self::to_items(Self::eval(&source)?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(code: &str) -> Node {
        serde_json::from_value(json!({
            "name": "Code",
            "type": "n8n-nodes-base.code",
            "parameters": {"jsCode": code},
        }))
        .unwrap()
    }

    async fn run(code: &str, input: Vec<Item>) -> Result<Vec<Item>, NodeError> {
        Code.execute(&node(code), input, &ExecContext::default())
            .await
            .map(|o| o.items)
    }

    #[tokio::test]
    async fn returns_a_list_of_json_wrapped_items() {
        let out = run("return [{json: {a: 1}}, {json: {a: 2}}];", vec![])
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].json["a"], 1);
        assert_eq!(out[1].json["a"], 2);
    }

    #[tokio::test]
    async fn accepts_bare_objects_and_a_single_object() {
        let bare = run("return [{a: 1}];", vec![]).await.unwrap();
        assert_eq!(bare[0].json["a"], 1);

        let single = run("return {json: {b: 2}};", vec![]).await.unwrap();
        assert_eq!(single[0].json["b"], 2);
    }

    #[tokio::test]
    async fn exposes_the_items_global_used_by_nine_nodes() {
        let input = vec![
            Item::from_json(json!({"n": 1})),
            Item::from_json(json!({"n": 2})),
        ];
        let out = run(
            "return items.map(i => ({json: {doubled: i.json.n * 2}}));",
            input,
        )
        .await
        .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].json["doubled"], 2);
        assert_eq!(out[1].json["doubled"], 4);
    }

    #[tokio::test]
    async fn exposes_input_helpers_and_json() {
        let input = vec![Item::from_json(json!({"v": "x"}))];
        let out = run(
            "return [{json: {a: $input.all().length, b: $input.first().json.v, c: $json.v}}];",
            input,
        )
        .await
        .unwrap();
        assert_eq!(out[0].json["a"], 1);
        assert_eq!(out[0].json["b"], "x");
        assert_eq!(out[0].json["c"], "x");
    }

    #[tokio::test]
    async fn supports_the_language_features_the_export_relies_on() {
        // Arrow functions, template literals, spread, optional chaining,
        // try/catch, Math.imul and unicode regex all appear in real nodes.
        let code = r#"
            const base = {a: 1};
            const merged = {...base, b: 2};
            const name = `v=${merged.b}`;
            const hash = Math.imul(2166136261, 16777619) >>> 0;
            const cleaned = "```json\nplain".replace(/```[a-z]*\s*/gi, '');
            const arabic = "مرحبا بالعالم".replace(/بالعالم/u, 'يا فرحان');
            let caught = null;
            try { null.x; } catch (e) { caught = 'ok'; }
            const deep = {x: {y: 5}};
            return [{json: {name, hash, cleaned, arabic, caught, opt: deep?.x?.y ?? 0}}];
        "#;
        let out = run(code, vec![]).await.unwrap();
        let j = &out[0].json;
        assert_eq!(j["name"], "v=2");
        assert_eq!(j["cleaned"], "plain");
        assert_eq!(j["arabic"], "مرحبا يا فرحان");
        assert_eq!(j["caught"], "ok");
        assert_eq!(j["opt"], 5);
        assert!(j["hash"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn per_item_mode_runs_once_for_each_item() {
        let n: Node = serde_json::from_value(json!({
            "name": "Code",
            "type": "n8n-nodes-base.code",
            "parameters": {"mode": "runOnceForEachItem", "jsCode": "return [{json: {seen: items.length}}];"},
        }))
        .unwrap();
        let input = vec![
            Item::from_json(json!({"n": 1})),
            Item::from_json(json!({"n": 2})),
            Item::from_json(json!({"n": 3})),
        ];
        let out = Code.execute(&n, input, &ExecContext::default()).await.unwrap();
        assert_eq!(out.items.len(), 3);
        // Each run sees exactly one item.
        assert!(out.items.iter().all(|i| i.json["seen"] == 1));
    }

    #[tokio::test]
    async fn a_thrown_error_carries_its_message() {
        let err = run("throw new Error('deliberate failure');", vec![])
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("deliberate failure"),
            "unhelpful error: {err}"
        );
    }

    #[tokio::test]
    async fn a_syntax_error_is_reported_not_silently_empty() {
        assert!(run("this is not javascript {{{", vec![]).await.is_err());
    }

    #[tokio::test]
    async fn require_is_refused_with_a_clear_message() {
        let err = run("const cp = require('child_process'); return [];", vec![])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("child_process"), "got: {err}");
        assert!(err.to_string().contains("not available"), "got: {err}");
    }

    #[tokio::test]
    async fn there_is_no_filesystem_or_network_in_the_sandbox() {
        for probe in ["return [{json:{v: typeof fetch}}];", "return [{json:{v: typeof process}}];"] {
            let out = run(probe, vec![]).await.unwrap();
            assert_eq!(out[0].json["v"], "undefined");
        }
    }

    #[tokio::test]
    async fn an_endless_loop_is_interrupted_rather_than_hanging() {
        let err = run("while (true) {}", vec![]).await.unwrap_err();
        assert!(err.to_string().to_lowercase().contains("interrupt")
            || err.to_string().to_lowercase().contains("failed"),
            "got: {err}");
    }

    #[tokio::test]
    async fn returning_nothing_yields_no_items() {
        assert!(run("const x = 1;", vec![]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_source_is_an_error() {
        let n: Node = serde_json::from_value(json!({
            "name": "Code", "type": "n8n-nodes-base.code", "parameters": {}
        }))
        .unwrap();
        let err = Code
            .execute(&n, vec![], &ExecContext::default())
            .await
            .unwrap_err();
        assert!(matches!(err, NodeError::MissingParameter("jsCode")));
    }
}
