// Imbo for pi: written by `imbo setup`, removed by `imbo uninstall`.
// Gives each session Imbo's tools, and puts messages from other sessions
// into it through a socket only Imbo's background daemon knows the token of.

import { randomBytes } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const IMBO: string = __IMBO__;
const ENV: Record<string, string> = __ENV__;
const NOTE: string = __NOTE__;

export default function (pi: ExtensionAPI) {
	let server: Server | undefined;
	let dir: string | undefined;

	const close = () => {
		server?.close();
		server = undefined;
		if (dir) rmSync(dir, { recursive: true, force: true });
		dir = undefined;
	};

	// One request per connection, one JSON line each way: `deliver` puts a
	// message into the session, `info` says it is still this one.
	const serve = (token: string, title: () => string) =>
		createServer((conn) => {
			let buf = "";
			conn.setEncoding("utf8");
			conn.on("error", () => {});
			conn.on("data", (chunk) => {
				buf += chunk;
				const nl = buf.indexOf("\n");
				if (nl < 0) return;
				let reply: Record<string, unknown>;
				try {
					const req = JSON.parse(buf.slice(0, nl));
					if (req.token !== token) {
						reply = { ok: false, error: "wrong token" };
					} else if (req.op === "deliver" && typeof req.text === "string") {
						// An idle session starts a turn; a busy one takes it
						// after the tool that runs, keeping the user's draft.
						pi.sendUserMessage(req.text, { deliverAs: "steer" });
						reply = { ok: true };
					} else if (req.op === "info") {
						reply = { ok: true, title: title() };
					} else {
						reply = { ok: false, error: "unknown request" };
					}
				} catch (e) {
					reply = { ok: false, error: String(e) };
				}
				conn.end(JSON.stringify(reply) + "\n");
			});
		});

	pi.on("session_start", async (_event, ctx) => {
		close();
		const token = randomBytes(24).toString("hex");
		dir = mkdtempSync(join(tmpdir(), "imbo-pi-"));
		const socket = join(dir, "in.sock");
		const title = () => {
			const name = pi.getSessionName();
			if (name) return name;
			for (const e of ctx.sessionManager.getEntries() as any[]) {
				const m = e.type === "message" ? e.message : undefined;
				if (m?.role !== "user") continue;
				const text = typeof m.content === "string"
					? m.content
					: (m.content ?? []).filter((c: any) => c.type === "text").map((c: any) => c.text).join(" ");
				return text.trim().split("\n")[0].slice(0, 200);
			}
			return "";
		};
		server = serve(token, title);
		await new Promise<void>((resolve, reject) => {
			server!.once("error", reject);
			server!.listen(socket, () => resolve());
		});
		pi.registerMcpServer("imbo", {
			command: IMBO,
			args: ["mcp"],
			env: {
				...ENV,
				PI_SESSION_ID: ctx.sessionManager.getSessionId(),
				IMBO_PI_SOCKET: socket,
				IMBO_PI_TOKEN: token,
			},
			exposure: "direct",
		});
	});

	pi.on("session_shutdown", () => close());

	// Without this, models take a message from another session for a
	// prompt injection and refuse it.
	pi.on("before_agent_start", (event) => ({ systemPrompt: `${event.systemPrompt}\n\n${NOTE}` }));
}
