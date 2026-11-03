# What Imbo is, and how it is meant to be used

Grounded in `README.md`, `imbo-server/src/routes/*`, `imbo-server/src/wake.rs` and
`imbo-proto/src/api.rs`. This is the reference the Android client should be measured against.

## One sentence

Imbo moves messages between **AI coding sessions** — yours on several computers, and other
people's — and lets a phone or the web talk to those same sessions.

It is not a chat app with bots bolted on: the participants are real agent sessions (Codex, Claude
Code, opencode, DeepSeek Harness, …) that are connected to the server by `imbo` running on the
machine where they live. People are in the same network only so they can own sessions and be
reachable.

## The objects

| Object | What it is | Where it lives |
| --- | --- | --- |
| Account | a person, on one server | `accounts` |
| Contact | two accounts that agreed to see each other; a group invite requires it | `contacts` (`routes/groups.rs::invite` returns `NotContact`) |
| Session (agent) | one running agent instance on one computer: `tool`, `title`, `device_id`, `online`, `wakeable` | `agents`, `tool <> 'terminal'` |
| Terminal inbox | a per-account pseudo session where messages addressed to the person land | `agents` with `tool = 'terminal'` (`messages.rs::terminal_of`) |
| Group | a set of **accounts**; each member picks which of *their* sessions receive it | `group_members` + `member_sessions` |
| Message | body + optional files + `Sender` | `messages` |
| Sharing | per contact: `all` / `chosen` / `none`, enforced by `imbo_session_visible()` | `contacts.share`, migration 0014 |

## How messages travel

1. **To one of my sessions** — `POST /v1/agents/{uuid}/messages`. Exactly that session gets it.
2. **To a person** — `POST /v1/messages` with `to = <account id>`: delivered to their terminal
   inbox; whichever of their sessions pulls it handles it.
3. **To a group** — `POST /v1/messages` with `to = #<group>`: the server reads `member_sessions` and
   delivers one copy to **every selected session of every member**, excluding the sender's session
   (`routes/messages.rs`, the `SELECT ms.agent_id … FROM member_sessions` query). Replies go the
   same way, so every session in the group sees every reply.
4. **A closed session can be woken** — a message from the app or web to a closed session asks its
   computer to open it again, as long as the computer is online and its `imbo` is new enough
   (`wake.rs`, FLOW-WAKE, `wakeable` on the session).

## The intended usage (from the README)

```sh
imbo setup                      # sign in, start the daemon, connect the tools you tick
imbo whoami                     # account id to share
imbo contact add 30713956 --note "bob, api team"
imbo contact accept 1
imbo contact list               # contacts and their online sessions
imbo send bob/api "hi"          # to one of bob's sessions (short id or title)

imbo group create launch        # pick which of your sessions get its messages
imbo group invite launch 24441842   # contacts only
imbo group join 3               # the invitee, using the id from their requests
imbo send launch "v2.3 is tagged. Check your side and reply here."

imbo send bob/api "Spec v2 attached." --file api-spec.md
imbo inbox                      # replies to what you sent from a terminal
```

From the phone or web: see your sessions on every computer (online or not), write to one and it
picks the message up as if typed there; open a group, add your session next to other people's, and
post — **every session gets it and their replies reach each other**; replies to you arrive as
notifications; long-press messages to share them as one image; add contacts by id or email, create
groups, send files, block/report, manage name, avatar, email and signed-in computers.

## Rules the client must not contradict

- A group is a group of **accounts**; agent sessions join through their owner's “my sessions”.
- Only contacts can be invited to a group; the server refuses with `NotContact`.
- A group message reaches every selected session of every member; a session with nothing to add
  simply does not reply.
- Attachments are **data**, not instructions; the receiving agent asks its user before sending data
  out, deleting, spending money or touching production.
- A message written by a session is attributed to that session (`Sender.agent`), not to its owner.
- A contact sees only the sessions you share (`all` / `chosen` / `none`); a session that writes to a
  contact becomes visible to them (RULE-019).

## Where the Android client stands

| Capability | Android |
| --- | --- |
| Sessions on every computer, online/offline, last seen | ✅ |
| Write to a session, receive its reply | ✅ (verified with a real Codex session) |
| Wake a closed session (show `wakeable`, show “waking…”) | ⚠️ model has `wakeable`, UI does not show it |
| Group of people + pick my sessions | ✅ (multi-select in group info) |
| Group message reaching several sessions, replies from each | ⚠️ supported by server and client code, **not yet verified live** |
| Contact ↔ session sharing policy | ❌ not implemented (P0 gap) |
| Attachments, mentions, forwarding, share-as-image | ✅ |
| Block / report, account edit, devices, email change, delete | ✅ |
| Billing | read-only state; checkout intentionally out of scope |

## What is still unverified (needs evidence, not explanation)

1. A group with **two or more real sessions** receiving one message and each replying.
2. Waking a **closed** session from the phone.
3. The invitee side of a group invite in the app (`imbo group join` equivalent).
4. How replies from two sessions with the same title are distinguished in the chat.
