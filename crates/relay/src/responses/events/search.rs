//! app-server executes web.run itself and reports its result separately from
//! model output. Keep this lifecycle separate from client function callbacks.
use super::{Output, protocol};
use crate::error::{Error, Result};
use serde_json::{Value, json};

pub(super) struct NativeSearch {
    index: usize,
    completed: bool,
    output_received: bool,
}

impl NativeSearch {
    pub(super) fn pending(&self) -> bool {
        !self.completed || !self.output_received
    }
}

impl Output {
    pub(super) fn native_search_started(
        &mut self,
        raw: &Value,
        out: &mut Vec<Value>,
    ) -> Result<()> {
        let arguments = raw["arguments"].as_str().ok_or_else(protocol)?;
        if !serde_json::from_str::<Value>(arguments).is_ok_and(|value| value.is_object()) {
            return Err(protocol());
        }
        let call = self.call_id(raw)?.to_owned();
        let index = self.items.len();
        // This changes item type, so do not reuse the native function's fc_ ID.
        let item = json!({"type":"web_search_call","id":format!("ws_{}",uuid::Uuid::new_v4().simple()),"status":"in_progress"});
        self.items.push(item.clone());
        self.native_searches.insert(
            call,
            NativeSearch {
                index,
                completed: false,
                output_received: false,
            },
        );
        self.bytes += arguments.len();
        self.check_budget()?;
        self.first.get_or_insert(crate::now_ms());
        out.push(
            self.event(
                json!({"type":"response.output_item.added","output_index":index,"item":item}),
            ),
        );
        Ok(())
    }

    pub(super) fn native_search_completed(
        &mut self,
        native: &Value,
        out: &mut Vec<Value>,
    ) -> Result<()> {
        let Some(call) = native["id"].as_str() else {
            return Err(protocol());
        };
        let Some(search) = self.native_searches.get_mut(call) else {
            // Hosted web_search_call also produces UI events; its raw item is
            // already handled by the ordinary server-tool conversion.
            return Ok(());
        };
        if search.completed {
            return Err(protocol());
        }
        if (!native["status"].is_null() && native["status"] != "completed")
            || !native["action"].is_object()
        {
            return Err(Error::new(
                "UPSTREAM_TOOL_FAILED",
                "原生网页搜索未完成，未重发请求。",
            ));
        }
        let mut action = native["action"].clone();
        match action["type"].as_str().ok_or_else(protocol)? {
            "openPage" => action["type"] = json!("open_page"),
            "findInPage" => action["type"] = json!("find_in_page"),
            _ => {}
        }
        if !native["results"].is_null() {
            let results = native["results"].as_array().ok_or_else(protocol)?;
            let sources: Vec<_> = results
                .iter()
                .filter_map(|result| {
                    let url = result["url"].as_str()?;
                    let mut source = json!({"type":"url","url":url});
                    if let Some(title) = result["title"].as_str() {
                        source["title"] = json!(title);
                    }
                    Some(source)
                })
                .collect();
            action["sources"] = json!(sources);
        }
        self.bytes += serde_json::to_vec(&action)?.len();
        self.items[search.index]["action"] = action;
        search.completed = true;
        self.finish_native_search(call, out)
    }

    pub(super) fn native_search_output(&mut self, raw: &Value, out: &mut Vec<Value>) -> Result<()> {
        let call = raw["call_id"].as_str().ok_or_else(protocol)?;
        let search = self.native_searches.get_mut(call).ok_or_else(protocol)?;
        if search.output_received || !(raw["output"].is_string() || raw["output"].is_array()) {
            return Err(protocol());
        }
        search.output_received = true;
        // Account for internal output without exposing it as a client callback
        // or retaining the full search text in the public response.
        self.bytes += serde_json::to_vec(&raw["output"])?.len();
        self.finish_native_search(call, out)
    }

    fn finish_native_search(&mut self, call: &str, out: &mut Vec<Value>) -> Result<()> {
        let search = &self.native_searches[call];
        if search.completed && search.output_received {
            let index = search.index;
            self.items[index]["status"] = json!("completed");
            let item = self.items[index].clone();
            self.finished
                .insert(item["id"].as_str().ok_or_else(protocol)?.to_owned());
            out.push(self.event(
                json!({"type":"response.output_item.done","output_index":index,"item":item}),
            ));
        }
        self.check_budget()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::responses::Request;

    fn request() -> Request {
        Request::parse(json!({"model":"m","input":"search","tools":[{"type":"web_search"}]}))
            .unwrap()
    }
    fn call() -> Value {
        json!({"method":"rawResponseItem/completed","params":{"item":{
            "type":"function_call","id":"fc_search","call_id":"search","namespace":"web","name":"run","arguments":"{\"search_query\":[{\"q\":\"fixture\"}]}"
        }}})
    }
    fn completed() -> Value {
        json!({"method":"item/completed","params":{"item":{
            "type":"webSearch","id":"search","action":{"type":"search","query":"fixture"},
            "results":[{"ref_id":"turn0search0","url":"https://example.test","title":"Fixture"}]
        }}})
    }
    fn result() -> Value {
        json!({"method":"rawResponseItem/completed","params":{"item":{
            "type":"function_call_output","id":"fco_search","call_id":"search","output":[{"type":"input_text","text":"Native search result"}]
        }}})
    }

    #[test]
    fn native_search_waits_for_execution_and_output_in_either_order() {
        for reversed in [false, true] {
            let r = request();
            let mut output = Output::new(&r);
            let events = output.consume(&call(), &r).unwrap();
            assert_eq!(events[0]["item"]["type"], "web_search_call");
            let id = events[0]["item"]["id"].as_str().unwrap().to_owned();
            assert!(id.starts_with("ws_"));
            assert!(!output.has_calls());
            assert!(output.has_pending_native_searches());
            let (first, second) = if reversed {
                (result(), completed())
            } else {
                (completed(), result())
            };
            assert!(output.consume(&first, &r).unwrap().is_empty());
            assert_eq!(output.completed().unwrap_err().code, "UPSTREAM_TOOL_FAILED");
            let events = output.consume(&second, &r).unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0]["type"], "response.output_item.done");
            assert_eq!(events[0]["item"]["id"], id);
            assert!(!output.has_pending_native_searches());
            assert!(!output.has_calls());
            let done = output.completed().unwrap();
            assert_eq!(done["type"], "response.completed");
            let item = &done["response"]["output"][0];
            assert_eq!(item["action"]["sources"][0]["url"], "https://example.test");
            assert_eq!(item["action"]["sources"][0]["title"], "Fixture");
            assert_eq!(done["response"]["output"].as_array().unwrap().len(), 1);
            assert!(!done.to_string().contains("Native search result"));
            assert!(output.first.is_some());
            let mut history = vec![item.clone()];
            history.push(json!({"role":"user","content":"next"}));
            Request::parse(json!({"model":"m","input":history})).unwrap();
        }
    }

    #[test]
    fn native_search_does_not_relax_client_tool_scope() {
        for body in [
            json!({"model":"m","input":"x"}),
            json!({"model":"m","input":"x","tools":[{"type":"web_search"}],"tool_choice":"none"}),
        ] {
            let r = Request::parse(body).unwrap();
            assert_eq!(
                Output::new(&r).consume(&call(), &r).unwrap_err().code,
                "NATIVE_TOOL_SCOPE"
            );
        }
        let r = request();
        for (namespace, name) in [("web", "other"), ("other", "run")] {
            let mut event = call();
            event["params"]["item"]["namespace"] = json!(namespace);
            event["params"]["item"]["name"] = json!(name);
            assert_eq!(
                Output::new(&r).consume(&event, &r).unwrap_err().code,
                "NATIVE_TOOL_SCOPE"
            );
        }
        let mut output = Output::new(&r);
        output.consume(&call(), &r).unwrap();
        let mut unknown = result();
        unknown["params"]["item"]["call_id"] = json!("unrelated");
        assert!(output.consume(&unknown, &r).is_err());
    }

    #[test]
    fn native_search_rejects_duplicates_failures_and_missing_completion() {
        let r = request();
        for duplicate in [call(), completed(), result()] {
            let mut output = Output::new(&r);
            for event in [call(), completed(), result()] {
                output.consume(&event, &r).unwrap();
            }
            assert!(output.consume(&duplicate, &r).is_err());
        }
        for field in ["status", "action"] {
            let mut output = Output::new(&r);
            output.consume(&call(), &r).unwrap();
            let mut failed = completed();
            failed["params"]["item"][field] = if field == "status" {
                json!("failed")
            } else {
                Value::Null
            };
            assert_eq!(
                output.consume(&failed, &r).unwrap_err().code,
                "UPSTREAM_TOOL_FAILED"
            );
        }
        let mut output = Output::new(&r);
        output.consume(&call(), &r).unwrap();
        assert_eq!(output.completed().unwrap_err().code, "UPSTREAM_TOOL_FAILED");
        output.interrupted();
        assert!(output.items().is_empty());
        assert_eq!(output.completed().unwrap()["type"], "response.incomplete");
    }

    #[test]
    fn native_search_preserves_page_actions_and_client_web_aliases() {
        let r = request();
        for (native, public) in [
            ("openPage", "open_page"),
            ("findInPage", "find_in_page"),
            ("other", "other"),
        ] {
            let mut output = Output::new(&r);
            output.consume(&call(), &r).unwrap();
            let mut done = completed();
            done["params"]["item"]["action"] =
                json!({"type":native,"url":"https://example.test","pattern":"text"});
            output.consume(&done, &r).unwrap();
            output.consume(&result(), &r).unwrap();
            assert_eq!(output.items()[0]["action"]["type"], public);
            assert_eq!(output.items()[0]["action"]["url"], "https://example.test");
        }
        let r = Request::parse(json!({"model":"m","input":"x","tools":[
            {"type":"web_search"},
            {"type":"namespace","name":"web","description":"client","tools":[{"type":"function","name":"run"}]}
        ]})).unwrap();
        let mut event = call();
        event["params"]["item"]["namespace"] = r.dynamic_tools()[0]["name"].clone();
        let mut output = Output::new(&r);
        let events = output.consume(&event, &r).unwrap();
        assert_eq!(events.last().unwrap()["item"]["type"], "function_call");
        assert_eq!(events.last().unwrap()["item"]["namespace"], "web");
        assert!(output.has_calls());
        assert!(!output.has_pending_native_searches());
    }
}
