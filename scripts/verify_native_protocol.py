"""Export and check this installation's native schema. No login or inference."""
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
installation = json.loads((ROOT / ".cache/launch-config.json").read_text())
executable = Path(installation["appServerExecutable"])
digest = hashlib.file_digest(executable.open("rb"), "sha256").hexdigest()
assert digest == installation["appServerSha256"], "Run prepare again after a native update."
checks = {
    "v1/InitializeParams.json": ["clientInfo", "capabilities"],
    "v2/GetAccountParams.json": ["refreshToken"],
    "v2/AccountLoginCompletedNotification.json": ["loginId", "success", "error"],
    "v2/CancelLoginAccountParams.json": ["loginId"],
    "v2/ThreadStartParams.json": ["dynamicTools", "experimentalRawEvents", "ephemeral", "config"],
    "v2/TurnStartParams.json": ["threadId", "input", "effort"],
    "v2/TurnInterruptParams.json": ["threadId", "turnId"],
    "DynamicToolCallParams.json": ["threadId", "turnId", "callId", "tool", "arguments"],
    "DynamicToolCallResponse.json": ["success", "contentItems"],
    "ChatgptAuthTokensRefreshParams.json": ["previousAccountId", "reason"],
    "ChatgptAuthTokensRefreshResponse.json": ["accessToken", "chatgptAccountId"],
    "AttestationGenerateResponse.json": ["token"],
    "CurrentTimeReadParams.json": ["threadId"],
    "CurrentTimeReadResponse.json": ["currentTimeAt"],
}
with tempfile.TemporaryDirectory(prefix="native-schema-", dir=ROOT / ".cache") as directory:
    subprocess.run([str(executable), "app-server", "generate-json-schema", "--experimental", "--out", directory], check=True, timeout=30, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    schemas = Path(directory)
    hashes = {}
    for name, fields in checks.items():
        raw = (schemas / name).read_bytes()
        schema = json.loads(raw)
        assert set(fields) <= schema["properties"].keys(), f"Native fields changed: {name}"
        hashes[name] = hashlib.sha256(raw).hexdigest()
    initialize = json.loads((schemas / "v1/InitializeParams.json").read_text())
    attestation = initialize["definitions"]["InitializeCapabilities"]["properties"]["requestAttestation"]
    assert attestation["type"] == "boolean" and attestation["default"] is False
    time_response = json.loads((schemas / "CurrentTimeReadResponse.json").read_text())
    assert time_response["properties"]["currentTimeAt"]["type"] == "integer"
    assert time_response["properties"]["currentTimeAt"]["format"] == "int64"
    params_raw = (schemas / "AttestationGenerateParams.json").read_bytes()
    params = json.loads(params_raw)
    assert params["type"] == "object" and not params.get("properties")
    hashes["AttestationGenerateParams.json"] = hashlib.sha256(params_raw).hexdigest()
    login = json.loads((schemas / "v2/LoginAccountParams.json").read_text())
    variants = {item["properties"]["type"]["enum"][0]: item for item in login["oneOf"]}
    assert {"chatgpt", "chatgptDeviceCode", "chatgptAuthTokens"} <= variants.keys()
    assert {"codexStreamlinedLogin", "useHostedLoginSuccessPage"} <= variants["chatgpt"]["properties"].keys()
    assert {"accessToken", "chatgptAccountId"} <= set(variants["chatgptAuthTokens"]["required"])
    hashes["v2/LoginAccountParams.json"] = hashlib.sha256((schemas / "v2/LoginAccountParams.json").read_bytes()).hexdigest()
    # Replace only individual generated schema files, never installation or account data.
    export = ROOT / ".cache/native-protocol"
    for source in schemas.rglob("*.json"):
        target = export / source.relative_to(schemas)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(source.read_bytes())
report = {
    "appServerVersion": installation["appServerVersion"],
    "appServerSha256": digest,
    "schemas": hashes,
    "legacyAuthStatus": "getAuthStatus is not exported in this schema; verified separately by native OAuth tests and desktop binding.",
}
(ROOT / ".cache/native-protocol-check.json").write_text(json.dumps(report, indent=2) + "\n")
print(f"PASS: {len(hashes)} installed native schemas; ChatGPT login, external tokens, refresh, tools, turns, attestation and host time.")
