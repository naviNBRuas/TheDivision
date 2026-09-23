# Decisions waiting on a person

Questions an agent could not settle on its own. Answer inline and remove the entry once acted on.

## Notch chat: how much of a long reply should the card show? (2026-09-23)

`shownText` in `extensions/gnome-shell/divisi-notch@nbr.company/chat.js` cuts a reply at
600 characters or 10 lines, whichever is hit first. In a headless gnome-shell check, the
600-character limit was the one that applied: at 12px in the 440 px card, 600 characters wrap
to about 13 lines, which is most of the card's height. The input box stays visible, but little
of the rest of the conversation does.

- Keep 600 / 10 as they are, or
- lower `MAX_REPLY_CHARS` (around 300 would keep a long reply to about 6–7 wrapped lines).

It is a one-line change either way. The node tests in `chat.test.mjs` pin 600 at the
boundary, so change them with it.
