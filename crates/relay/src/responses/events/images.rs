//! Adapt app-server's image_gen.imagegen lifecycle to a hosted Responses tool.
use super::{Output, protocol};
use crate::error::{Error, Result};
use serde_json::{Value, json};

pub(super) struct NativeImage {
    index: usize,
    completed: bool,
    output_received: bool,
}

impl NativeImage {
    pub(super) fn pending(&self) -> bool {
        !self.completed || !self.output_received
    }
}

impl Output {
    pub(super) fn native_image_started(&mut self, raw: &Value, out: &mut Vec<Value>) -> Result<()> {
        let arguments = raw["arguments"].as_str().ok_or_else(protocol)?;
        if !serde_json::from_str::<Value>(arguments).is_ok_and(|value| value.is_object()) {
            return Err(protocol());
        }
        let call = self.call_id(raw)?.to_owned();
        let index = self.items.len();
        let item = json!({"type":"image_generation_call","id":format!("ig_{}",uuid::Uuid::new_v4().simple()),"status":"in_progress","result":null});
        self.items.push(item.clone());
        self.native_images.insert(
            call,
            NativeImage {
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

    pub(super) fn native_image_completed(
        &mut self,
        native: &Value,
        out: &mut Vec<Value>,
    ) -> Result<()> {
        let call = native["id"].as_str().ok_or_else(protocol)?;
        let Some(image) = self.native_images.get_mut(call) else {
            // Hosted image_generation_call has its own raw completion event.
            return Ok(());
        };
        if image.completed {
            return Err(protocol());
        }
        if native["status"] != "completed" {
            return Err(Error::new(
                "UPSTREAM_TOOL_FAILED",
                "原生生图工具未完成，未重发请求。",
            ));
        }
        let result = native["result"].as_str().ok_or_else(protocol)?;
        crate::media::generated_image(result)?;
        let item = &mut self.items[image.index];
        item["result"] = json!(result);
        self.bytes += result.len();
        if !native["revisedPrompt"].is_null() {
            let prompt = native["revisedPrompt"].as_str().ok_or_else(protocol)?;
            item["revised_prompt"] = json!(prompt);
            self.bytes += prompt.len();
        }
        // savedPath is neither read nor exposed; the inline result is authoritative.
        image.completed = true;
        self.finish_native_image(call, out)
    }

    pub(super) fn native_image_output(&mut self, raw: &Value, out: &mut Vec<Value>) -> Result<()> {
        let call = raw["call_id"].as_str().ok_or_else(protocol)?;
        let image = self.native_images.get_mut(call).ok_or_else(protocol)?;
        if image.output_received || !(raw["output"].is_string() || raw["output"].is_array()) {
            return Err(protocol());
        }
        image.output_received = true;
        self.bytes += serde_json::to_vec(&raw["output"])?.len();
        self.finish_native_image(call, out)
    }

    fn finish_native_image(&mut self, call: &str, out: &mut Vec<Value>) -> Result<()> {
        let image = &self.native_images[call];
        if !image.pending() {
            let index = image.index;
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

    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFUlEQVR4nGNgYPhPIhrVMKph2GoAAJLb/wFh5Z4RAAAAAElFTkSuQmCC";
    fn request() -> Request {
        Request::parse(json!({"model":"m","input":"draw","tools":[{"type":"image_generation"}]}))
            .unwrap()
    }
    fn call() -> Value {
        json!({"method":"rawResponseItem/completed","params":{"item":{"type":"function_call","id":"fc_image","call_id":"image","namespace":"image_gen","name":"imagegen","arguments":"{\"prompt\":\"draw\"}"}}})
    }
    fn completed() -> Value {
        json!({"method":"item/completed","params":{"item":{"type":"imageGeneration","id":"image","status":"completed","result":PNG,"revisedPrompt":"draw","savedPath":"/outside/workspace/generated.png"}}})
    }
    fn result() -> Value {
        json!({"method":"rawResponseItem/completed","params":{"item":{"type":"function_call_output","id":"fco_image","call_id":"image","output":[{"type":"input_image","image_url":format!("data:image/png;base64,{PNG}")},{"type":"input_text","text":"Saved to /outside/workspace/generated.png"}]}}})
    }

    #[test]
    fn native_image_waits_for_both_events_and_emits_only_validated_inline_data() {
        for reversed in [false, true] {
            let r = request();
            let mut output = Output::new(&r);
            let added = output.consume(&call(), &r).unwrap();
            assert_eq!(added[0]["item"]["type"], "image_generation_call");
            assert!(output.has_pending_native_tools());
            assert!(!output.has_calls());
            let events = if reversed {
                [result(), completed()]
            } else {
                [completed(), result()]
            };
            assert!(output.consume(&events[0], &r).unwrap().is_empty());
            assert_eq!(output.completed().unwrap_err().code, "UPSTREAM_TOOL_FAILED");
            assert_eq!(output.consume(&events[1], &r).unwrap().len(), 1);
            let done = output.completed().unwrap();
            assert_eq!(done["type"], "response.completed");
            let item = &done["response"]["output"][0];
            assert_eq!(item["result"], PNG);
            assert_eq!(item["revised_prompt"], "draw");
            assert!(item["id"].as_str().unwrap().starts_with("ig_"));
            assert!(!done.to_string().contains("/outside/"));
            assert!(!output.has_pending_native_tools());
            Request::parse(
                json!({"model":"m","input":[item,{"role":"user","content":"continue"}]}),
            )
            .unwrap();
        }
    }

    #[test]
    fn image_failures_missing_output_and_invalid_images_are_not_successful() {
        let r = request();
        for (field, value) in [
            ("status", json!("failed")),
            ("result", json!("not an image")),
        ] {
            let mut output = Output::new(&r);
            output.consume(&call(), &r).unwrap();
            let mut event = completed();
            event["params"]["item"][field] = value;
            assert!(output.consume(&event, &r).is_err());
            assert!(output.completed().is_err());
        }
        let mut output = Output::new(&r);
        output.consume(&call(), &r).unwrap();
        output.consume(&completed(), &r).unwrap();
        assert_eq!(output.completed().unwrap_err().code, "UPSTREAM_TOOL_FAILED");
        output.interrupted();
        assert_eq!(output.completed().unwrap()["type"], "response.incomplete");
        assert!(output.items().is_empty());
    }

    #[test]
    fn image_scope_and_duplicate_events_stay_strict() {
        for body in [
            json!({"model":"m","input":"draw"}),
            json!({"model":"m","input":"draw","tools":[{"type":"image_generation"}],"tool_choice":"none"}),
        ] {
            let r = Request::parse(body).unwrap();
            assert_eq!(
                Output::new(&r).consume(&call(), &r).unwrap_err().code,
                "NATIVE_TOOL_SCOPE"
            );
        }
        let r = request();
        for duplicate in [call(), completed(), result()] {
            let mut output = Output::new(&r);
            for event in [call(), completed(), result()] {
                output.consume(&event, &r).unwrap();
            }
            assert!(output.consume(&duplicate, &r).is_err());
        }
        let r = Request::parse(json!({"model":"m","input":"x","tools":[{"type":"image_generation"},
            {"type":"namespace","name":"image_gen","description":"client","tools":[{"type":"function","name":"imagegen"}]}]})).unwrap();
        let mut event = call();
        event["params"]["item"]["namespace"] = r.dynamic_tools()[0]["name"].clone();
        let mut output = Output::new(&r);
        output.consume(&event, &r).unwrap();
        assert!(output.has_calls());
        assert!(!output.has_pending_native_tools());
    }
}
