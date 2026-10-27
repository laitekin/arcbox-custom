import assert from 'node:assert/strict';
import { test } from 'node:test';
import { parseView, stepZoom, remoteSize, zoomLabel, defaultView } from '../view.js';

test('stored view settings are validated', () => {
    assert.deepEqual(parseView(null), defaultView);
    assert.deepEqual(parseView('not json'), defaultView);
    assert.deepEqual(parseView('{"resize":"fixed","zoom":"fit"}'), { resize: 'fixed', zoom: 'fit' });
    assert.deepEqual(parseView('{"resize":"follow","zoom":"fit"}'), { resize: 'follow', zoom: 1 });
    assert.deepEqual(parseView('{"resize":"fixed","zoom":0.33}'), { resize: 'fixed', zoom: 1 });
    assert.deepEqual(parseView('{"resize":"other","zoom":1.5}'), { resize: 'follow', zoom: 1.5 });
});

test('zoom steps from exact and fitted scales and stops at the ends', () => {
    assert.equal(stepZoom(1, 1), 1.1);
    assert.equal(stepZoom(1, -1), 0.9);
    assert.equal(stepZoom(0.7, 1), 0.75);
    assert.equal(stepZoom(0.7, -1), 0.67);
    assert.equal(stepZoom(2, 1), 2);
    assert.equal(stepZoom(0.5, -1), 0.5);
});

test('the machine resolution follows the window divided by zoom, within limits', () => {
    assert.deepEqual(remoteSize(1280, 764, 1), { w: 1280, h: 764 });
    assert.deepEqual(remoteSize(1280, 764, 0.5), { w: 2560, h: 1528 });
    assert.deepEqual(remoteSize(1280, 764, 2), { w: 640, h: 382 });
    assert.deepEqual(remoteSize(100, 50, 1), { w: 320, h: 240 });
    assert.deepEqual(remoteSize(6000, 3000, 0.5), { w: 8192, h: 6000 });
});

test('the label shows the real scale when the picture is fitted', () => {
    assert.equal(zoomLabel({ resize: 'follow', zoom: 1 }, 1), '100%');
    assert.equal(zoomLabel({ resize: 'follow', zoom: 1 }, 0.667), '适应 67%');
    assert.equal(zoomLabel({ resize: 'fixed', zoom: 'fit' }, 0.5), '适应 50%');
    assert.equal(zoomLabel({ resize: 'fixed', zoom: 1.5 }, 1.5), '150%');
});
