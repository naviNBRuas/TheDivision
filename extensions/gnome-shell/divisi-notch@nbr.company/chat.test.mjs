// Run with: node chat.test.mjs (also run by `cargo test -p divisi-cli --test notch_chat_js`).
import assert from 'node:assert/strict';
import {applyEvents, chatEntry, newChat, parseTail, progressText, submitInput} from './chat.js';

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

console.log('chat.js: all checks passed');
