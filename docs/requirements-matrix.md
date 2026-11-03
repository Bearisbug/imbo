# Requirements matrix: what Imbo is, and where the Android client stands

Sources: `README.md`, `imbo-proto/src/api.rs`, `imbo-server/src/routes/*`, `imbo-server/src/share.rs`,
`imbo-server/migrations/0014_contact_share.sql`, and the Android client in `android/`.
Every row cites the server file that defines the rule; "Android" marks implementation, not intent.

## Accounts, contacts, sessions

| Capability | Rule (server) | Android | Notes |
| --- | --- | --- | --- |
| Sign in with an email code | `routes/auth.rs` | ✅ | review code path used in local tests |
| Contacts are account-level | `contacts` table keyed by `(account_id, peer_id)` | ✅ | add by ID or email |
| Contact requests must be accepted | `routes/contacts.rs` `request_contact` / `accept` | ✅ | |
| A contact sees only the sessions you share | `contacts.share` + `imbo_session_visible()` (`migrations/0014_contact_share.sql`, `share.rs`) | ❌ | **P0 gap**, see gaps doc |
| Sending from a session reveals it to that peer | `share.rs::reveal`, RULE-019 | ❌ (server does it; client shows nothing) | automatic, no UI needed — but the client should not contradict it |
| Sessions belong to the machine running `imbo` | `agents.device_id`, `/v1/agents` | ✅ | a Windows Codex desktop has no daemon, so its sessions never appear |

## Conversations and messages

| Capability | Rule (server) | Android | Notes |
| --- | --- | --- | --- |
| Direct chat with a person | `POST /v1/messages` with `to=<account_id>` | ✅ | |
| Chat with one of your own sessions | `POST /v1/agents/{uuid}/messages` | ✅ | verified end-to-end with Codex in WSL |
| Message authored by a session vs by the person | `Sender.agent` present ⇒ session; `from_me` ⇒ the person (`api.rs`) | ✅ (fixed) | was mis-attributed as “me” until this round |
| Delivery state | `MessageView.state` (`pending`/`pushed`/`delivered`/`canceled`) | ⚠️ | only send-failure is shown; server states are not surfaced |
| Attachments | `routes/files.rs` + `files` on send | ✅ | camera, gallery, files |
| Mentions in a group | `@name` + server `mention` flag (`routes/messages.rs`) | ✅ | |
| Forward / multi-select export | client-side | ✅ | not a server feature |

## Groups

| Capability | Rule (server) | Android | Notes |
| --- | --- | --- | --- |
| A group member is an **account**, not a session | `group_members(group_id, account_id, role)` | ✅ | UI lists people |
| Inviting requires the target to be **your contact** | `routes/groups.rs::invite` returns `NotContact` otherwise | ✅ | error is surfaced |
| So a group can include **other people** | same rule — any contact on this server | ✅ | **not** limited to your own agents |
| Which of *your* sessions receive the group | `member_sessions` via `PUT /v1/groups/{id}/sessions` | ✅ | the “我的会话” section |
| Owner-only actions: rename, remove, transfer, avatar, dissolve | `routes/groups.rs` (`require_owner`) | ✅ | |
| Leave vs dissolve | `DELETE /members/me`, `DELETE /groups/{id}` | ✅ | owner must transfer/dissolve first |

## Client surface

| Capability | Official surface | Android | Notes |
| --- | --- | --- | --- |
| Conversation list, unread, sessions tab | README “iPhone and web” | ✅ | |
| Push notifications | Expo for iOS; App pushes via `POST /v1/app/push` | ✅ | ntfy on Android (self-hosted) |
| Billing: trial/paid state | `/v1/billing` | ⚠️ | shows `access.state`, no checkout (deliberately out of scope) |
| Web/iPhone parity for sharing sessions | `/v1/contacts/{id}/share` | ❌ | P0 gap |

## Endpoint coverage

Android (`RemoteImboApi`) covers every route in `routes/mod.rs` except:

- `PUT/PATCH /v1/contacts/{account_id}/share` — session sharing policy (P0)
- `POST /v1/billing/checkout`, `/v1/billing/portal`, `/v1/billing/alipay` — subscriptions (out of scope)
- `GET /v1/inbox`, `POST /v1/inbox/pull`, `GET /v1/messages/{id}` — CLI-facing, not needed by the app
