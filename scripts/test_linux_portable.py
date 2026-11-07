"""Validate Linux ZIP contents, foreground startup, admin auth and graceful SIGTERM."""
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
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


def extract_verified(archive, destination):
    with zipfile.ZipFile(archive) as bundle:
        names = bundle.namelist()
        require(len(names) == len(set(names)), "ZIP 存在重复文件。")
        roots = set()
        root_files = {"codex-relay", "start-linux.sh", "codex-relay.service", "relay.config.json",
                      "使用说明.md", "manifest.json"}
        for info in bundle.infolist():
            path = PurePosixPath(info.filename)
            require(not path.is_absolute() and ".." not in path.parts and len(path.parts) >= 2
                    and path.as_posix() == info.filename and "\\" not in info.filename,
                    "ZIP 含不安全的路径。")
            require(stat.S_ISREG(info.external_attr >> 16), "ZIP 仅允许普通文件。")
            require((len(path.parts) == 2 and path.name in root_files)
                    or (len(path.parts) > 3 and path.parts[1:3] == ("web", "dist"))
                    or (len(path.parts) == 3 and path.parts[1] == "third-party"
                        and path.name in {"dependencies.json", "LICENSES.txt"}), "ZIP 含非发行文件。")
            require(not ({"data", ".cache", "node_modules", ".git", "__MACOSX"} & set(path.parts))
                    and not path.name.startswith(".env")
                    and path.name not in {"auth.json", "config.toml", ".DS_Store"}
                    and path.suffix.lower() not in {".db", ".sqlite", ".sqlite3", ".log", ".pem", ".key"},
                    "ZIP 包含运行数据或私密配置。")
            roots.add(path.parts[0])
        require(len(roots) == 1, "ZIP 必须只有一个顶层文件夹。")
        root = roots.pop()
        manifest = json.loads(bundle.read(f"{root}/manifest.json"))
        architecture = manifest.get("architecture")
        from package_linux import TARGETS, check_binary
        require(architecture in TARGETS and manifest.get("target") == TARGETS[architecture][0]
                and manifest.get("staticLinked") is True, "Linux 静态发行清单无效。")
        expected = {f"{root}/manifest.json"}
        for entry in manifest["files"]:
            name = f"{root}/{entry['path']}"
            require(name not in expected, "清单存在重复文件。")
            expected.add(name)
            data = bundle.read(name)
            require(len(data) == entry["size"] and hashlib.sha256(data).hexdigest() == entry["sha256"],
                    f"文件校验失败：{entry['path']}")
            mode = stat.S_IMODE(bundle.getinfo(name).external_attr >> 16)
            required_mode = 0o755 if entry["path"] in {"codex-relay", "start-linux.sh"} else 0o644
            require(mode == int(entry["mode"], 8) == required_mode, "发行文件权限错误。")
        required = root_files | {"web/dist/index.html", "third-party/dependencies.json", "third-party/LICENSES.txt"}
        require({f"{root}/{name}" for name in required} <= expected and set(names) == expected,
                "ZIP 文件与清单不一致或缺少必要文件。")
        notices = bundle.read(f"{root}/third-party/LICENSES.txt")
        packages = json.loads(bundle.read(f"{root}/third-party/dependencies.json"))
        require(bool(packages), "缺少第三方依赖清单。")
        for package in packages:
            for original in package["licenseFiles"]:
                data = notices[original["offset"]:original["offset"] + original["size"]]
                require(len(data) == original["size"]
                        and hashlib.sha256(data).hexdigest() == original["sha256"], "许可原文校验失败。")
        require(bundle.testzip() is None, "ZIP CRC 校验失败。")
        # Paths and types are validated before creating anything outside the fresh destination.
        for info in bundle.infolist():
            target = destination / info.filename
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(bundle.read(info))
            target.chmod(stat.S_IMODE(info.external_attr >> 16))
    package = destination / root
    check_binary(package / "codex-relay", architecture)
    return package, manifest


def verify_archive(archive):
    require(sys.platform == "linux", "启动验证需要 Linux。")
    client = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    # Use disk-backed temp when supplied by the caller; no personal credentials are read.
    with tempfile.TemporaryDirectory(prefix="Codex Linux 验证 空格 ") as temporary:
        directory = Path(temporary)
        package, manifest = extract_verified(archive, directory)
        with socket.socket() as port_socket:
            port_socket.bind(("127.0.0.1", 0))
            port = port_socket.getsockname()[1]
        (package / "relay.config.json").write_text(json.dumps({"port": port}))
        home = directory / "home"
        home.mkdir()
        environment = dict(os.environ, HOME=str(home), CODEX_HOME=str(home / ".codex"),
                           PATH="/usr/bin:/bin")
        log = directory / "server.log"
        process = None

        def request(path, token=None):
            headers = {"Authorization": f"Bearer {token}"} if token else {}
            try:
                response = client.open(urllib.request.Request(f"http://127.0.0.1:{port}{path}",
                                                             headers=headers), timeout=3)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                return response.status, response.read()

        try:
            with log.open("wb") as output:
                process = subprocess.Popen([str(package / "start-linux.sh"), "--require-admin-token"],
                                           cwd=directory, env=environment, stdin=subprocess.DEVNULL,
                                           stdout=output, stderr=output)
            deadline = time.monotonic() + 30
            token = None
            while time.monotonic() < deadline:
                require(process.poll() is None, "Linux 启动器提前退出。")
                match = re.search(r"#token=([^\s]+)", log.read_text())
                if match:
                    token = match[1]
                    try:
                        if request("/healthz")[0] == 200:
                            break
                    except (OSError, urllib.error.URLError):
                        pass
                time.sleep(.1)
            else:
                raise RuntimeError("Linux 服务启动超时。")
            require(request("/api/overview")[0] == 403, "管理令牌未生效。")
            status, body = request("/api/overview", token)
            require(status == 200, "管理概览不可用。")
            overview = json.loads(body)
            require(overview["accounts"] == [] and overview["installation"] is None
                    and not overview["inferenceEnabled"], "发行包带入了账户或安装记录。")
            require(overview["operations"]["active"] == 0 and overview["operations"]["queued"] == 0,
                    "首次启动存在运行请求。")
            require(json.loads(request("/api/api-keys", token)[1])["keys"] == [], "发行包带入了 Key。")
            require(request("/v1/models")[0] == 401, "客户端接口缺少鉴权。")
            require(request("/")[0] == 200 and b'id="root"' in request("/")[1], "管理界面不可用。")
            asset = next((package / "web/dist/assets").glob("*.js"))
            require(request("/assets/" + asset.name)[0] == 200, "管理界面资源不可用。")
            # A second instance must fail without changing the first instance's data.
            duplicate = subprocess.run([str(package / "start-linux.sh"), "--require-admin-token"],
                                       cwd=directory, env=environment, capture_output=True, timeout=15)
            require(duplicate.returncode != 0 and request("/healthz")[0] == 200,
                    "重复启动未被拒绝或影响了原服务。")
            process.send_signal(signal.SIGTERM)
            require(process.wait(timeout=30) == 0, "SIGTERM 未正常退出。")
            with socket.socket() as connection:
                require(connection.connect_ex(("127.0.0.1", port)) != 0, "服务退出后端口仍在监听。")
            # Restart with the same data directory to check that the storage lock was released.
            with log.open("wb") as output:
                process = subprocess.Popen([str(package / "start-linux.sh"), "--require-admin-token"],
                                           cwd=directory, env=environment, stdin=subprocess.DEVNULL,
                                           stdout=output, stderr=output)
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                require(process.poll() is None, "重启失败，数据目录锁未释放。")
                try:
                    if request("/healthz")[0] == 200:
                        break
                except (OSError, urllib.error.URLError):
                    pass
                time.sleep(.1)
            else:
                raise RuntimeError("Linux 服务重启超时。")
            process.send_signal(signal.SIGTERM)
            require(process.wait(timeout=30) == 0, "重启后未正常退出。")
        finally:
            if process is not None and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
    return {"target": manifest["target"], "staticLinked": True, "portableLauncher": True,
            "managementAuth": True, "gracefulShutdown": True, "restart": True}


if __name__ == "__main__":
    print(json.dumps(verify_archive(Path(sys.argv[1]).resolve()), ensure_ascii=False, indent=2))
