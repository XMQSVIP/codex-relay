"""pip install openai; set BRIDGE_BASE_URL, BRIDGE_API_KEY and BRIDGE_MODEL."""
import json
import os
from datetime import datetime, timezone
from openai import OpenAI

client = OpenAI(
    base_url=os.environ.get("BRIDGE_BASE_URL", "http://127.0.0.1:4317/v1"),
    api_key=os.environ["BRIDGE_API_KEY"],
)
model = os.environ["BRIDGE_MODEL"]
tools = [{
    "type": "function", "name": "client_time",
    "description": "Read the current UTC time on the caller computer.",
    "parameters": {"type": "object", "properties": {}, "additionalProperties": False},
    "strict": False,
}]
history = [{"role": "user", "content": "调用 client_time，告诉我客户端的当前时间。"}]
for _ in range(8):
    with client.responses.stream(model=model, input=history, tools=tools, store=False) as stream:
        for event in stream:
            if event.type == "response.output_text.delta":
                print(event.delta, end="", flush=True)
        response = stream.get_final_response()
    history.extend(item.model_dump(exclude_none=True) for item in response.output)
    calls = [item for item in response.output if item.type == "function_call"]
    if not calls:
        print()
        break
    for call in calls:
        if call.name != "client_time" or json.loads(call.arguments) != {}:
            raise RuntimeError("Unexpected tool call")
        result = datetime.now(timezone.utc).isoformat()
        print("Executed on this client:", result)
        history.append({"type": "function_call_output", "call_id": call.call_id, "output": result})
else:
    raise RuntimeError("Tool round-trip limit reached")
