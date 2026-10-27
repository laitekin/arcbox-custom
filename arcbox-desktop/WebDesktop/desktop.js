import RFB from './vendor/core/rfb.js';
import { keys, modifiers, shortcut, unicodeKeysyms, CompositionCommit, inputAnchor, SerialQueue } from './input.js';

const $ = id => document.getElementById(id);
const token = location.hash.slice(1);
const input = $('input');
const composition = new CompositionCommit();
let rfb;
let connected = false;
const inputQueue = new SerialQueue();
const clipboardQueue = new SerialQueue();
let syncTimer;
let active = false;
let disposed = false;
let lastError = '';
let statusTimer;
let networkBusy = false;

function status(message, error = false) {
    clearTimeout(statusTimer);
    $('status').hidden = false;
    $('status').textContent = message;
    $('status').classList.toggle('error', error);
    lastError = error ? message : '';
    statusTimer = setTimeout(() => { $('status').hidden = true; }, error ? 7000 : 2200);
}
async function api(path, body, method = 'POST') {
    const response = await fetch('/api/' + path, {
        method, cache: 'no-store', credentials: 'omit',
        headers: { Authorization: 'Bearer ' + token, 'Content-Type': 'application/json' },
        ...(method === 'POST' ? { body: JSON.stringify(body ?? {}) } : {}),
        signal: AbortSignal.timeout(path === 'network' ? 30000 : 18000),
    });
    const result = await response.json();
    if (!response.ok) throw new Error(result.error || '桌面连接失败');
    return result;
}
function enqueue(operation) {
    inputQueue.enqueue(operation).catch(error => status(error.message, true));
}
function enqueueClipboard(operation) {
    clipboardQueue.enqueue(operation).catch(error => status(error.message, true));
}
function chord(keysym, { ctrl = false, shift = false, alt = false } = {}) {
    const held = [];
    for (const [enabled, sym] of [[ctrl, modifiers.Control], [shift, modifiers.Shift], [alt, modifiers.Alt]]) {
        if (enabled) { rfb.sendKey(sym, undefined, true); held.push(sym); }
    }
    rfb.sendKey(keysym);
    for (const sym of held.reverse()) rfb.sendKey(sym, undefined, false);
}
function sendText(text) {
    if (!text) return;
    enqueue(() => { for (const sym of unicodeKeysyms(text)) rfb.sendKey(sym); });
}
async function activate() {
    if (!connected || disposed || !document.hasFocus() || document.hidden) return;
    await api('activate');
    active = connected && !disposed && document.hasFocus() && !document.hidden;
    if (!active) await api('deactivate');
}
function deactivate() {
    active = false;
    // Clear server ownership immediately, including while a clipboard read is in flight.
    api('deactivate').catch(error => { if (!disposed) status(error.message, true); });
}
function focusDesktop(event) {
    if (!connected) return;
    $('settings').open = false;
    if (event && Number.isFinite(event.clientX)) {
        const point = inputAnchor(event.clientX, event.clientY, $('screen').getBoundingClientRect());
        input.style.left = point.x + 'px';
        input.style.top = point.y + 'px';
    }
    input.focus({ preventScroll: true });
    if (!active) enqueueClipboard(activate);
}
function clipboardKey(key, shift = $('terminal').checked) {
    enqueue(() => clipboardQueue.enqueue(async () => {
        await activate();
        if (key === 'v') {
            await api('clipboard/paste');
            chord(key.charCodeAt(0), { ctrl: true, shift });
            status('已粘贴 Mac 剪贴板（文字 / 图片）');
        } else {
            chord(key.charCodeAt(0), { ctrl: true, shift });
            await api('clipboard/copy');
            status('已同步到 Mac 剪贴板');
        }
    }));
}
// An invisible, focusable textarea anchors native IME candidates at the desktop click.
// Do not hide/display:none it: macOS needs a rendered editable target for composition.
$('screen').addEventListener('mousedown', event => { event.preventDefault(); focusDesktop(event); }, true);
$('screen').addEventListener('touchend', event => {
    const touch = event.changedTouches[0];
    if (touch) focusDesktop(touch);
}, true);
input.addEventListener('blur', () => {
    // Some browser/noVNC pointer paths focus the canvas after the mousedown handler.
    queueMicrotask(() => {
        if (connected && document.hasFocus() && $('screen').contains(document.activeElement)) input.focus({ preventScroll: true });
    });
});
input.addEventListener('compositionstart', () => composition.start());
input.addEventListener('compositionend', event => {
    sendText(composition.end(event.data));
    input.value = '';
});
input.addEventListener('input', event => {
    const text = composition.input(event, input.value);
    if (!composition.composing && !event.isComposing) input.value = '';
    sendText(text);
});
input.addEventListener('keydown', event => {
    if (!connected) return;
    composition.keydown(event);
    if (composition.composing || event.isComposing || event.keyCode === 229) return;
    // OS input-source switching remains on the Mac, including Caps Lock and Globe/Fn.
    if (['CapsLock', 'Fn', 'Globe', 'Meta', 'Control', 'Alt', 'Shift'].includes(event.key)) return;
    if (event.ctrlKey && (event.code === 'Space' || event.key === ' ')) return;
    const command = shortcut(event, $('terminal').checked);
    if (command) {
        event.preventDefault();
        if (command.action === 'key') enqueue(() => chord(command.key.charCodeAt(0), { ctrl: true, shift: command.shift }));
        else clipboardKey(command.key, command.shift);
        return;
    }
    let sym = keys[event.key];
    let ctrl = event.ctrlKey;
    let alt = false;
    if (event.metaKey && event.key.startsWith('Arrow')) {
        sym = ['ArrowLeft', 'ArrowUp'].includes(event.key) ? keys.Home : keys.End;
        ctrl = ['ArrowUp', 'ArrowDown'].includes(event.key);
    } else if (event.altKey && ['ArrowLeft', 'ArrowRight'].includes(event.key)) {
        ctrl = true;
    } else if (event.metaKey) {
        return; // Browser/system shortcuts (tabs, address bar, etc.) stay local.
    } else if (event.ctrlKey && event.key.length === 1) {
        sym = event.key.toLowerCase().charCodeAt(0);
    } else if (/^F([1-9]|1[0-2])$/.test(event.key)) {
        sym = 0xffbd + Number(event.key.slice(1));
    }
    if (sym) {
        event.preventDefault();
        enqueue(() => chord(sym, { ctrl, alt, shift: event.shiftKey }));
    }
    // Printable keys and Option-accent sequences arrive through input/composition events.
});
input.addEventListener('paste', event => {
    event.preventDefault();
    clipboardKey('v');
});
async function checkNetwork() {
    if (networkBusy) return;
    networkBusy = true;
    $('network-retry').disabled = true;
    $('network-result').textContent = '正在从虚拟机检测…';
    if (!$('network-dialog').open) $('network-dialog').showModal();
    try {
        const result = await api('network');
        $('network-result').textContent = result.output;
    } catch (error) { $('network-result').textContent = error.message; }
    finally { networkBusy = false; $('network-retry').disabled = false; }
}
$('network').onclick = checkNetwork;
$('network-retry').onclick = checkNetwork;
$('network-close').onclick = () => { $('network-dialog').close(); focusDesktop(); };
$('network-dialog').addEventListener('close', () => focusDesktop());
$('copy').onclick = () => { focusDesktop(); clipboardKey('c'); };
$('paste').onclick = () => { focusDesktop(); clipboardKey('v'); };
$('fullscreen').onclick = () => {
    (document.fullscreenElement ? document.exitFullscreen() : document.documentElement.requestFullscreen())
        .catch(error => status(error.message, true));
};
async function settings() {
    await activate();
    await api('settings', { display: Number($('display').value), clipboard: $('sync').checked });
    status($('sync').checked ? '自动同步已开启，仅在此桌面获得焦点时运行' : '自动同步已暂停；⌘C / ⌘V 仍可使用');
    focusDesktop();
}
$('sync').onchange = () => enqueueClipboard(settings);
$('display').onchange = () => enqueueClipboard(settings);
$('terminal').onchange = () => {
    status($('terminal').checked ? '终端模式：⌘C / ⌘V → Ctrl+Shift+C / V' : '桌面模式：⌘C / ⌘V → Ctrl+C / V');
    focusDesktop();
};
$('disconnect').onclick = () => enqueue(async () => {
    disposed = true;
    clearTimeout(syncTimer);
    rfb?.disconnect();
    await api('close');
    status('已断开。返回 ArcBox 可重新打开。');
});
window.addEventListener('blur', () => {
    deactivate();
});
window.addEventListener('focus', () => enqueueClipboard(activate));
document.addEventListener('visibilitychange', () => {
    if (document.hidden) deactivate();
    else if (document.hasFocus()) enqueueClipboard(activate);
});
async function pollClipboard() {
    if (disposed) return;
    try {
        if (connected && active && document.hasFocus() && !document.hidden && $('sync').checked
            && inputQueue.pending === 0 && clipboardQueue.pending === 0) {
            // Serialize clipboard RPCs without making regular typing wait for them.
            enqueueClipboard(async () => {
                if (!active || !document.hasFocus()) return;
                await api('clipboard/sync');
                if (lastError) status('剪贴板已连接');
            });
        }
    } finally { syncTimer = setTimeout(pollClipboard, 1000); }
}
$('password-form').onsubmit = event => {
    event.preventDefault();
    rfb.sendCredentials({ password: $('password').value });
    $('password').value = '';
    $('password-dialog').close();
};
async function start() {
    if (!/^[a-f0-9]{64}$/i.test(token)) throw new Error('请从 ArcBox 的“Open in Browser”打开桌面。');
    const config = await api('config', undefined, 'GET');
    $('machine').textContent = config.name;
    $('display').value = config.display;
    document.title = config.name + ' · ArcBox';
    rfb = new RFB($('screen'), 'ws://127.0.0.1:' + config.port + '/', { wsProtocols: ['binary', 'arcbox-' + token] });
    rfb.focusOnClick = false;
    rfb.scaleViewport = true;
    rfb.resizeSession = true;
    rfb.addEventListener('connect', () => {
        connected = true;
        $('connection').textContent = '已连接';
        focusDesktop();
        pollClipboard();
    });
    rfb.addEventListener('disconnect', event => {
        connected = false;
        active = false;
        clearTimeout(syncTimer);
        $('connection').textContent = '已断开';
        if (!disposed) status(event.detail.clean ? '连接已关闭，请刷新重连。' : 'VNC 连接失败，请检查机器中的 VNC 服务和转发端口。', true);
    });
    rfb.addEventListener('credentialsrequired', () => {
        $('password-dialog').showModal();
        $('password').focus();
    });
    rfb.addEventListener('securityfailure', event => status(event.detail.reason || 'VNC 认证失败', true));
}
start().catch(error => { $('connection').textContent = '连接失败'; status(error.message, true); });
