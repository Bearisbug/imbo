# Requirements gaps

Companion to `requirements-matrix.md`. Each item names the rule it violates or the surface it is
missing, plus the evidence that the rule exists server-side.

## P0 — client contradicts a server rule

**1. Session sharing is not implemented at all**

The server lets a contact see only the sessions you share (`contacts.share` = `all` / `chosen` /
`none`, enforced by `imbo_session_visible()` in `migrations/0014_contact_share.sql`, used by
`share.rs` and the `/v1/agents` listing). A session that writes to a contact is revealed to them
automatically (RULE-019, `share.rs::reveal`).

- Android has no way to set the policy: `PUT /v1/contacts/{account_id}/share` is the only route in
  `routes/mod.rs` that `RemoteImboApi` does not call (besides billing and CLI-only inbox routes).
- Consequence: a user who switched a contact to “chosen” on the web/iOS app cannot see or change it
  on Android, and the contact page shows no sharing state.
- Fix shape: a “共享的会话” section in the contact sheet, reading/writing `share` +
  `shared_session_ids`; one screen, one request.

**2. Message attribution (fixed this round, needs to stay fixed)**

`Sender.agent` present means the message came from a session; only `from_me` means the person.
The client previously rendered session replies as “我”. Fixed in `Mapper.message`:
`senderName = agent.title ?: name`, `mine = fromMe || (accountId == myId && agent == null)`.
Regression risk: any new message list that maps `Sender` by hand.

## P1 — product-relevant, not blocking

**3. Delivery state is invisible.** `MessageView.state` distinguishes `pending` / `pushed` /
`delivered` / `canceled` for messages to your own sessions. Android only shows send failures, so
“the agent has not picked it up yet” looks identical to “delivered”.

**4. Group membership semantics are not explained.** The UI lists members as people and separately
lists “我的会话”, which matches the server, but nothing tells the user that a group is a group of
**accounts** and that their own sessions join through that section. The most likely confusion —
“can I invite an outside person?” — is answered only by the server error `not your contact`.

**5. Contacts page has no session visibility.** Even without the P0 editor, the contact sheet could
show “对方能看到你的 2 个会话”, which is cheap and removes the surprise of a hidden session.

## P2 — deliberate or low value

**6. Billing checkout / portal / Alipay** — `/v1/billing` read-only is enough; subscriptions were
explicitly out of scope.

**7. CLI-only endpoints** — `/v1/inbox`, `/v1/inbox/pull`, `/v1/messages/{id}` are for `imbo inbox`;
the app uses conversations instead.

**8. Windows-only agent UIs** — a Codex desktop app on Windows cannot see sessions unless the `imbo`
daemon runs there; `imbo` is macOS/Linux only (README “Supported: macOS and Linux”). On Windows the
workable path is WSL, which is what the verification used.

## Open questions for the product owner

1. Is “chosen sessions per contact” a feature the Android client must reach parity on before release?
   (Server supports it; web/iOS presumably expose it — not verifiable from this repo.)
2. Should a group of people ever contain *another person's* sessions directly, or is “sessions join
   through their owner” the intended model? The server says the latter; the client currently makes
   that implicit.
