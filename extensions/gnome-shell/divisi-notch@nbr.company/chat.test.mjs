// Run with: node chat.test.mjs (also run by `cargo test -p divisi-cli --test notch_chat_js`).
import assert from 'node:assert/strict';
import {applyEvents, chatEntry, newChat, parseTail, progressText, shownText, submitInput} from './chat.js';

const ev = (id, kind, body) => ({id, kind, body: JSON.stringify(body)});

// events become entries, and are never applied twice
{
    const c = newChat();
    const events = [
        ev(1, 'chat_user', {text: 'status', surface: 'notch'}),
        ev(2, 'chat_assistant', {text: 'Nothing is running.', degraded: true}),
        ev(3, 'node_output', {text: 'noise'}),
        ev(4, 'blocked', 'planning failed'),
    ];
    assert.equal(applyEvents(c, 's1', events), true);
    assert.equal(applyEvents(c, 's1', events), false, 'a repeat changes nothing');
    assert.deepEqual(c.entries.map(e => e.type), ['you', 'divisi', 'progress']);
    assert.equal(c.entries[1].degraded, true);
    assert.equal(c.entries[2].text, 'blocked: "planning failed"');
    assert.equal(c.cursor, 4);
}

// a confirmation stays pending until its own result arrives
{
    const c = newChat();
    applyEvents(c, 's', [ev(1, 'chat_confirm', {approval_id: 7, summary: 'cancel goal_a_1', remember_ok: true})]);
    assert.equal(c.pending, 7);
    assert.equal(c.entries[0].rememberOk, true);
    applyEvents(c, 's', [ev(2, 'chat_result', {approval_id: 8, outcome: 'approved'})]);
    assert.equal(c.pending, 7, "another approval's result does not clear it");
    applyEvents(c, 's', [ev(3, 'chat_result', {approval_id: 7, outcome: 'denied'})]);
    assert.equal(c.pending, null);
}

// typed lines: sends, and bare answers only while something is pending
{
    const c = newChat();
    assert.deepEqual(submitInput(c, '  how is the pool  '), {type: 'send', text: 'how is the pool'});
    assert.equal(submitInput(c, '   '), null);
    assert.deepEqual(submitInput(c, 'yes'), {type: 'send', text: 'yes'}, 'with nothing pending, yes is just text');
    c.pending = 7;
    assert.deepEqual(submitInput(c, 'Yes'), {type: 'confirm', approvalId: 7, allow: true, remember: false});
    assert.deepEqual(submitInput(c, 'always'), {type: 'confirm', approvalId: 7, allow: true, remember: true});
    assert.deepEqual(submitInput(c, 'no'), {type: 'confirm', approvalId: 7, allow: false, remember: false});
    assert.deepEqual(submitInput(c, 'yes but first show status'), {type: 'send', text: 'yes but first show status'});
    c.busy = true;
    assert.equal(submitInput(c, 'status'), null, 'nothing is sent while a request is in flight');
}

// a different session starts a fresh view
{
    const c = newChat();
    applyEvents(c, 'a', [ev(1, 'chat_user', {text: 'hi'})]);
    applyEvents(c, 'b', [ev(1, 'chat_user', {text: 'other'})]);
    assert.deepEqual(c.entries.map(e => e.text), ['other']);
}

// malformed bodies are shown as-is, other kinds are ignored
assert.equal(chatEntry('chat_assistant', 'not json').text, 'not json');
assert.equal(chatEntry('plan', '{}'), null);
assert.equal(chatEntry('chat_confirm', '{"summary":"x"}'), null, 'a confirmation without an id cannot be answered');
assert.equal(progressText('node_output', 'x'), null);
assert.equal(progressText('node_failed', ''), 'a step failed');

// tail output parsing carries the session id
{
    const out = [
        JSON.stringify({id: 1, kind: 'chat_user', body: '{}', session: 'sess_9'}),
        '',
        'not json',
        JSON.stringify({id: 2, kind: 'chat_assistant', body: '{}', session: 'sess_9'}),
    ].join('\n');
    const {session, events} = parseTail(out);
    assert.equal(session, 'sess_9');
    assert.deepEqual(events.map(e => e.id), [1, 2]);
    assert.deepEqual(parseTail(''), {session: null, events: []});
}

// long replies are cut short with a pointer to the TUI or Zed, short ones are shown whole
{
    assert.equal(shownText({type: 'divisi', text: 'Nothing is running.'}), 'Nothing is running.');
    assert.equal(shownText({type: 'divisi', text: 'ok', degraded: true}), 'ok  (rules only)');
    const long = 'word '.repeat(400).trim();
    const cut = shownText({type: 'divisi', text: long});
    assert.ok(cut.length < 700, `a long reply is cut (got ${cut.length} chars)`);
    assert.match(cut, /open it in the TUI or Zed/);
    assert.ok(!/\bwor\b|\bwo\b|\bw\b/.test(cut.split('…')[0]), 'the cut falls on a word boundary');
    const tall = Array.from({length: 40}, (_, i) => `line ${i}`).join('\n');
    const cutTall = shownText({type: 'divisi', text: tall, degraded: true});
    assert.ok(cutTall.split('\n').length <= 13, 'a reply with many short lines is cut too');
    assert.match(cutTall, /open it in the TUI or Zed/);
    assert.match(cutTall, /\(rules only\)/, 'the rules-only note survives the cut');
    assert.equal(shownText({type: 'you', text: long}), long, 'only replies are cut');
}

// the limits are inclusive: exactly 600 characters or 10 lines is shown whole, one more is cut
{
    const hint = /open it in the TUI or Zed/;
    const at = 'a'.repeat(600);
    assert.equal(shownText({type: 'divisi', text: at}), at);
    assert.match(shownText({type: 'divisi', text: `${at} b`}), hint);
    const ten = Array.from({length: 10}, (_, i) => `line ${i}`).join('\n');
    assert.equal(shownText({type: 'divisi', text: ten}), ten);
    const eleven = shownText({type: 'divisi', text: `${ten}\nline 10`});
    assert.match(eleven, hint);
    assert.ok(eleven.startsWith(`${ten}…`), 'the first ten lines are kept whole');
    assert.ok(!eleven.includes('line 10'));
}

// trailing whitespace alone is not a longer reply
{
    const ten = Array.from({length: 10}, (_, i) => `line ${i}`).join('\n');
    assert.equal(shownText({type: 'divisi', text: `${ten}\n`}), `${ten}\n`);
    const at = 'a'.repeat(600);
    assert.equal(shownText({type: 'divisi', text: `${at}   `}), `${at}   `);
}

// a reply with no space to cut on is cut hard at the limit rather than kept whole
{
    const token = 'x'.repeat(2000);
    const cut = shownText({type: 'divisi', text: token});
    assert.equal(cut.split('…')[0], 'x'.repeat(600));
    assert.match(cut, /open it in the TUI or Zed/);
}

// confirmations, results and progress notes are never cut, and the entry itself is left alone
{
    const long = 'word '.repeat(400).trim();
    for (const type of ['confirm', 'result', 'progress'])
        assert.equal(shownText({type, text: long}), long, `${type} is shown whole`);
    const entry = {type: 'divisi', text: long, degraded: true};
    shownText(entry);
    assert.deepEqual(entry, {type: 'divisi', text: long, degraded: true});
}

// a long degraded reply arriving from the tail is what the row cuts and marks
{
    const c = newChat();
    const long = 'word '.repeat(400).trim();
    applyEvents(c, 's', [ev(1, 'chat_assistant', {text: long, degraded: true})]);
    assert.equal(c.entries[0].text, long, 'the stored entry keeps the whole reply');
    assert.match(shownText(c.entries[0]), /open it in the TUI or Zed\)  \(rules only\)$/);
    assert.equal(shownText(chatEntry('chat_assistant', 'not json')), 'not json');
}

console.log('chat.js: all checks passed');
