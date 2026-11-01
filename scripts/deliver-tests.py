#!/usr/bin/env python3
"""TC-473, TC-474 and TC-478 (docs/TEST.md): a lost ack makes the server
push the delivery again without a second insert, an attachment that expired
while its session was offline goes in marked expired (FLOW-DELIVER), and
the server logs the events of DESIGN §20 that the success metrics are
counted from, against the local server.

    . crates/scripts/local-env.sh
    python3 crates/scripts/deliver-tests.py <RUN-ID> [473 474 478]

The Claude Code session is played here: an `imbo mcp` that stays open
registers it, and a Unix socket of this script takes its inserts. Each case
starts its own server and seed the way parity-tests.py does, writes its
lines to docs/test-runs/<run>-tc-NNN.txt (with RUN_HEADER, if set, on the
first line) and exits 1 when any step failed. Standard library only; about
four minutes, most of it the ack timeout and the backoff after it.
"""

import importlib.util
import json
import os
import re
import socket
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("parity", os.path.join(HERE, "parity-tests.py"))
P = importlib.util.module_from_spec(spec)
spec.loader.exec_module(P)
L, OUT, call, check, imbo = P.L, P.OUT, P.call, P.check, P.imbo
SID = "55555555-aaaa-bbbb-cccc-000000"
INSERTED = "inserted into Claude Code session"


class Session:
    """A Claude Code session as the daemon sees one: `imbo mcp` started
    with the session's variables, and the messaging socket it inserts
    through. Each insert's text lands in `inserts`."""

    def __init__(self, who, sid, title):
        self.inserts = []
        path = f"{L}/{title}.sock"
        self.srv = socket.socket(socket.AF_UNIX)
        self.srv.bind(path)
        self.srv.listen()
        threading.Thread(target=self._accept, daemon=True).start()
        cc = f"{L}/cc-{title}"
        os.makedirs(f"{cc}/sessions", exist_ok=True)
        with open(f"{cc}/sessions/{os.getpid()}.json", "w") as f:
            json.dump({"pid": os.getpid(), "sessionId": f"{SID}{sid}", "name": title}, f)
        env = {**os.environ, "IMBO_HOME": f"{L}/{who}", "CLAUDE_CODE_SESSION_ID": f"{SID}{sid}",
               "CLAUDE_CODE_MESSAGING_SOCKET": f"uds:{path}", "CLAUDE_CODE_MESSAGING_TOKEN": title,
               "CLAUDE_CONFIG_DIR": cc}
        self.mcp = subprocess.Popen([P.IMBO, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL, env=env)

    def _accept(self):
        while True:
            try:
                c, _ = self.srv.accept()
            except OSError:
                return
            buf = b""
            while chunk := c.recv(65536):
                buf += chunk
            c.close()
            for line in buf.splitlines():
                frame = json.loads(line)
                if frame.get("type") == "user":
                    self.inserts.append(frame["message"]["content"])

    def close(self):
        self.mcp.kill()
        self.srv.close()


def wait(cond, secs, every=0.5):
    end = time.time() + secs
    while time.time() < end:
        if v := cond():
            return v
        time.sleep(every)
    return cond()


def text(path):
    return open(path, errors="replace").read()


def lines(path, needle, n):
    """Lines of the log at `path` with `needle` about message `n`."""
    return [l for l in text(path).splitlines() if needle in l and re.search(rf"\bmsg={n}\b", l)]


def connects(who):
    return len(re.findall(r"\bconnected sessions=", text(f"{L}/{who}/daemon.log")))


def stop_daemon(who):
    pid = int(open(f"{L}/{who}/daemon.pid").read())
    os.kill(pid, 15)
    wait(lambda: subprocess.run(["kill", "-0", str(pid)], capture_output=True).returncode != 0, 10)


def start_daemon(who, **env):
    before = connects(who)
    subprocess.run(["sh", P.ENV, "daemon", who], env={**os.environ, **env}, check=True)
    return wait(lambda: connects(who) > before, 15)


def agent(auth, title):
    _, b, _ = call("GET", "/v1/agents", **auth)
    return next((a for a in b["agents"] if a.get("title") == title), {})


def tc473():
    P.up("alice:bob")
    stop_daemon("alice")
    start_daemon("alice", IMBO_FAULT_INJECT="lose-ack")
    s = Session("alice", "473001", "t473")
    try:
        alice = {"bearer": P.app_token("alice@imbo.test")}
        G = wait(lambda: agent(alice, "t473").get("online") and agent(alice, "t473")["id"], 20)
        OUT.append(f"前置:alice 常驻程序带 IMBO_FAULT_INJECT=lose-ack;假会话 t473 <G>={G}")

        st, b, _ = call("POST", f"/v1/agents/{G}/messages", {"body": "T473", "client_msg_id": "t473"}, **alice)
        N = ((b or {}).get("message") or {}).get("id")
        check(473, 1, "发给自己的会话", st == 201 and N, f"{st} <N>={N}")

        def state():
            _, b, _ = call("GET", f"/v1/agents/{G}/messages", **alice)
            return next((m.get("state") for m in b["messages"] if m["id"] == N), None)

        dlog = f"{L}/alice/daemon.log"
        wait(lambda: s.inserts and "dropped the first ack" in text(dlog), 5)
        st2 = state()
        dropped = lines(dlog, "dropped the first ack (lose-ack)", N)
        check(473, 2, "插入一次、丢掉回执", len(s.inserts) == 1 and "T473" in s.inserts[0] and dropped and st2 == "pushed",
              f"插入 {len(s.inserts)} 次,含 T473:{'是' if s.inserts and 'T473' in s.inserts[0] else '否'};"
              f"丢回执日志 {len(dropped)} 行;state={st2}")

        start = time.time()
        got = wait(lambda: state() == "delivered", 120, every=5)
        requeued = "delivery.requeue: ack timed out" in text(P.LOG)
        check(473, 3, "超时退回后重推并送达", got and requeued,
              f"state={state()} 用时={time.time() - start:.0f}s;退回日志:{'有' if requeued else '没有'}")

        ins = lines(dlog, INSERTED, N)
        check(473, 4, "没有重复插入", len(s.inserts) == 1 and len(ins) == 1,
              f"插入 {len(s.inserts)} 次;「{INSERTED}」{len(ins)} 行")
    finally:
        s.close()


def tc474():
    P.up("alice:bob")
    s = Session("alice", "474001", "t474")
    try:
        alice = {"bearer": P.app_token("alice@imbo.test")}
        wait(lambda: agent(alice, "t474").get("online"), 20)
        with open(f"{L}/work/n474.txt", "w") as f:
            f.write("T474\n")
        OUT.append("前置:假会话 t474 在线;$L/work/n474.txt 5 字节")

        stop_daemon("alice")
        off = wait(lambda: agent(alice, "t474").get("online") is False, 10)
        check(474, 1, "停掉常驻程序后会话离线", off, f"online={agent(alice, 't474').get('online')}")

        code, out = imbo("bob", "send", "alice/t474", "T474", "--file", f"{L}/work/n474.txt")
        m = re.search(r"Sent #(\d+) with 1 file", out)
        N = int(m.group(1)) if m else None
        check(474, 2, "bob 发带文件的消息", code == 0 and N, f"退出码 {code};{out.splitlines()[-1] if out else '无输出'}")

        r = subprocess.run([os.environ["S"], "dev", "age-files", "--message", str(N), "--hours", "169"],
                           capture_output=True, text=True, env={**os.environ, "IMBO_DATA_DIR": f"{L}/server"})
        aged = r.stdout.strip()
        check(474, 3, "附件老化", aged == "aged 1 file(s) by 169 h; file.cleanup ran", aged or r.stderr.strip())

        start_daemon("alice")
        wait(lambda: s.inserts, 40)
        want = "n474.txt (5 B) not downloaded: expired; the server keeps files for 7 days"
        body = s.inserts[0] if s.inserts else ""
        local = os.path.exists(f"{L}/alice/files/{N}/n474.txt")
        check(474, 4, "上线后插入并写明已过期",
              len(s.inserts) == 1 and "T474" in body and want in body
              and "Fetch the attachments again" not in body and not local,
              f"插入 {len(s.inserts)} 次;已过期一行:{'有' if want in body else '没有'};"
              f"重取提示:{'有' if 'Fetch the attachments again' in body else '没有'};本地文件:{'有' if local else '没有'}")

        def delivered_state():
            _, b, _ = call("GET", "/v1/conversations", **alice)
            c = next((c for c in b["conversations"] if (c.get("peer") or {}).get("name") == "bob"), {})
            _, b, _ = call("GET", f"/v1/conversations/{c.get('id')}/messages", **alice)
            return next((m for m in b["messages"] if m["id"] == N), {})

        msg = wait(lambda: (m := delivered_state()) and m.get("state") is None and m, 10)
        check(474, 5, "送达后不再带 state", bool(msg), f"state={delivered_state().get('state')}")
    finally:
        s.close()


def event(name, **fields):
    """Server log lines of `event="<name>"` with these fields, written as
    the log writes them (strings in quotes)."""
    pats = [re.compile(rf"\b{k}={re.escape(str(v))}(\s|$)") for k, v in fields.items()]
    return [l for l in text(P.LOG).splitlines()
            if f'event="{name}"' in l and all(p.search(l) for p in pats)]


def tc478():
    P.up("alice:bob")
    s = Session("alice", "478001", "t478")
    try:
        imbo("alice", "dev", "fake-session", f"{SID}478002", "--title", "t478f")
        i = P.ids()
        A, B = i["alice"], i["bob"]
        alice = {"bearer": P.app_token("alice@imbo.test")}
        bob = {"bearer": P.app_token("bob@imbo.test")}
        G = wait(lambda: agent(alice, "t478").get("online") and agent(alice, "t478")["id"], 20)
        F = agent(alice, "t478f").get("id")
        OUT.append(f"前置:假会话 t478 <G>={G};没有插入通道的 t478f <F>={F}")

        st, b, _ = P.sign_in("new478@imbo.test", "/v1/app/session", {"platform": "ios", "name": "n478"}, False)
        N = ((b or {}).get("account") or {}).get("id")
        got = wait(lambda: event("account_registered", account_id=N, via='"app"'), 5)
        check(478, 1, "新账号", st == 200 and (b or {}).get("new_account") is True and got,
              f"{st} new_account={(b or {}).get('new_account')};account_registered:{got[:1]}")

        st, b, _ = call("POST", f"/v1/agents/{G}/messages", {"body": "T478-1", "client_msg_id": "t478-1"}, **alice)
        M = ((b or {}).get("message") or {}).get("id")

        def three():
            return (event("message_sent", message_id=M, account_id=A, conversation_kind="direct",
                          recipients=1, online_recipients=1),
                    event("delivery_pushed", message_id=M, attempt=1),
                    [l for l in event("delivery_acked", message_id=M, via='"inject"') if "latency_ms=" in l])

        wait(lambda: all(three()), 5)
        sent, pushed, acked = three()
        check(478, 2, "发消息、推送、回执", st == 201 and sent and pushed and acked,
              f"{st} <M>={M};message_sent {len(sent)} 行、delivery_pushed {len(pushed)} 行、delivery_acked {len(acked)} 行")

        st, b, _ = call("POST", f"/v1/agents/{F}/messages", {"body": "T478-2", "client_msg_id": "t478-2"}, **alice)
        M2 = ((b or {}).get("message") or {}).get("id")
        back = wait(lambda: event("delivery_requeued", message_id=M2, reason='"no-channel"'), 5)
        check(478, 3, "退回待送达", st == 201 and back, f"{st} <M2>={M2};delivery_requeued:{back[:1]}")

        codes = [call("POST", "/v1/messages", {"to": str(A), "body": f"T478-r{n}", "client_msg_id": f"t478-r{n}"},
                      **bob)[0] for n in range(1, 32)]
        limited = event("rate_limited", scope='"conversation"', account_id=B)
        check(478, 4, "限流", codes == [201] * 30 + [429] and limited,
              f"201 {codes.count(201)} 次、429 {codes.count(429)} 次(第 31 条 {codes[-1]});rate_limited:{limited[:1]}")
    finally:
        s.close()


def main():
    run = sys.argv[1]
    want = sys.argv[2:] or ["473", "474", "478"]
    head = os.environ.get("RUN_HEADER")
    root = os.path.dirname(os.path.dirname(HERE))
    for tc in want:
        OUT.clear()
        if head:
            OUT.append(head.replace("<TC>", f"TC-{tc}"))
        try:
            globals()[f"tc{tc}"]()
        except Exception as e:  # a step the build can't take yet still leaves a record
            check(int(tc), 0, "中断", False, f"{type(e).__name__}: {e}")
        with open(f"{root}/docs/test-runs/{run.lower()}-tc-{tc}.txt", "w") as f:
            f.write("\n".join(OUT) + "\n")
    subprocess.run(["sh", P.ENV, "down"], capture_output=True)
    print(f"failed steps: {P.FAILS}")
    sys.exit(1 if P.FAILS else 0)


if __name__ == "__main__":
    main()
