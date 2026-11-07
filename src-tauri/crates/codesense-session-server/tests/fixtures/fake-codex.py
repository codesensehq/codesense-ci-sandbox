#!/usr/bin/python3 -S
# A fake qualified Codex, in the two parts a CodeSense launch runs it as.
# - Its engine (`app-server --listen unix://<socket>`): it runs the `-c hooks.<event>`
#   commands its argv was given, as Codex's engine runs them (`sh -c`, the hook's JSON on
#   stdin), serves JSON-RPC over a WebSocket on its socket (`initialize`,
#   `thread/loaded/list`, `thread/read`, `turn/start`, `thread/items/list`) for its one
#   thread, whose id its hooks name as their `session_id`, and runs its launch's first
#   turn itself, as its TUI's starting prompt would, once its TUI runs.
# - Its TUI (attached with `--remote`): it records every byte typed into the terminal in
#   `typed`, so a test can tell nothing ever was.
# Each turn started through the engine makes two tool calls, each waiting for its marker
# beside the fixture (`finish-tool`, `finish-tool-2`).
# With a `launcher` marker beside it, its engine is run as Codex on Linux from npm runs
# it: it names its socket by a link, and its hooks run in a child of its own (npm's
# `codex` script runs its binary as its child). Without one, as on macOS: its socket is at
# `listen`, and it runs its hooks itself.
# Its version is answered before anything else is imported: its host's probe of it is
# bounded, and a loaded machine starts Python slowly.
import sys
if sys.argv[1:] == ["--version"]:
    print("codex-cli 0.162.1")
    sys.exit(0)
import base64, hashlib, json, os, re, socket, struct, subprocess, threading, time, tty
d = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]


def mark(name, text="1"):
    with open(d + "/" + name, "w") as f:
        f.write(text)


def note(name, text):
    with open(d + "/" + name, "a") as f:
        f.write(text)


if "--remote" not in args and "--listen" not in args:
    mark("unobserved")
    time.sleep(60)
    sys.exit(0)

if "--remote" in args:
    # The TUI refuses extra roots beside `--remote`, as Codex 0.162.1's does.
    if any(a == "--add-dir" or a.startswith("--add-dir=") or "writable_roots" in a for a in args):
        sys.stderr.write("Error: --add-dir is not supported with --remote. Configure additional workspace roots on the server.\n")
        sys.exit(1)
    # The TUI: bracketed paste on, as Codex's is, and every byte typed recorded.
    mark("tui", " ".join(args))
    tty.setraw(0)
    sys.stdout.write("\x1b[?2004h")
    sys.stdout.flush()
    while True:
        data = os.read(0, 4096)
        if not data:
            break
        note("typed", data.decode(errors="replace"))
    sys.exit(0)

hooks = {}
for i, a in enumerate(args[:-1]):
    if a == "-c" and args[i + 1].startswith("hooks."):
        event, value = args[i + 1][len("hooks."):].split("=", 1)
        found = re.findall(r"""command\s*=\s*("(?:[^"\\]|\\.)*"|'[^']*')""", value)
        hooks[event] = [c[1:-1] if c.startswith("'") else json.loads(c) for c in found]
listen = args[args.index("--listen") + 1][len("unix://"):]
mark("engine", "%d %s" % (os.getpid(), listen))
log = open(d + "/hooks.log", "a")
lock = threading.Lock()
THREAD = os.environ.get("CODESENSE_TEST_THREAD_ID", "codex-e2e")
# Shared by the turns and the connections that ask about them.
state = {"turn": 0, "active": False, "messages": []}


launcher = os.path.exists(d + "/launcher")
# Under a launcher, a hook's `$PPID` is the child that runs it, not this engine.
RUN_IN_CHILD = "import subprocess, sys; sys.exit(subprocess.run(['sh', '-c', sys.argv[1]]).returncode)"


def run(event, **extra):
    payload = dict(hook_event_name=event, session_id=THREAD, turn_id="turn-%d" % state["turn"], **extra)
    for command in hooks.get(event, []):
        argv = [sys.executable, "-S", "-c", RUN_IN_CHILD, command] if launcher else ["sh", "-c", command]
        p = subprocess.run(argv, input=json.dumps(payload), capture_output=True, text=True)
        with lock:
            log.write(json.dumps({"event": event, "agent": extra.get("agent_id"), "check": "--inbox-wait" in command, "code": p.returncode, "stdout": p.stdout, "stderr": p.stderr}) + "\n")
            log.flush()


def wait_for(marker):
    deadline = time.time() + 30
    while not os.path.exists(d + "/" + marker):
        if time.time() > deadline:
            sys.stderr.write("fake codex: no %s marker in 30s\n" % marker)
            os._exit(1)
        time.sleep(0.05)


def turn(text):
    """A turn started through the engine: its prompt, two tool calls, its end."""
    with lock:
        state["turn"] += 1
    run("UserPromptSubmit", prompt=text)
    mark("prompt", text)
    for call, marker in (("t1", "finish-tool"), ("t2", "finish-tool-2")):
        wait_for(marker)
        if call == "t1":
            # A subagent's tool call runs its hooks in this same process first.
            run("PostToolUse", tool_name="Bash", tool_use_id="c1", agent_id="child")
        run("PreToolUse", tool_name="Bash", tool_use_id=call)
        run("PostToolUse", tool_name="Bash", tool_use_id=call)
    run("Stop")
    with lock:
        state["active"] = False
    mark("stopped")


def recv(conn, n):
    data = b""
    while len(data) < n:
        chunk = conn.recv(n - len(data))
        if not chunk:
            raise EOFError
        data += chunk
    return data


def read_message(conn):
    first, second = recv(conn, 2)
    n = second & 127
    if n == 126:
        n = struct.unpack("!H", recv(conn, 2))[0]
    elif n == 127:
        n = struct.unpack("!Q", recv(conn, 8))[0]
    mask = recv(conn, 4) if second & 128 else None
    data = recv(conn, n)
    if mask:
        data = bytes(b ^ mask[i % 4] for i, b in enumerate(data))
    if first & 15 == 8:
        raise EOFError
    return data


def send_message(conn, value):
    data = json.dumps(value).encode()
    n = len(data)
    header = bytes([0x81, n]) if n < 126 else bytes([0x81, 126]) + struct.pack("!H", n) if n < 65536 else bytes([0x81, 127]) + struct.pack("!Q", n)
    conn.sendall(header + data)


def serve(conn):
    head = b""
    while b"\r\n\r\n" not in head:
        chunk = conn.recv(4096)
        if not chunk:
            conn.close()
            return
        head += chunk
    key = re.search(rb"Sec-WebSocket-Key: (\S+)", head, re.I).group(1)
    accept = base64.b64encode(hashlib.sha1(key + b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest())
    conn.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + accept + b"\r\n\r\n")
    try:
        while True:
            request = json.loads(read_message(conn))
            method, params = request.get("method"), request.get("params") or {}
            if "id" not in request:
                continue
            with lock:
                if method == "thread/loaded/list":
                    result = {"data": [THREAD]}
                elif method == "thread/read":
                    result = {"thread": {"id": THREAD, "recencyAt": 1, "status": {"type": "active" if state["active"] else "idle"}}}
                elif method == "turn/start":
                    state["active"] = True
                    state["messages"].append(params["clientUserMessageId"])
                    text = params["input"][0]["text"]
                    threading.Thread(target=turn, args=(text,)).start()
                    result = {"turn": {"id": "turn-%d" % (state["turn"] + 1)}}
                elif method == "thread/items/list":
                    result = {"data": [{"item": {"type": "userMessage", "clientId": c}} for c in state["messages"]]}
                else:
                    result = {}
            send_message(conn, {"id": request["id"], "result": result})
    except (EOFError, OSError):
        conn.close()


# As Codex on Linux does, under a launcher it serves on a socket of its own and names it
# at `listen` by a link.
served = os.path.join(d, "served-%d.sock" % os.getpid()) if launcher else listen
server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
os.chdir(os.path.dirname(served))
server.bind(os.path.basename(served))
server.listen(8)
if launcher:
    os.symlink(served, listen)
# Its TUI's starting prompt: the launch's first turn, once its TUI runs (its launch bound
# the engine before it started the TUI).
def first_turn():
    wait_for("tui")
    with lock:
        state["turn"], state["active"] = 1, True
    run("SessionStart")
    run("UserPromptSubmit", prompt="hi")
    run("Stop")
    with lock:
        state["active"] = False
    mark("ready")
threading.Thread(target=first_turn).start()
while True:
    conn, _ = server.accept()
    threading.Thread(target=serve, args=(conn,)).start()
