// Platform-independent input policy, shared by the browser and regression tests.
export const keys = {
    Backspace: 0xff08, Tab: 0xff09, Enter: 0xff0d, Escape: 0xff1b,
    Delete: 0xffff, Insert: 0xff63, Home: 0xff50, End: 0xff57,
    ArrowLeft: 0xff51, ArrowUp: 0xff52, ArrowRight: 0xff53, ArrowDown: 0xff54,
    PageUp: 0xff55, PageDown: 0xff56,
};
export const modifiers = { Control: 0xffe3, Shift: 0xffe1, Alt: 0xffe9 };

export function shortcut(event, terminal = false) {
    if (!event.metaKey || event.ctrlKey || event.altKey) return null;
    const key = event.key.toLowerCase();
    if (["c", "x", "v"].includes(key)) {
        return { key, shift: terminal || event.shiftKey, action: key === "v" ? "paste" : "copy" };
    }
    if (controlLetters.has(key)) return { key, shift: event.shiftKey, action: "key" };
    return null;
}

// ⌘ shortcuts sent to Linux as Ctrl. ⌘Q/W/N/T/H/M/L/R/Tab stay with the browser and macOS.
const controlLetters = new Set(["a", "b", "d", "e", "f", "g", "i", "k", "o", "p", "s", "u", "y", "z", "/"]);

// The Linux chord for a Mac copy, cut or paste. Terminals use Ctrl+Shift+C/V (Ctrl+C would
// interrupt the running program) and have no cut, so ⌘X copies there.
export function clipboardChord(key, terminal, shift = false) {
    if (terminal) return { key: key === "x" ? "c" : key, ctrl: true, shift: true };
    return { key, ctrl: true, shift };
}

// Mac text-editing keys and their usual Linux equivalents; null for keys sent unchanged.
export function editingChord(event) {
    const { key, metaKey: meta, altKey: alt, shiftKey: shift } = event;
    if (meta && key.startsWith("Arrow")) {
        const vertical = key === "ArrowUp" || key === "ArrowDown";
        const start = key === "ArrowLeft" || key === "ArrowUp";
        return { sym: start ? keys.Home : keys.End, ctrl: vertical, shift };
    }
    if (alt && !meta && (key === "ArrowLeft" || key === "ArrowRight")) return { sym: keys[key], ctrl: true, shift };
    if (alt && !meta && (key === "Backspace" || key === "Delete")) return { sym: keys[key], ctrl: true, shift: false };
    if (meta && key === "Enter") return { sym: keys.Enter, ctrl: true, shift };
    return null;
}

// The keysym for a chord's key. With Shift held a letter must be sent upper case: VNC servers
// such as TigerVNC release Shift to produce a lower-case keysym, turning Ctrl+Shift+V into Ctrl+V.
export function chordKeysym(keysym, shift) {
    return shift && keysym >= 0x61 && keysym <= 0x7a ? keysym - 0x20 : keysym;
}

export function unicodeKeysyms(text) {
    return Array.from(text, character => {
        const point = character.codePointAt(0);
        if (point === 10 || point === 13) return keys.Enter;
        if (point === 9) return keys.Tab;
        return point <= 0xff ? point : 0x01000000 | point;
    });
}

export class CompositionCommit {
    composing = false;
    committed = null;
    start() { this.composing = true; this.committed = null; }
    keydown(event) {
        if (!event.isComposing && event.keyCode !== 229 && !this.composing) this.committed = null;
    }
    end(text) {
        this.composing = false;
        this.committed = text;
        return text || "";
    }
    input(event, value) {
        if (this.composing || event.isComposing) return "";
        if (this.committed !== null && (event.data === this.committed || value === this.committed)) {
            this.committed = null;
            return "";
        }
        this.committed = null;
        return event.data ?? value;
    }
}

// Keep the native IME anchor in the visible desktop, near the user's last click.
export function inputAnchor(x, y, bounds) {
    return {
        x: Math.max(bounds.left + 2, Math.min(x, bounds.right - 4)),
        y: Math.max(bounds.top + 2, Math.min(y, bounds.bottom - 24)),
    };
}

// Separate keyboard and clipboard queues keep background RPCs off the typing path.
export class SerialQueue {
    pending = 0;
    tail = Promise.resolve();
    enqueue(operation) {
        if (this.pending >= 128) return Promise.reject(new Error('操作队列已满，请稍后重试。'));
        this.pending++;
        const result = this.tail.then(operation);
        this.tail = result.catch(() => {}).finally(() => this.pending--);
        return result;
    }
}
