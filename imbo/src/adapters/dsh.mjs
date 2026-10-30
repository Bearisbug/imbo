// Imbo for DeepSeek Harness: written by `imbo setup`, removed by `imbo uninstall`.
// Gives each session Imbo's tools, and puts messages from other sessions
// into it through a socket only Imbo's background daemon knows the token of.

import { randomBytes } from "node:crypto";
import { mkdtempSync, realpathSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const IMBO = __IMBO__;
const ENV = __ENV__;
const NOTE = __NOTE__;

export const name = "imbo";
export const inject = ["agents", "systemPrompt"];

// This file sits outside dsh's install, where dsh's own packages can't be
// found by name; they are looked up from the dsh that is running.
async function load(pkg) {
	try {
		return await import(pkg);
	} catch {
		const require = createRequire(realpathSync(process.argv[1]));
		return import(pathToFileURL(require.resolve(pkg)).href);
	}
}

export async function apply(ctx) {
	let mcpClient, createUserMessage;
	try {
		mcpClient = await load("@deepseek-ai/dsh-mcp-client");
		({ createUserMessage } = await load("@deepseek-ai/dsh-llm"));
	} catch (e) {
		console.error(`Imbo: couldn't load DeepSeek Harness's modules, so its sessions get no Imbo tools or messages (${e})`);
		return;
	}

	const token = randomBytes(24).toString("hex");
	const dir = mkdtempSync(join(tmpdir(), "imbo-dsh-"));
	const socket = join(dir, "in.sock");
	// Subagents' sessions are parts of another session's turn.
	const ours = (agent) => agent.session?.header?.origin !== "subagent";
	const servers = new Map();

	// Each session gets its own `imbo mcp`, which knows the session from its
	// environment.
	const attach = (agent) => {
		if (!ours(agent) || servers.has(agent.id)) return;
		servers.set(agent.id, agent.ctx.plugin(mcpClient, {
			serverName: "imbo",
			transport: "stdio",
			command: IMBO,
			args: ["mcp"],
			env: { ...ENV, IMBO_DSH_SESSION: agent.id, IMBO_DSH_SOCKET: socket, IMBO_DSH_TOKEN: token },
		}));
	};

	// One request per connection, one JSON line each way: `deliver` puts a
	// message into a session, `info` lists the sessions this dsh has open.
	const server = createServer((conn) => {
		let buf = "";
		conn.setEncoding("utf8");
		conn.on("error", () => {});
		conn.on("data", (chunk) => {
			buf += chunk;
			const nl = buf.indexOf("\n");
			if (nl < 0) return;
			let reply;
			try {
				const req = JSON.parse(buf.slice(0, nl));
				if (req.token !== token) {
					reply = { ok: false, error: "wrong token" };
				} else if (req.op === "deliver" && typeof req.session === "string" && typeof req.text === "string") {
					const agent = ctx.agents.get(req.session);
					if (!agent || !ours(agent)) {
						reply = { ok: false, error: "no such session open" };
					} else {
						// An idle session starts a turn; a busy one takes it after
						// the tool that runs. As the user's kind it shows in full,
						// not as a folded notice.
						agent.steer(createUserMessage({ content: [{ type: "text", text: req.text }], source: { kind: "user" } }));
						reply = { ok: true };
					}
				} else if (req.op === "info") {
					reply = { ok: true, sessions: ctx.agents.list().filter(ours).map((a) => a.id) };
				} else {
					reply = { ok: false, error: "unknown request" };
				}
			} catch (e) {
				reply = { ok: false, error: String(e) };
			}
			conn.end(JSON.stringify(reply) + "\n");
		});
	});

	ctx.effect(() => {
		server.listen(socket);
		return () => {
			server.close();
			rmSync(dir, { recursive: true, force: true });
			for (const s of servers.values()) s?.dispose?.();
			servers.clear();
		};
	});

	ctx.on("agent/created", ({ agent }) => attach(agent));
	ctx.on("agent/disposed", ({ agent }) => servers.delete(agent.id));
	// Sessions already open when Imbo was connected to a running dsh.
	for (const agent of ctx.agents.list()) attach(agent);

	// Without this, models take a message from another session for a prompt
	// injection and refuse it. Minimal mode leaves it out; the rules at the
	// end of each message still say how to reply.
	ctx.systemPrompt.section({ name: "imbo", order: 9000, text: NOTE });
}
