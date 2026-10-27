import assert from 'node:assert/strict';
import { test } from 'node:test';
import { shortcut, unicodeKeysyms, CompositionCommit, keys, inputAnchor, SerialQueue } from '../input.js';

test('Mac copy/paste maps to desktop and terminal modifiers', () => {
    const c = { key: 'c', metaKey: true, ctrlKey: false, altKey: false, shiftKey: false };
    assert.deepEqual(shortcut(c), { key: 'c', shift: false, action: 'copy' });
    assert.deepEqual(shortcut({ ...c, key: 'v' }, true), { key: 'v', shift: true, action: 'paste' });
    assert.equal(shortcut({ ...c, key: ' ' }), null);
    assert.equal(shortcut({ ...c, key: 'l' }), null);
    assert.equal(shortcut({ ...c, metaKey: false, ctrlKey: true }), null);
});
test('Chinese and emoji produce Unicode keysyms, not UTF-16 surrogate key presses', () => {
    assert.deepEqual(unicodeKeysyms('A中🙂\n\t'), [65, 0x01004e2d, 0x0101f642, keys.Enter, keys.Tab]);
});
test('IME provisional text is suppressed and committed text is emitted exactly once', () => {
    const state = new CompositionCommit();
    state.start();
    assert.equal(state.input({ isComposing: true, data: 'zhong' }, 'zhong'), '');
    assert.equal(state.end('中文'), '中文');
    assert.equal(state.input({ isComposing: false, data: '中文' }, '中文'), '');
    state.keydown({ isComposing: false, keyCode: 65 });
    assert.equal(state.input({ isComposing: false, data: 'a' }, 'a'), 'a');
});
test('IME commit without trailing input does not swallow a later identical character', () => {
    const state = new CompositionCommit();
    state.start(); state.end('a');
    state.keydown({ isComposing: false, keyCode: 65 });
    assert.equal(state.input({ isComposing: false, data: 'a' }, 'a'), 'a');
});

test('IME anchor stays beside the click and within the remote desktop viewport', () => {
    const bounds = { left: 0, top: 36, right: 1200, bottom: 800 };
    assert.deepEqual(inputAnchor(340, 250, bounds), { x: 340, y: 250 });
    assert.deepEqual(inputAnchor(1200, 800, bounds), { x: 1196, y: 776 });
    assert.deepEqual(inputAnchor(-10, 0, bounds), { x: 2, y: 38 });
});

test('background clipboard transfer does not delay typing; explicit paste preserves key order', async () => {
    const keyboard = new SerialQueue();
    const clipboard = new SerialQueue();
    const events = [];
    let release;
    const transfer = new Promise(resolve => { release = resolve; });
    const sync = clipboard.enqueue(async () => { await transfer; events.push('sync'); });
    await keyboard.enqueue(() => events.push('typed'));
    assert.deepEqual(events, ['typed']);
    const paste = keyboard.enqueue(() => clipboard.enqueue(() => events.push('paste')));
    const following = keyboard.enqueue(() => events.push('after paste'));
    release();
    await Promise.all([sync, paste, following]);
    assert.deepEqual(events, ['typed', 'sync', 'paste', 'after paste']);
});

test('failed input operation does not prevent subsequent keys', async () => {
    const keyboard = new SerialQueue();
    await assert.rejects(keyboard.enqueue(() => { throw new Error('transfer failed'); }));
    assert.equal(await keyboard.enqueue(() => 'next key'), 'next key');
});
