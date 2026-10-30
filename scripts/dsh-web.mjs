// Drives a DeepSeek Harness web host for the TC-350 test session
// (docs/TEST.md §3) through the same /api calls its web page makes.
//   node crates/scripts/dsh-web.mjs <login URL> new "<text>"   prints the session ID
//   node crates/scripts/dsh-web.mjs <login URL> send|log|archive|unarchive|open <session> ["<text>"]
//   node crates/scripts/dsh-web.mjs <login URL> shot <session> <png>
// <login URL> is the http://127.0.0.1:<port>/?token=… line dsh prints. Only
// `shot` opens a browser: Playwright's chromium-headless-shell, closed at once.
import { randomUUID } from 'node:crypto'
import { createRequire } from 'node:module'
import { execSync } from 'node:child_process'

const [login, cmd, session, arg] = process.argv.slice(2)
const origin = new URL(login).origin

// The token in the URL buys a cookie, as when the page loads.
async function cookie() {
	const r = await fetch(login, { redirect: 'manual' })
	const c = (r.headers.getSetCookie?.() ?? []).map((s) => s.split(';')[0]).join('; ')
	if (!c) throw new Error(`no cookie from ${origin} (HTTP ${r.status})`)
	return c
}

async function call(jar, method, args) {
	const r = await fetch(`${origin}/api/${method}`, {
		method: 'POST',
		headers: { 'content-type': 'application/json', cookie: jar },
		body: JSON.stringify({ type: 'client-request', rpcId: randomUUID(), method, payload: { args } }),
	})
	if (!r.ok) throw new Error(`${method}: HTTP ${r.status} ${await r.text()}`)
	const { result } = await r.json()
	if (!result.ok) throw new Error(`${method}: ${result.error.code} ${result.error.message}`)
	return result.value
}

const prompt = (jar, sessionId, text) =>
	call(jar, 'session/prompt', {
		request: { requestId: randomUUID(), sessionId, mode: 'queue', content: [{ type: 'text', text }] },
	})

// One line per message and tool call, oldest first.
async function log(jar, sessionId) {
	const { asOfSeq } = await call(jar, 'session/projections', { request: { sessionId } })
	const page = await call(jar, 'session/page', {
		request: { address: { kind: 'session', sessionId }, throughSeq: asOfSeq, maxMessages: 500 },
	})
	const text = (content = []) =>
		content
			.filter((p) => p.type === 'text')
			.map((p) => p.text)
			.join(' ')
	for (const { event: e } of page.records) {
		const d = e.data ?? {}
		// Runtime and time notes dsh adds are user messages of other kinds.
		if (e.type === 'user/message' && d.source?.kind === 'user') console.log(`[${e.seq}] user: ${text(d.content)}`)
		else if (e.type === 'assistant/message' && text(d.message?.content)) console.log(`[${e.seq}] assistant: ${text(d.message.content)}`)
		else if (e.type === 'tool/call') console.log(`[${e.seq}] tool ${d.name}: ${d.arguments}`)
		else if (e.type === 'tool/result') console.log(`[${e.seq}] result: ${text(d.message?.content).slice(0, 400)}`)
		else if (e.type === 'turn/start' || e.type === 'turn/end') console.log(`[${e.seq}] ${e.type}`)
	}
}

// The page has no address per session: the session is clicked by its title
// in the sidebar.
async function shot(jar, sessionId, png) {
	const { values } = await call(jar, 'session/projections', { request: { sessionId } })
	const root = execSync('npm root -g').toString().trim()
	const { chromium } = createRequire(`${root}/playwright/`)('playwright')
	const browser = await chromium.launch()
	try {
		const page = await browser.newPage({ viewport: { width: 1400, height: 900 } })
		await page.goto(login, { waitUntil: 'domcontentloaded' })
		// The preview notice shown on first use.
		await page.getByRole('button', { name: 'Continue' }).click({ timeout: 8000 }).catch(() => {})
		await page.getByText(values.title, { exact: true }).first().click({ timeout: 10000 })
		await page.waitForTimeout(4000)
		await page.screenshot({ path: png })
	} finally {
		await browser.close()
	}
}

const jar = await cookie()
switch (cmd) {
	case 'new': {
		const { workspace } = await call(jar, 'workspace/initializeDefault', {})
		const { sessionId } = await call(jar, 'session/create', { request: { workspaceId: workspace.workspaceId } })
		// The page creates the session when it opens and the user types after;
		// a prompt sent at once starts before the session's MCP servers are up.
		await new Promise((r) => setTimeout(r, 3000))
		await prompt(jar, sessionId, session)
		console.log(sessionId)
		break
	}
	case 'send':
		await prompt(jar, session, arg)
		break
	case 'log':
		await log(jar, session)
		break
	case 'archive':
		await call(jar, 'workspace/archiveSession', { request: { sessionId: session } })
		break
	case 'unarchive':
		await call(jar, 'workspace/unarchiveSession', { request: { sessionId: session } })
		break
	// What the page asks when a session is clicked; it loads the session.
	case 'open':
		await call(jar, 'commands/list', { agentId: session })
		break
	case 'shot':
		await shot(jar, session, arg)
		break
	default:
		throw new Error(`unknown command ${cmd}`)
}
