"""Synthetic response variants for installed-client cache roundtrip validation."""
import copy
import json
import uuid

CASES = (
    "text", "message_null_cipher", "message_empty_metadata", "message_parts",
    "reasoning_missing", "reasoning_null", "reasoning_empty", "reasoning_summary",
    "reasoning_plain", "reasoning_text", "message_cipher", "function", "parallel",
    "namespace", "image_result", "error_result", "custom", "settings_null", "settings_auto",
    "turn_effort", "turn_tier", "turn_model", "settings_reset", "tool_turn_settings", "tool_settings_changed",
    "identity_header_rotation", "identity_thread_change", "identity_conflict", "identity_anonymous_child",
)
FALLBACK_CASES = {"message_cipher", "identity_thread_change"}
TOOL_CASES = {"function", "parallel", "namespace", "image_result", "error_result", "custom", "tool_turn_settings", "tool_settings_changed"}


def response_items(case, call_index, message, model_body):
    """Only the first response varies; later turns prove recovery and continuity."""
    item = copy.deepcopy(message)
    if call_index != 1:
        return [item]
    if case == "message_null_cipher":
        item["encrypted_content"] = None
    elif case == "message_empty_metadata":
        item["internal_chat_message_metadata_passthrough"] = None
    elif case == "message_cipher":
        item["encrypted_content"] = "synthetic-message-cipher"
    elif case == "message_parts":
        text = item["content"][0]["text"]
        item["content"] = [{"type": "output_text", "text": text[:5]},
                           {"type": "output_text", "text": text[5:]}]
    elif case.startswith("reasoning_"):
        reasoning = {"type": "reasoning", "id": "rs_" + uuid.uuid4().hex,
                     "summary": [], "encrypted_content": "synthetic-reasoning-state"}
        if case == "reasoning_null":
            reasoning["content"] = None
        elif case == "reasoning_empty":
            reasoning["content"] = []
        elif case == "reasoning_summary":
            reasoning["summary"] = [{"type": "summary_text", "text": "Synthetic summary"}]
        elif case in ("reasoning_plain", "reasoning_text"):
            reasoning["content"] = [{"type": "reasoning_text" if case == "reasoning_plain" else "text",
                                     "text": "Synthetic reasoning content"}]
        return [reasoning, item]
    elif case in TOOL_CASES:
        # Discover the registered name on the wire; namespaces may be escaped by the bridge.
        catalog = list(model_body.get("tools") or [])
        for entry in model_body.get("input", []):
            if entry.get("type") == "additional_tools":
                catalog.extend(entry["tools"])
        found = []
        for tool in catalog:
            if tool.get("type") == "namespace":
                found.extend((tool["name"], member) for member in tool["tools"])
            else:
                found.append((None, tool))
        tool_name = "exec" if case == "custom" else "unused_fixture_tool"
        namespace, tool = next((namespace, tool) for namespace, tool in found
                               if tool.get("name") == tool_name)
        arguments = {"case": case} if case != "custom" else {
            "input": 'text(await tools.unused_fixture_tool({case:"custom"}));'}
        call = {"type": "function_call", "id": "fc_" + uuid.uuid4().hex,
                "call_id": "audit-call", "name": tool["name"], "arguments": json.dumps(arguments),
                "status": "completed", "encrypted_content": None}
        if namespace:
            call["namespace"] = namespace
        return [call, {**call, "id": "fc_" + uuid.uuid4().hex, "call_id": "audit-parallel"}] if case == "parallel" else [call]
    return [item]
