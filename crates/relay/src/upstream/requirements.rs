//! Read capability declarations, never arbitrary JSON schemas or caller data.
use crate::operations::Feature;
use serde_json::Value;

pub(super) fn features(value: &Value) -> Vec<Feature> {
    fn add(features: &mut Vec<Feature>, feature: Feature) {
        if !features.contains(&feature) {
            features.push(feature);
        }
    }
    fn tools(value: &Value, features: &mut Vec<Feature>) {
        for tool in value.as_array().into_iter().flatten() {
            match tool["type"].as_str() {
                Some("web_search" | "web_search_preview") => add(features, Feature::WebSearch),
                Some("image_generation") => add(features, Feature::ImageGeneration),
                Some("namespace") => tools(&tool["tools"], features),
                _ => {}
            }
        }
    }
    fn content(value: &Value, features: &mut Vec<Feature>) {
        for part in value.as_array().into_iter().flatten() {
            if part["type"] == "input_image" {
                add(features, Feature::ImageInput);
            }
        }
    }
    let mut features = vec![];
    tools(&value["tools"], &mut features);
    for item in value["input"].as_array().into_iter().flatten() {
        match item["type"].as_str() {
            Some("additional_tools" | "tool_search_output") => tools(&item["tools"], &mut features),
            Some("message") => content(&item["content"], &mut features),
            None if item["role"].is_string() => content(&item["content"], &mut features),
            Some("function_call_output" | "custom_tool_call_output") => {
                content(&item["output"], &mut features)
            }
            Some("input_image") => add(&mut features, Feature::ImageInput),
            _ => {}
        }
    }
    features
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn schema_metadata_and_tool_arguments_are_not_declarations() {
        let trap = json!({"type":"web_search","nested":{"type":"image_generation"},"content":[{"type":"input_image"}]});
        let value = json!({"metadata":trap,"text":{"format":{"schema":trap}},"tools":[{"type":"function","name":"f","parameters":trap}],"input":[{"type":"function_call","arguments":trap},{"type":"function_call_output","output":[{"type":"input_text","text":trap}]}]});
        assert!(features(&value).is_empty());
    }
    #[test]
    fn protocol_tools_and_media_remain_capability_checked() {
        for value in [
            json!({"tools":[{"type":"web_search_preview"},{"type":"image_generation"}],"input":[{"role":"user","content":[{"type":"input_image"}]}]}),
            json!({"input":[{"type":"additional_tools","tools":[{"type":"namespace","tools":[{"type":"web_search"}]}]},{"type":"tool_search_output","tools":[{"type":"image_generation"}]},{"type":"function_call_output","output":[{"type":"input_image"}]}]}),
        ] {
            let found = features(&value);
            for feature in [
                Feature::WebSearch,
                Feature::ImageGeneration,
                Feature::ImageInput,
            ] {
                assert!(found.contains(&feature));
            }
        }
    }
}
