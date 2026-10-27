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
    if (["a", "z", "s", "f"].includes(key)) return { key, shift: event.shiftKey, action: "key" };
    return null;
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
