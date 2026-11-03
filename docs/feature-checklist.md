# Feature checklist: official Imbo vs the Android client

Sources: `README.md` (Use / iPhone and web / Pricing / Self-hosting), `docs/product-guide.md`,
`docs/requirements-matrix.md`, `docs/requirements-gaps.md`, and the server routes. Status means:

- ✅ implemented and exercised in this session (device or real server)
- ⚠️ implemented but not verified live, or only partially surfaced
- ❌ not implemented

## Account and identity

| # | Official behaviour | Android | Evidence / gap |
| --- | --- | --- | --- |
| 1 | Sign in with an email code | ✅ | real server, fixed review code |
| 2 | Signed-in computers, revoke one | ✅ | `/v1/devices` |
| 3 | Rename, avatar, change email, delete account | ✅ | account screen |
| 4 | Sign out | ✅ | |
| 5 | Contact note (`imbo contact add --note`) | ❌ | no note field in the contact UI |
| 6 | Per-contact session sharing (`all` / `chosen` / `none`) | ✅ | contact row 「共享」 → three modes; `chosen` lists sessions with checkmarks; verified server state `{"share":"chosen","sessions":[codex]}` |

## Sessions (agents)

| # | Official behaviour | Android | Evidence / gap |
| --- | --- | --- | --- |
| 7 | Sessions on every computer, online or not | ✅ | `codex · DESKTOP-CL5GDEG · 在线` |
| 8 | Write to a session, it replies | ✅ | `hello-from-phone` → `Hello! Got your message from your phone.` |
| 9 | Replies are attributed to the session | ✅ | fixed this session (`Sender.agent`) |
| 10 | Rename a session | ✅ | `PUT /v1/agents/{id}/title` |
| 11 | Wake a closed session from the phone | ⚠️ | UI shows 「离线（可唤醒）」, but both Codex sessions report `wakeable=false`, so no wake is ever offered; the flow could not be exercised on this setup |
| 12 | Delivery state for my own session's messages | ✅ | `MessageView.state` now renders as 「等待会话领取」/「已取消」; server returned `state=delivered` for message #148 |
| 13 | Session visibility follows sharing | ✅ | the share dialog shows 「对方能看到的会话（1 / 2）」 and lets the policy be changed |

## Contacts

| # | Official behaviour | Android | Evidence / gap |
| --- | --- | --- | --- |
| 14 | Add by account id or email | ✅ | |
| 15 | Accept / decline requests | ✅ | |
| 16 | Remove a contact (history kept) | ✅ | |
| 17 | Block / unblock, report | ✅ | |

## Groups

| # | Official behaviour | Android | Evidence / gap |
| --- | --- | --- | --- |
| 18 | Create a group, pick which of my sessions receive it | ✅ | new-group screen lists my sessions and sends the checked ids; `picker-test` came back with `sessions=d2012d` |
| 19 | Invite contacts only | ✅ | server rejects non-contacts; the error is surfaced |
| 20 | Invitee joins the group | ✅ | on the phone as bob: 「邀请你加入「invite-ui-test」」 → 接受; server members alice,bob; alice's message then appeared in bob's chat |
| 21 | A group message reaches every selected session of every member | ✅ | two Codex sessions in group 4229147: send returned `"recipients":2`; both TUIs showed message #144 |
| 22 | Replies reach every session in the group | ✅ | session `1a3cb5` posted #147 (`Sender.agent = Acknowledge phone hello`); the other session saw it |
| 23 | Rename, remove member, transfer owner, dissolve, leave | ✅ | |
| 24 | Group avatar | ✅ | |
| 25 | @-mention a member | ✅ | picker + server `mention` flag |

## Messages

| # | Official behaviour | Android | Evidence / gap |
| --- | --- | --- | --- |
| 26 | Direct chat with a person | ✅ | |
| 27 | Attachments: files, gallery, camera | ✅ | camera end-to-end, 3.7 MB image on server |
| 28 | Forward a message | ✅ | |
| 29 | Share selected messages as one image | ✅ | `ShareImage`, system share sheet |
| 30 | In-chat search, unread divider, jump to latest | ✅ | |
| 31 | Failed send is retryable | ✅ | tap the bubble |
| 32 | Same-title sessions distinguishable | ✅ | session list appends a uuid tail when titles repeat; chat sender labels now append the agent short id when two senders share a title |

## Notifications

| # | Official behaviour | Android | Evidence / gap |
| --- | --- | --- | --- |
| 33 | A reply to me arrives as a notification | ✅ | ntfy, title `bob`, body text |
| 34 | Tapping it opens the chat | ✅ | `imbo://chat/<id>` |
| 35 | Mention in a group notifies separately | ✅ | server mention wording |
| 36 | Ask the system for notification permission | ✅ | first launch after sign-in |

## Billing

| # | Official behaviour | Android | Evidence / gap |
| --- | --- | --- | --- |
| 37 | Show trial / paid / expired state | ✅ | `/v1/billing` read-only |
| 38 | Subscribe / manage payment | ❌ | deliberately out of scope |

## Self-hosting and build

| # | Official behaviour | Android | Evidence / gap |
| --- | --- | --- | --- |
| 39 | One-command server + ntfy stack | ✅ | `deploy/docker/setup.sh`, both health checks |
| 40 | Public https without opening ports | ✅ | `tailscale serve` on this deployment |
| 41 | Signed release without the key in CI | ✅ | `release.ps1` locally; CI runs tests only |
| 42 | Client works against any deployment | ✅ | server/ntfy addresses are runtime settings |

## Count

| Status | Items |
| --- | --- |
| ✅ implemented and exercised | 39 |
| ⚠️ implemented, not verified or partial | 1 |
| ❌ missing | 2 (contact note, subscription checkout) |
| **Total tracked** | **42** |

The three ❌ are deliberate except one: contact session sharing has its API but no screen, and it is
the only place where the client currently contradicts a server rule. The ⚠️ items are all
*verification* gaps (two-session group, wake, invitee join, delivery state) rather than missing
plumbing, except #18 (session picker at creation) and #32 (same-title sessions).
