"""Verify and smoke-test the actual macOS ZIP without desktop login or inference."""
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import zipfile


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def verify_archive(archive):
    archive = Path(archive).resolve()
    with zipfile.ZipFile(archive) as bundle:
        names = bundle.namelist()
        require(len(names) == len(set(names)), "ZIP 存在重复文件。")
        roots = set()
        allowed_directories = {"web", "third-party"}
        allowed_root_files = {"codex-relay", "启动中转站.command", "relay.config.json", "使用说明.md", "manifest.json"}
        for info in bundle.infolist():
            path = PurePosixPath(info.filename)
            require(not path.is_absolute() and ".." not in path.parts and len(path.parts) >= 2,
                    "ZIP 含不安全的路径。")
            require("\\" not in info.filename and ":" not in info.filename, "ZIP 路径格式错误。")
            require(not stat.S_ISLNK(info.external_attr >> 16), "ZIP 不得包含符号链接。")
            require(not ({"data", ".cache", "target", "node_modules", ".git", "__MACOSX"} & set(path.parts)),
                    "ZIP 包含运行数据或开发缓存。")
            require(not path.name.startswith(".env") and path.name not in {
                "auth.json", "config.toml", "bridge-auth.json", "bridge-token.json", ".DS_Store"},
                "ZIP 含不应分享的配置文件。")
            require(path.suffix.lower() not in {".sqlite", ".sqlite3", ".db", ".log", ".pem", ".key", ".p12", ".pfx"},
                    "ZIP 包含数据库、日志或私有证书。")
            require((len(path.parts) == 2 and path.name in allowed_root_files)
                    or (len(path.parts) > 2 and path.parts[1] in allowed_directories), "ZIP 含非发行文件。")
            roots.add(path.parts[0])
        require(len(roots) == 1, "ZIP 必须只有一个顶层文件夹。")
        root_name = roots.pop()
        manifest = json.loads(bundle.read(f"{root_name}/manifest.json"))
        expected_names = {f"{root_name}/manifest.json"}
        for entry in manifest["files"]:
            name = f"{root_name}/{entry['path']}"
            require(name not in expected_names, "清单存在重复文件。")
            expected_names.add(name)
            data = bundle.read(name)
            require(len(data) == entry["size"] and hashlib.sha256(data).hexdigest() == entry["sha256"],
                    f"ZIP 文件校验失败：{entry['path']}")
            mode = stat.S_IMODE(bundle.getinfo(name).external_attr >> 16)
            require(mode == int(entry["mode"], 8), f"ZIP 文件权限不匹配：{entry['path']}")
        require(set(names) == expected_names, "ZIP 文件与清单不一致。")
        notice_names = {name for name in names if name.startswith(f"{root_name}/third-party/")}
        require(notice_names == {f"{root_name}/third-party/{name}"
                                 for name in ("dependencies.json", "LICENSES.txt")},
                "第三方许可应合并为两个文件。")
        packages = json.loads(bundle.read(f"{root_name}/third-party/dependencies.json"))
        require(bool(packages), "缺少第三方依赖清单。")
        notices = bundle.read(f"{root_name}/third-party/LICENSES.txt")
        for package in packages:
            for original in package["licenseFiles"]:
                data = notices[original["offset"]:original["offset"] + original["size"]]
                require(len(data) == original["size"]
                        and hashlib.sha256(data).hexdigest() == original["sha256"],
                        "第三方许可原文校验失败。")
        for relative in ("codex-relay", "启动中转站.command"):
            require(bundle.getinfo(f"{root_name}/{relative}").external_attr >> 16 & stat.S_IXUSR,
                    f"ZIP 缺少执行权限：{relative}")
        require(bundle.testzip() is None, "ZIP CRC 校验失败。")

    client = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with socket.socket() as port_socket:
        port_socket.bind(("127.0.0.1", 0))
        port = port_socket.getsockname()[1]
    base_url = f"http://127.0.0.1:{port}"

    def request(path, data=None):
        try:
            response = client.open(urllib.request.Request(base_url + path, data=data), timeout=5)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            return response.status, response.read()

    def port_open():
        with socket.socket() as connection:
            connection.settimeout(.2)
            return connection.connect_ex(("127.0.0.1", port)) == 0

    with tempfile.TemporaryDirectory(prefix="Codex 便携包验证 空格 ") as temporary:
        folder = Path(temporary).resolve()
        subprocess.run(["/usr/bin/ditto", "-x", "-k", str(archive), str(folder)], check=True)
        package = folder / root_name
        environment = dict(os.environ, PATH="/usr/bin:/bin:/usr/sbin:/sbin")
        (package / "relay.config.json").write_text(json.dumps({"port": port}), encoding="utf-8")
        command = [str(package / "启动中转站.command"), "--no-browser"]
        pid_file = package / f".cache/launcher-{port}.pid"
        owned_pid = None
        require(not (package / "data").exists() and not (package / ".cache").exists(), "解压包已有运行数据。")

        def launch():
            result = subprocess.run(command, cwd=folder, env=environment, stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, timeout=45)
            require(result.returncode == 0, "便携启动器失败：" + result.stdout + result.stderr)

        try:
            launch()
            owned_pid = int(pid_file.read_text(encoding="ascii").strip())
            require(request("/healthz")[0] == 200, "便携服务健康检查失败。")
            overview = json.loads(request("/api/overview")[1])
            require(overview["accounts"] == [] and overview["installation"] is None, "便携包带入了账户或安装记录。")
            require(overview["operations"]["active"] == 0 and overview["operations"]["queued"] == 0,
                    "便携包首次启动存在运行请求。")
            require(not overview["inferenceEnabled"], "便携包不应预置推理路由。")
            require(json.loads(request("/api/api-keys")[1])["keys"] == [], "便携包带入了 API Key。")
            require(request("/v1/models")[0] == 401, "客户端接口缺少鉴权。")
            require(request("/")[0] == 200 and b'id="root"' in request("/")[1], "管理界面不可用。")
            asset = next((package / "web/dist/assets").glob("*.js"))
            require(request("/assets/" + asset.name)[0] == 200, "管理界面资源不可用。")
            launch()
            require(int(pid_file.read_text(encoding="ascii").strip()) == owned_pid, "重复启动产生了新服务。")
            require(request("/api/service/stop", b"{}")[0] == 202, "便携服务不能正常停止。")
            for _ in range(100):
                if not port_open():
                    break
                time.sleep(.1)
            require(not port_open(), "便携服务退出后仍监听端口。")
        finally:
            if owned_pid is None and pid_file.exists():
                owned_pid = int(pid_file.read_text(encoding="ascii").strip())
            if owned_pid and port_open():
                # Only signal the exact executable started in the temporary package.
                process = subprocess.run(["/bin/ps", "-p", str(owned_pid), "-o", "comm="],
                                         capture_output=True, text=True)
                if process.stdout.strip() == str(package / "codex-relay"):
                    try:
                        os.kill(owned_pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                    for _ in range(100):
                        if not port_open():
                            break
                        time.sleep(.1)
    return {"passed": True, "verifiedFiles": len(manifest["files"]), "target": manifest["target"],
            "archiveChecksums": True, "executablePermissions": True, "noRuntimeData": True,
            "emptyAccountsAndKeys": True, "minimalPath": True, "unicodeAndSpaces": True,
            "repeatLaunchReusedPid": True, "managementPage": True, "clientAuthRequired": True,
            "portableLauncher": True, "gracefulShutdown": True, "modelRequests": 0}


if __name__ == "__main__":
    print(json.dumps(verify_archive(sys.argv[1]), ensure_ascii=False, indent=2))
